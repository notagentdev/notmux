use crate::action_dispatch::ActionDispatcher;
use crate::settings::settings;
use crate::views::overlay_manager::{OverlayManager, OverlayManagerEvent};
use crate::workspace::requests::OverlayRequest;
use crate::workspace::requests::SidebarRequest;
use crate::workspace::state::{LayoutNode, Workspace};
use gpui::*;

use notmux_core::api::ActionRequest;

use super::RootView;

struct MainDiffViewerRequest {
    project_id: String,
    file: Option<String>,
    mode: Option<notmux_core::types::DiffMode>,
    commit_message: Option<String>,
    commits: Option<Vec<crate::git::CommitLogEntry>>,
    commit_index: Option<usize>,
}

impl RootView {
    /// Build an ActionDispatcher for the given project.
    /// Returns Remote variant if the project is a remote project,
    /// otherwise returns Local variant.
    fn dispatcher_for_project(&self, project_id: &str, cx: &Context<Self>) -> ActionDispatcher {
        let backend = Some(self.backend.clone());
        crate::action_dispatch::dispatcher_for_project(
            project_id,
            &self.workspace,
            &backend,
            &self.terminals,
            &self.service_manager,
            &self.remote_manager,
            cx,
        )
        .unwrap_or_else(|| ActionDispatcher::Local {
            workspace: self.workspace.clone(),
            backend: self.backend.clone(),
            terminals: self.terminals.clone(),
            service_manager: self.service_manager.clone(),
        })
    }

    /// Resolve remote connection parameters for a remote project.
    /// Returns (host, port, token, actual_project_id) or None if unavailable.
    fn remote_params(
        &self,
        project_id: &str,
        connection_id: &str,
        cx: &Context<Self>,
    ) -> Option<(String, u16, String, String)> {
        let rm = self.remote_manager.as_ref()?.read(cx);
        let connections = rm.connections();
        let (config, _, _) = connections.iter().find(|(c, _, _)| c.id == connection_id)?;
        let token = config.saved_token.as_ref()?.clone();
        let actual_id = notmux_core::client::strip_prefix(project_id, connection_id);
        Some((config.host.clone(), config.port, token, actual_id))
    }

    /// Build a GitProvider for the given project (local or remote).
    pub(super) fn build_git_provider(
        &self,
        project_id: &str,
        cx: &Context<Self>,
    ) -> Option<std::sync::Arc<dyn crate::views::overlays::diff_viewer::provider::GitProvider>>
    {
        use crate::views::overlays::diff_viewer::provider::{LocalGitProvider, RemoteGitProvider};
        let ws = self.workspace.read(cx);
        let project = ws.project(project_id)?;
        if project.is_remote {
            let conn_id = project.connection_id.as_ref()?;
            let (host, port, token, actual_id) = self.remote_params(project_id, conn_id, cx)?;
            Some(std::sync::Arc::new(RemoteGitProvider::new(
                host, port, token, actual_id,
            )))
        } else {
            Some(std::sync::Arc::new(LocalGitProvider::new(
                project.path.clone(),
            )))
        }
    }

    /// Build a ProjectFs provider for the given project (local or remote).
    fn build_project_fs(
        &self,
        project_id: &str,
        cx: &Context<Self>,
    ) -> Option<std::sync::Arc<dyn notmux_files::project_fs::ProjectFs>> {
        let ws = self.workspace.read(cx);
        let project = ws.project(project_id)?;
        if project.is_remote {
            let conn_id = project.connection_id.as_ref()?;
            let (host, port, token, actual_id) = self.remote_params(project_id, conn_id, cx)?;
            Some(std::sync::Arc::new(
                notmux_files::project_fs::RemoteProjectFs::new(
                    host,
                    port,
                    token,
                    actual_id,
                    project.name.clone(),
                ),
            ))
        } else {
            Some(std::sync::Arc::new(
                notmux_files::project_fs::LocalProjectFs::new(project.path.clone()),
            ))
        }
    }
}

impl RootView {
    /// Handle events from the OverlayManager that require RootView access.
    pub(super) fn handle_overlay_manager_event(
        &mut self,
        _: Entity<OverlayManager>,
        event: &OverlayManagerEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            OverlayManagerEvent::SwitchWorkspace(data) => {
                self.handle_switch_workspace(data.clone(), cx);
            }
            OverlayManagerEvent::WorktreeCreated(new_project_id) => {
                self.spawn_terminals_for_project(new_project_id.clone(), cx);
            }
            OverlayManagerEvent::ShellSelected {
                shell_type,
                project_id,
                terminal_id,
            } => {
                self.switch_terminal_shell(project_id, terminal_id, shell_type.clone(), cx);
            }
            OverlayManagerEvent::AddTerminal { project_id } => {
                let dispatcher = self.dispatcher_for_project(project_id, cx);
                dispatcher.dispatch(
                    ActionRequest::CreateTerminal {
                        project_id: project_id.clone(),
                    },
                    cx,
                );
            }
            OverlayManagerEvent::CreateWorktree {
                project_id,
                project_path,
            } => {
                self.overlay_manager.update(cx, |om, cx| {
                    om.show_worktree_dialog(project_id.clone(), project_path.clone(), cx);
                });
            }
            OverlayManagerEvent::RenameProject {
                project_id,
                project_name,
            } => {
                self.request_broker.update(cx, |broker, cx| {
                    broker.push_sidebar_request(
                        SidebarRequest::RenameProject {
                            project_id: project_id.clone(),
                            project_name: project_name.clone(),
                        },
                        cx,
                    );
                });
            }
            OverlayManagerEvent::RenameDirectory {
                project_id,
                project_path,
            } => {
                self.overlay_manager.update(cx, |om, cx| {
                    om.show_rename_directory_dialog(project_id.clone(), project_path.clone(), cx);
                });
            }
            OverlayManagerEvent::CloseWorktree { project_id } => {
                self.overlay_manager.update(cx, |om, cx| {
                    om.show_close_worktree_dialog(project_id.clone(), cx);
                });
            }
            OverlayManagerEvent::DeleteProject { project_id } => {
                // Collect hook terminal IDs before deleting so we can clean them from the registry
                let hook_tids = self
                    .workspace
                    .read(cx)
                    .hook_terminal_ids_for_project(project_id);
                self.workspace.update(cx, |ws, cx| {
                    ws.delete_project(project_id, &settings(cx).hooks, cx);
                });
                for tid in hook_tids {
                    self.terminals.lock().remove(&tid);
                }
            }
            OverlayManagerEvent::ConfigureHooks { project_id } => {
                self.main_diff_viewer = None;
                self.overlay_manager.update(cx, |om, cx| {
                    om.show_settings_for_project(project_id.clone(), cx);
                });
            }
            OverlayManagerEvent::ReloadServices { project_id } => {
                let dispatcher = self.dispatcher_for_project(project_id, cx);
                dispatcher.dispatch(
                    notmux_core::api::ActionRequest::ReloadServices {
                        project_id: project_id.clone(),
                    },
                    cx,
                );
            }
            OverlayManagerEvent::QuickCreateWorktree { project_id } => {
                self.request_broker.update(cx, |broker, cx| {
                    broker.push_sidebar_request(
                        crate::workspace::requests::SidebarRequest::QuickCreateWorktree {
                            project_id: project_id.clone(),
                        },
                        cx,
                    );
                });
            }
            OverlayManagerEvent::ProjectColorChanged { project_id, color } => {
                self.sidebar.update(cx, |sidebar, cx| {
                    sidebar.sync_remote_color(project_id, *color, cx);
                });
            }
            OverlayManagerEvent::FocusParent { project_id } => {
                let parent_id = self
                    .workspace
                    .read(cx)
                    .project(project_id)
                    .and_then(|p| p.worktree_info.as_ref())
                    .map(|wt| wt.parent_project_id.clone());

                if let Some(parent_id) = parent_id {
                    self.workspace.update(cx, |ws, cx| {
                        ws.set_focused_project(Some(parent_id.clone()), cx);
                    });
                    self.follow_git_panel_to_project(&parent_id, cx);
                }
            }
            OverlayManagerEvent::FocusProject(project_id) => {
                self.workspace.update(cx, |ws, cx| {
                    ws.set_focused_project(Some(project_id.clone()), cx);
                });
                self.follow_git_panel_to_project(project_id, cx);
            }
            OverlayManagerEvent::ToggleProjectVisibility(project_id) => {
                self.workspace.update(cx, |ws, cx| {
                    ws.toggle_project_overview_visibility(project_id, cx);
                });
            }
            OverlayManagerEvent::RemoteReconnect { connection_id } => {
                if let Some(ref rm) = self.remote_manager {
                    rm.update(cx, |rm, cx| {
                        rm.reconnect(connection_id, cx);
                    });
                }
            }
            OverlayManagerEvent::RemotePair {
                connection_id,
                connection_name,
            } => {
                self.overlay_manager.update(cx, |om, cx| {
                    om.show_remote_pair_dialog(connection_id.clone(), connection_name.clone(), cx);
                });
            }
            OverlayManagerEvent::RemotePaired {
                connection_id,
                code,
            } => {
                if let Some(ref rm) = self.remote_manager {
                    rm.update(cx, |rm, cx| {
                        rm.pair(connection_id, code, cx);
                    });
                }
            }
            OverlayManagerEvent::RemoteRemoveConnection { connection_id } => {
                if let Some(ref rm) = self.remote_manager {
                    rm.update(cx, |rm, cx| {
                        rm.remove_connection(connection_id, cx);
                    });
                }
            }
            OverlayManagerEvent::TerminalCopy { terminal_id } => {
                let terminals = self.terminals.lock();
                if let Some(terminal) = terminals.get(terminal_id)
                    && let Some(text) = terminal.get_selected_text()
                {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            }
            OverlayManagerEvent::TerminalPaste { terminal_id } => {
                let text = cx
                    .read_from_clipboard()
                    .and_then(|item| item.text().map(|t| t.to_string()));
                if let Some(text) = text {
                    let terminals = self.terminals.lock();
                    if let Some(terminal) = terminals.get(terminal_id) {
                        terminal.send_paste(&text);
                    }
                }
            }
            OverlayManagerEvent::TerminalClear { terminal_id } => {
                let terminals = self.terminals.lock();
                if let Some(terminal) = terminals.get(terminal_id) {
                    terminal.clear();
                }
            }
            OverlayManagerEvent::TerminalSelectAll { terminal_id } => {
                let terminals = self.terminals.lock();
                if let Some(terminal) = terminals.get(terminal_id) {
                    terminal.select_all();
                }
                cx.notify();
            }
            OverlayManagerEvent::TerminalSplit {
                project_id,
                layout_path,
                direction,
            } => {
                let dispatcher = self.dispatcher_for_project(project_id, cx);
                dispatcher.dispatch(
                    ActionRequest::SplitTerminal {
                        project_id: project_id.clone(),
                        path: layout_path.clone(),
                        direction: *direction,
                    },
                    cx,
                );
            }
            OverlayManagerEvent::TerminalClose {
                project_id,
                terminal_id,
            } => {
                let dispatcher = self.dispatcher_for_project(project_id, cx);
                dispatcher.dispatch(
                    ActionRequest::CloseTerminal {
                        project_id: project_id.clone(),
                        terminal_id: terminal_id.clone(),
                    },
                    cx,
                );
            }
            OverlayManagerEvent::TabClose {
                project_id,
                layout_path,
                tab_index,
            } => {
                let terminal_ids =
                    collect_tab_terminal_ids(&self.workspace, project_id, layout_path, cx);
                if let Some(tid) = terminal_ids.get(*tab_index).cloned() {
                    let dispatcher = self.dispatcher_for_project(project_id, cx);
                    dispatcher.dispatch(
                        ActionRequest::CloseTerminal {
                            project_id: project_id.clone(),
                            terminal_id: tid,
                        },
                        cx,
                    );
                }
            }
            OverlayManagerEvent::TabCloseOthers {
                project_id,
                layout_path,
                tab_index,
            } => {
                let terminal_ids =
                    collect_tab_terminal_ids(&self.workspace, project_id, layout_path, cx);
                let to_close: Vec<String> = terminal_ids
                    .into_iter()
                    .enumerate()
                    .filter(|(i, _)| *i != *tab_index)
                    .map(|(_, id)| id)
                    .collect();
                if !to_close.is_empty() {
                    let dispatcher = self.dispatcher_for_project(project_id, cx);
                    dispatcher.dispatch(
                        ActionRequest::CloseTerminals {
                            project_id: project_id.clone(),
                            terminal_ids: to_close,
                        },
                        cx,
                    );
                }
            }
            OverlayManagerEvent::TabCloseToRight {
                project_id,
                layout_path,
                tab_index,
            } => {
                let terminal_ids =
                    collect_tab_terminal_ids(&self.workspace, project_id, layout_path, cx);
                let to_close: Vec<String> = terminal_ids.into_iter().skip(tab_index + 1).collect();
                if !to_close.is_empty() {
                    let dispatcher = self.dispatcher_for_project(project_id, cx);
                    dispatcher.dispatch(
                        ActionRequest::CloseTerminals {
                            project_id: project_id.clone(),
                            terminal_ids: to_close,
                        },
                        cx,
                    );
                }
            }
            OverlayManagerEvent::RemoteConnected { config } => {
                if let Some(ref rm) = self.remote_manager {
                    let config_clone = config.clone();
                    let result = rm.update(cx, |rm, cx| rm.add_connection(config.clone(), cx));
                    if let Err(msg) = result {
                        crate::views::panels::toast::ToastManager::warning(msg, cx);
                        return;
                    }
                    // Save connection config (with token) to settings (atomic update)
                    let _ = crate::workspace::settings::update_remote_connections(|conns| {
                        if !conns.iter().any(|c| c.id == config_clone.id) {
                            conns.push(config_clone);
                        }
                    });
                }
            }
            OverlayManagerEvent::GitFileStage {
                project_id,
                file_path,
            } => {
                let dispatcher = self.dispatcher_for_project(project_id, cx);
                dispatcher.dispatch(
                    notmux_core::api::ActionRequest::GitStageFile {
                        project_id: project_id.clone(),
                        file_path: file_path.clone(),
                    },
                    cx,
                );
                self.refresh_git_panel(project_id, cx);
            }
            OverlayManagerEvent::GitFileUnstage {
                project_id,
                file_path,
            } => {
                let dispatcher = self.dispatcher_for_project(project_id, cx);
                dispatcher.dispatch(
                    notmux_core::api::ActionRequest::GitUnstageFile {
                        project_id: project_id.clone(),
                        file_path: file_path.clone(),
                    },
                    cx,
                );
                self.refresh_git_panel(project_id, cx);
            }
            OverlayManagerEvent::GitFileDiscard {
                project_id,
                file_path,
                is_untracked,
            } => {
                let dispatcher = self.dispatcher_for_project(project_id, cx);
                dispatcher.dispatch(
                    notmux_core::api::ActionRequest::GitDiscardFile {
                        project_id: project_id.clone(),
                        file_path: file_path.clone(),
                        is_untracked: *is_untracked,
                    },
                    cx,
                );
                self.refresh_git_panel(project_id, cx);
            }
            OverlayManagerEvent::GitFileAddToGitignore {
                project_id,
                file_path,
            } => {
                self.append_to_gitignore(project_id, file_path, cx);
                self.refresh_git_panel(project_id, cx);
            }
            OverlayManagerEvent::GitStageAll { project_id } => {
                self.with_git_header(project_id, cx, |gh, cx| gh.handle_stage_all(cx));
            }
            OverlayManagerEvent::GitUnstageAll { project_id } => {
                self.with_git_header(project_id, cx, |gh, cx| gh.handle_unstage_all(cx));
            }
            OverlayManagerEvent::GitStashAll { project_id } => {
                self.with_git_header(project_id, cx, |gh, cx| gh.handle_stash_all(cx));
            }
            OverlayManagerEvent::GitStashPop { project_id } => {
                self.with_git_header(project_id, cx, |gh, cx| gh.handle_stash_pop(cx));
            }
            OverlayManagerEvent::GitDiscardAllTracked { project_id } => {
                self.with_git_header(project_id, cx, |gh, cx| gh.handle_discard_all_tracked(cx));
            }
            OverlayManagerEvent::GitStashRefresh { project_id } => {
                self.refresh_git_panel(project_id, cx);
            }
            OverlayManagerEvent::ExplorerNewFile { parent } => {
                let parent = parent.clone();
                self.sidebar.update(cx, |sb, cx| {
                    if let Some(fe) = sb.file_explorer_for_path(&parent, cx) {
                        fe.update(cx, |fe, cx| fe.start_new_file(parent, cx));
                    }
                });
            }
            OverlayManagerEvent::ExplorerNewFolder { parent } => {
                let parent = parent.clone();
                self.sidebar.update(cx, |sb, cx| {
                    if let Some(fe) = sb.file_explorer_for_path(&parent, cx) {
                        fe.update(cx, |fe, cx| fe.start_new_folder(parent, cx));
                    }
                });
            }
            OverlayManagerEvent::ExplorerRename { target } => {
                let target = target.clone();
                self.sidebar.update(cx, |sb, cx| {
                    if let Some(fe) = sb.file_explorer_for_path(&target, cx) {
                        fe.update(cx, |fe, cx| fe.start_rename(target, cx));
                    }
                });
            }
            OverlayManagerEvent::ExplorerDelete { path, is_dir } => {
                let path = path.clone();
                let is_dir = *is_dir;
                let sidebar = self.sidebar.clone();
                cx.spawn(async move |_this, cx| {
                    let p = path.clone();
                    let result =
                        smol::unblock(move || notmux_files::fs_ops::delete(&p, is_dir)).await;
                    cx.update(|cx| {
                        if let Err(msg) = result {
                            log::warn!("explorer delete failed: {msg}");
                        }
                        sidebar.update(cx, |sb, cx| {
                            sb.patch_explorers_for_path(&path, cx);
                        });
                    });
                })
                .detach();
            }
            OverlayManagerEvent::ExplorerReveal { path } => {
                let path = path.clone();
                cx.spawn(async move |_this, _cx| {
                    let _ =
                        smol::unblock(move || notmux_files::fs_ops::reveal_in_file_manager(&path))
                            .await;
                })
                .detach();
            }
            OverlayManagerEvent::ExplorerContextMenuClosed => {
                self.sidebar.update(cx, |sb, cx| {
                    sb.clear_all_explorer_context_menu_targets(cx);
                });
            }
            OverlayManagerEvent::ExplorerAddToGitignore { path } => {
                let (project_id, rel) = {
                    let sb = self.sidebar.read(cx);
                    let Some(fe) = sb.file_explorer_for_path(path, cx) else {
                        return;
                    };
                    let fe = fe.read(cx);
                    let Ok(rel) = path.strip_prefix(fe.project_path()) else {
                        return;
                    };
                    (
                        fe.project_id().to_string(),
                        rel.to_string_lossy().replace('\\', "/"),
                    )
                };
                self.append_to_gitignore(&project_id, &rel, cx);
                self.refresh_git_panel(&project_id, cx);
            }
            OverlayManagerEvent::ExplorerPaste { target_dir } => {
                let target_dir = target_dir.clone();
                let Some(cb) = cx
                    .try_global::<notmux_files::clipboard::ExplorerClipboard>()
                    .cloned()
                else {
                    return;
                };
                let (Some(src), Some(op)) = (cb.path.clone(), cb.op) else {
                    return;
                };
                let sidebar = self.sidebar.clone();
                let src_for_paths = src.clone();
                let target_for_paths = target_dir.clone();
                let file_name = match src.file_name() {
                    Some(n) => n.to_os_string(),
                    None => return,
                };
                let dst = target_dir.join(&file_name);
                cx.spawn(async move |_this, cx| {
                    let src_ = src.clone();
                    let dst_ = dst.clone();
                    let result = smol::unblock(move || match op {
                        notmux_files::clipboard::ClipboardOp::Cut => {
                            notmux_files::fs_ops::move_to(&src_, &dst_)
                        }
                        notmux_files::clipboard::ClipboardOp::Copy => {
                            notmux_files::fs_ops::copy(&src_, &dst_)
                        }
                    })
                    .await;
                    cx.update(|cx| {
                        if let Err(msg) = result {
                            log::warn!("explorer paste failed: {msg}");
                        }
                        // Clear clipboard on Cut; keep on Copy.
                        if matches!(op, notmux_files::clipboard::ClipboardOp::Cut) {
                            cx.global_mut::<notmux_files::clipboard::ExplorerClipboard>()
                                .clear();
                        }
                        sidebar.update(cx, |sb, cx| {
                            sb.patch_explorers_for_path(&src_for_paths, cx);
                            sb.patch_explorers_for_path(&target_for_paths, cx);
                        });
                    });
                })
                .detach();
            }
        }
    }

    /// Run a closure against the GitHeader of the project, if it exists.
    fn with_git_header<F>(&self, project_id: &str, cx: &mut Context<Self>, f: F)
    where
        F: FnOnce(
            &mut notmux_views_git::git_header::GitHeader,
            &mut Context<notmux_views_git::git_header::GitHeader>,
        ),
    {
        if let Some(col) = self.project_columns.get(project_id).cloned() {
            let gh = col.read(cx).git_header();
            gh.update(cx, |gh, cx| f(gh, cx));
        }
    }

    /// Refresh the git panel's working tree state for the given project.
    fn refresh_git_panel(&self, project_id: &str, cx: &mut Context<Self>) {
        if let Some(col) = self.project_columns.get(project_id).cloned() {
            let gh = col.read(cx).git_header();
            gh.update(cx, |gh, cx| gh.refresh_working_tree_status(cx));
        }
    }

    /// When the focused project changes, keep the git panel in sync:
    /// if the panel is open and showing a different project, swap its
    /// content (close old commit log, open new one) and refresh the working
    /// tree. Always refreshes the new project's status so the header badge
    /// is up to date.
    pub(super) fn follow_git_panel_to_project(&mut self, project_id: &str, cx: &mut Context<Self>) {
        // Ensure the ProjectColumn (+ its GitHeader) exists before we try
        // to bind the panel to it. Without this, switching into a project
        // that was previously hidden (individual-mode focus change) silently
        // no-ops the open_commit_log call because sync_project_columns
        // hasn't run for the frame yet — `render_git_panel` then sees
        // `commit_log_visible = false` and renders nothing.
        self.sync_project_columns(cx);

        self.refresh_git_panel(project_id, cx);

        let panel_open = self.git_panel_ctrl.is_open();
        let same = self.git_panel_project_id.as_deref() == Some(project_id);
        if !panel_open || same {
            return;
        }

        if let Some(old_pid) = self.git_panel_project_id.take()
            && let Some(old_col) = self.project_columns.get(&old_pid).cloned()
        {
            let gh = old_col.read(cx).git_header();
            gh.update(cx, |gh, cx| gh.hide_commit_log(cx));
        }

        self.git_panel_project_id = Some(project_id.to_string());

        if let Some(col) = self.project_columns.get(project_id).cloned() {
            let gh = col.read(cx).git_header();
            gh.update(cx, |gh, cx| gh.open_commit_log(cx));
        }
        cx.notify();

        // Belt-and-braces: the GitHeader's async load (commit graph +
        // working tree + branches) can complete on a later frame. The
        // per-project observer set up in `sync_project_columns` catches
        // that, but in practice there's a timing window where the first
        // render after a project switch still sees stale state. Schedule
        // a few extra notifies so `render_git_panel` re-runs once data
        // lands. Cheap, idempotent, no state drift.
        cx.spawn(async move |this, cx| {
            for delay_ms in [30u64, 120, 400, 900] {
                smol::Timer::after(std::time::Duration::from_millis(delay_ms)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Append a path to the project's .gitignore file (create if missing).
    fn append_to_gitignore(&self, project_id: &str, file_path: &str, cx: &mut Context<Self>) {
        let ws = self.workspace.read(cx);
        let Some(project) = ws.project(project_id) else {
            return;
        };
        let gitignore = std::path::Path::new(&project.path).join(".gitignore");

        let existing = std::fs::read_to_string(&gitignore).unwrap_or_default();
        let has_trailing_newline = existing.ends_with('\n') || existing.is_empty();
        let mut content = existing;
        if !has_trailing_newline {
            content.push('\n');
        }
        content.push_str(file_path);
        content.push('\n');

        if let Err(e) = std::fs::write(&gitignore, content) {
            crate::views::panels::toast::ToastManager::error(
                format!("Failed to update .gitignore: {}", e),
                cx,
            );
        }
    }

    /// Handle workspace switch from session manager.
    pub(super) fn handle_switch_workspace(
        &mut self,
        data: crate::workspace::state::WorkspaceData,
        cx: &mut Context<Self>,
    ) {
        // Kill all existing terminals
        {
            let terminals = self.terminals.lock();
            for terminal in terminals.values() {
                self.backend.kill(&terminal.terminal_id);
            }
        }
        self.terminals.lock().clear();

        // Clear project columns (will be recreated)
        self.project_columns.clear();

        // Update workspace with new data
        self.workspace.update(cx, |ws, cx| {
            ws.replace_data(data, cx);
        });

        // Sync project columns for new data
        self.sync_project_columns(cx);

        cx.notify();
    }

    /// Process pending overlay requests from workspace state.
    ///
    /// Drains the overlay request queue and dispatches each request to the
    /// OverlayManager. Requests for already-open overlays are silently dropped.
    pub(super) fn process_pending_requests(&mut self, cx: &mut Context<Self>) {
        let requests: Vec<_> = self
            .request_broker
            .update(cx, |broker, _cx| broker.drain_overlay_requests());

        for request in requests {
            match request {
                OverlayRequest::ContextMenu {
                    project_id,
                    position,
                } => {
                    if !self.overlay_manager.read(cx).has_context_menu() {
                        self.overlay_manager.update(cx, |om, cx| {
                            om.show_context_menu(
                                crate::workspace::requests::ContextMenuRequest {
                                    project_id,
                                    position,
                                },
                                cx,
                            );
                        });
                    }
                }
                OverlayRequest::FolderContextMenu {
                    folder_id,
                    folder_name,
                    position,
                } => {
                    if !self.overlay_manager.read(cx).has_folder_context_menu() {
                        self.overlay_manager.update(cx, |om, cx| {
                            om.show_folder_context_menu(
                                crate::workspace::requests::FolderContextMenuRequest {
                                    folder_id,
                                    folder_name,
                                    position,
                                },
                                cx,
                            );
                        });
                    }
                }
                OverlayRequest::ShellSelector {
                    project_id,
                    terminal_id,
                    current_shell,
                } => {
                    self.overlay_manager.update(cx, |om, cx| {
                        om.show_shell_selector(current_shell, project_id, terminal_id, cx);
                    });
                }
                OverlayRequest::AddProjectDialog => {
                    let rm = self.remote_manager.clone();
                    self.overlay_manager.update(cx, |om, cx| {
                        om.toggle_add_project_dialog(rm, cx);
                    });
                }
                OverlayRequest::CommandPalette => {
                    self.overlay_manager.update(cx, |om, cx| {
                        om.toggle_command_palette(cx);
                    });
                }
                OverlayRequest::DiffViewer {
                    project_id,
                    file,
                    mode,
                    commit_message,
                    commits,
                    commit_index,
                } => {
                    if file.is_some() && mode.is_none() {
                        self.show_main_diff_viewer(
                            MainDiffViewerRequest {
                                project_id,
                                file,
                                mode,
                                commit_message,
                                commits,
                                commit_index,
                            },
                            cx,
                        );
                    } else if let Some(provider) = self.build_git_provider(&project_id, cx) {
                        self.overlay_manager.update(cx, |om, cx| {
                            om.show_diff_viewer(
                                provider,
                                file,
                                mode,
                                commit_message,
                                commits,
                                commit_index,
                                cx,
                            );
                        });
                    }
                }
                OverlayRequest::MainDiffViewer {
                    project_id,
                    file,
                    mode,
                    commit_message,
                    commits,
                    commit_index,
                } => {
                    self.show_main_diff_viewer(
                        MainDiffViewerRequest {
                            project_id,
                            file,
                            mode,
                            commit_message,
                            commits,
                            commit_index,
                        },
                        cx,
                    );
                }
                OverlayRequest::MainFileViewer { project_id, file } => {
                    self.open_editor_tab(project_id, file, cx);
                }
                OverlayRequest::RemoteConnect => {
                    if let Some(ref rm) = self.remote_manager {
                        let rm = rm.clone();
                        self.overlay_manager.update(cx, |om, cx| {
                            om.toggle_remote_connect(rm, cx);
                        });
                    }
                }
                OverlayRequest::RemoteConnectionContextMenu {
                    connection_id,
                    connection_name,
                    is_pairing,
                    position,
                } => {
                    if !self.overlay_manager.read(cx).has_remote_context_menu() {
                        self.overlay_manager.update(cx, |om, cx| {
                            om.show_remote_context_menu(
                                connection_id,
                                connection_name,
                                is_pairing,
                                position,
                                cx,
                            );
                        });
                    }
                }
                OverlayRequest::TerminalContextMenu {
                    terminal_id,
                    project_id,
                    layout_path,
                    position,
                    has_selection,
                    link_url,
                } => {
                    self.overlay_manager.update(cx, |om, cx| {
                        om.show_terminal_context_menu(
                            terminal_id,
                            project_id,
                            layout_path,
                            position,
                            has_selection,
                            link_url,
                            cx,
                        );
                    });
                }
                OverlayRequest::TabContextMenu {
                    tab_index,
                    num_tabs,
                    project_id,
                    layout_path,
                    position,
                } => {
                    self.overlay_manager.update(cx, |om, cx| {
                        om.show_tab_context_menu(
                            tab_index,
                            num_tabs,
                            project_id,
                            layout_path,
                            position,
                            cx,
                        );
                    });
                }
                OverlayRequest::ShowServiceLog {
                    project_id,
                    service_name,
                } => {
                    self.handle_show_service_log(project_id, service_name, cx);
                }
                OverlayRequest::RunProjectCommand {
                    project_id,
                    name,
                    command,
                    cwd,
                } => {
                    self.run_project_command(&project_id, &name, &command, &cwd, cx);
                }
                OverlayRequest::ShowHookTerminal {
                    project_id,
                    terminal_id,
                } => {
                    if let Some(col) = self.project_columns.get(&project_id).cloned() {
                        col.update(cx, |col, cx| {
                            col.show_hook_terminal(&terminal_id, cx);
                        });
                    }
                }
                OverlayRequest::FileSearch { project_id } => {
                    if let Some(fs) = self.build_project_fs(&project_id, cx) {
                        self.overlay_manager.update(cx, |om, cx| {
                            om.toggle_file_search(fs, cx);
                        });
                    }
                }
                OverlayRequest::ContentSearch { project_id } => {
                    if let Some(fs) = self.build_project_fs(&project_id, cx) {
                        self.overlay_manager.update(cx, |om, cx| {
                            om.toggle_content_search(fs, cx);
                        });
                    }
                }
                OverlayRequest::FileBrowser { project_id } => {
                    if let Some(fs) = self.build_project_fs(&project_id, cx) {
                        self.overlay_manager.update(cx, |om, cx| {
                            om.show_file_browser(fs, cx);
                        });
                    }
                }
                OverlayRequest::ColorPicker {
                    project_id,
                    position,
                } => {
                    self.overlay_manager.update(cx, |om, cx| {
                        om.show_color_picker(
                            notmux_views_sidebar::ColorPickerTarget::Project { project_id },
                            position,
                            cx,
                        );
                    });
                }
                OverlayRequest::FolderColorPicker {
                    folder_id,
                    position,
                } => {
                    self.overlay_manager.update(cx, |om, cx| {
                        om.show_color_picker(
                            notmux_views_sidebar::ColorPickerTarget::Folder { folder_id },
                            position,
                            cx,
                        );
                    });
                }
                OverlayRequest::WorktreeList {
                    project_id,
                    position,
                } => {
                    self.overlay_manager.update(cx, |om, cx| {
                        om.show_worktree_list(project_id, position, cx);
                    });
                }
                OverlayRequest::ToggleGitPanel { project_id } => {
                    self.toggle_git_panel(&project_id, cx);
                }
                OverlayRequest::ExplorerContextMenu {
                    kind,
                    path,
                    parent_dir,
                    has_clipboard,
                    position,
                } => {
                    if !self.overlay_manager.read(cx).has_explorer_context_menu() {
                        self.overlay_manager.update(cx, |om, cx| {
                            om.show_explorer_context_menu(
                                kind,
                                path,
                                parent_dir,
                                has_clipboard,
                                position,
                                cx,
                            );
                        });
                    }
                }
                OverlayRequest::GitFileContextMenu {
                    project_id,
                    file_path,
                    is_staged,
                    is_untracked,
                    is_conflict,
                    position,
                } => {
                    if !self.overlay_manager.read(cx).has_git_file_context_menu() {
                        self.overlay_manager.update(cx, |om, cx| {
                            om.show_git_file_context_menu(
                                project_id,
                                file_path,
                                is_staged,
                                is_untracked,
                                is_conflict,
                                position,
                                cx,
                            );
                        });
                    }
                }
                OverlayRequest::GitOverflowMenu {
                    project_id,
                    position,
                    has_staged,
                    has_unstaged,
                    has_tracked,
                    has_untracked,
                    has_stash,
                } => {
                    if !self.overlay_manager.read(cx).has_git_overflow_menu() {
                        self.overlay_manager.update(cx, |om, cx| {
                            om.show_git_overflow_menu(
                                project_id,
                                position,
                                has_staged,
                                has_unstaged,
                                has_tracked,
                                has_untracked,
                                has_stash,
                                cx,
                            );
                        });
                    }
                }
                OverlayRequest::GitStashList {
                    project_id,
                    position,
                } => {
                    if self.overlay_manager.read(cx).has_git_stash_list() {
                        continue;
                    }
                    let Some(col) = self.project_columns.get(&project_id).cloned() else {
                        continue;
                    };
                    let provider = col.read(cx).git_header().read(cx).git_provider();
                    self.overlay_manager.update(cx, |om, cx| {
                        om.show_git_stash_list(project_id, provider, position, cx);
                    });
                }
            }
        }
    }

    fn show_main_diff_viewer(&mut self, request: MainDiffViewerRequest, cx: &mut Context<Self>) {
        let Some(provider) = self.build_git_provider(&request.project_id, cx) else {
            return;
        };

        self.overlay_manager
            .update(cx, |om, cx| om.close_settings_panel(cx));

        self.main_file_viewer = None;
        let viewer = cx.new(|cx| {
            notmux_views_git::diff_viewer::DiffViewer::new(
                provider,
                request.file,
                request.mode,
                request.commit_message,
                request.commits,
                request.commit_index,
                cx,
            )
        });

        cx.subscribe(
            &viewer,
            |this, _, event: &notmux_views_git::diff_viewer::DiffViewerEvent, cx| {
                if matches!(event, notmux_views_git::diff_viewer::DiffViewerEvent::Close) {
                    this.main_diff_viewer = None;
                    cx.notify();
                }
            },
        )
        .detach();

        self.main_diff_viewer = Some(viewer);
        cx.notify();
    }

    /// Open a file as a draggable editor tab in the middle panel: inserts a
    /// `LayoutNode::Editor` next to the focused pane of the project (or its first
    /// visible pane), so it drags/splits exactly like a terminal tab.
    pub(super) fn open_editor_tab(
        &mut self,
        project_id: String,
        file: String,
        cx: &mut Context<Self>,
    ) {
        let path = {
            let ws = self.workspace.read(cx);
            ws.focus_manager
                .focused_terminal_state()
                .filter(|f| f.project_id == project_id)
                .map(|f| f.layout_path)
                .or_else(|| {
                    ws.project(&project_id)
                        .and_then(|p| p.layout.as_ref())
                        .map(|l| l.find_visible_terminal_path())
                })
        };
        let Some(path) = path else { return };
        self.workspace.update(cx, |ws, cx| {
            ws.add_editor(&project_id, &path, &file, cx);
        });
    }

    #[allow(dead_code)]
    fn show_main_file_viewer(&mut self, project_id: String, file: String, cx: &mut Context<Self>) {
        let Some(fs) = self.build_project_fs(&project_id, cx) else {
            return;
        };

        self.overlay_manager
            .update(cx, |om, cx| om.close_settings_panel(cx));

        let settings = crate::settings::settings_entity(cx).read(cx).settings.clone();
        let font_size = settings.file_font_size;
        let monochrome_icons = settings.monochrome_icons;
        let theme_colors = crate::theme::theme(cx);
        let is_dark = theme_colors.is_dark();

        let viewer = cx.new(|cx| {
            notmux_files::file_viewer::FileViewer::new_embedded(
                std::path::PathBuf::from(file),
                fs,
                font_size,
                is_dark,
                theme_colors,
                monochrome_icons,
                cx,
            )
        });

        cx.subscribe(
            &viewer,
            |this, _, event: &notmux_files::file_viewer::FileViewerEvent, cx| {
                if matches!(event, notmux_files::file_viewer::FileViewerEvent::Close) {
                    this.main_file_viewer = None;
                    cx.notify();
                }
            },
        )
        .detach();

        self.main_diff_viewer = None;
        self.main_file_viewer = Some(viewer);
        cx.notify();
    }

    /// Runs a notmux.yaml custom command: creates a new terminal in the
    /// project, names it after the command, runs the command, and focuses it.
    fn run_project_command(
        &mut self,
        project_id: &str,
        name: &str,
        command: &str,
        cwd: &str,
        cx: &mut Context<Self>,
    ) {
        use crate::workspace::actions::execute::{ActionResult, execute_action};
        use notmux_core::api::ActionRequest;

        let backend = self.backend.clone();
        let terminals = self.terminals.clone();
        let pid = project_id.to_string();

        // A cwd other than the project root becomes a `cd` prefix — the new
        // terminal always starts at the project's working directory.
        let full_command = if cwd.is_empty() || cwd == "." {
            command.to_string()
        } else {
            // Double quotes work across POSIX shells, cmd, and PowerShell.
            format!("cd \"{}\" && {}", cwd.replace('"', "\\\""), command)
        };

        let new_terminal_id = self.workspace.update(cx, |ws, cx| {
            let result = execute_action(
                ActionRequest::CreateTerminal { project_id: pid.clone() },
                ws,
                &*backend,
                &terminals,
                cx,
            );
            match result {
                ActionResult::Ok(Some(payload)) => payload
                    .get("terminal_ids")
                    .and_then(|ids| ids.as_array())
                    .and_then(|ids| ids.first())
                    .and_then(|id| id.as_str())
                    .map(|s| s.to_string()),
                ActionResult::Ok(None) => None,
                ActionResult::Err(e) => {
                    log::warn!("Custom command '{}' failed to create terminal: {}", name, e);
                    None
                }
            }
        });

        let Some(terminal_id) = new_terminal_id else {
            return;
        };

        self.workspace.update(cx, |ws, cx| {
            for action in [
                ActionRequest::RenameTerminal {
                    project_id: pid.clone(),
                    terminal_id: terminal_id.clone(),
                    name: name.to_string(),
                },
                ActionRequest::RunCommand {
                    terminal_id: terminal_id.clone(),
                    command: full_command.clone(),
                },
                ActionRequest::FocusTerminal {
                    project_id: pid.clone(),
                    terminal_id: terminal_id.clone(),
                },
            ] {
                if let ActionResult::Err(e) =
                    execute_action(action, ws, &*backend, &terminals, cx)
                {
                    log::warn!("Custom command '{}' step failed: {}", name, e);
                }
            }
        });
        cx.notify();
    }

    /// Handle a ShowServiceLog request: delegate to the correct ProjectColumn.
    fn handle_show_service_log(
        &mut self,
        project_id: String,
        service_name: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(col) = self.project_columns.get(&project_id).cloned() {
            col.update(cx, |col, cx| {
                col.show_service(&service_name, cx);
            });
        }
    }
}

/// Collect terminal IDs from children of a Tabs node at the given layout path.
///
/// Each child subtree is traversed with `collect_terminal_ids()`, so nested
/// splits/tabs within a tab are handled correctly. Returns one entry per child.
fn collect_tab_terminal_ids(
    workspace: &Entity<Workspace>,
    project_id: &str,
    layout_path: &[usize],
    cx: &Context<RootView>,
) -> Vec<String> {
    let ws = workspace.read(cx);
    let Some(project) = ws.project(project_id) else {
        return Vec::new();
    };
    let Some(ref layout) = project.layout else {
        return Vec::new();
    };
    let Some(node) = layout.get_at_path(layout_path) else {
        return Vec::new();
    };
    match node {
        LayoutNode::Tabs { children, .. } => {
            children
                .iter()
                .filter_map(|child| {
                    // For simple Terminal children, get the ID directly.
                    // For nested structures, get the first terminal ID.
                    child.collect_terminal_ids().into_iter().next()
                })
                .collect()
        }
        LayoutNode::Terminal { terminal_id, .. } => terminal_id.iter().cloned().collect(),
        _ => Vec::new(),
    }
}
