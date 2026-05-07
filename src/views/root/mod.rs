mod git_panel;
mod handlers;
mod pane_switcher;
mod render;
mod sidebar;
mod terminal_actions;

use crate::git::watcher::GitStatusWatcher;
use crate::remote_client::manager::RemoteConnectionManager;
use crate::services::manager::ServiceManager;
use crate::settings::settings;
use crate::terminal::backend::{LocalBackend, TerminalBackend};
use crate::terminal::pty_manager::PtyManager;
use crate::views::chrome::title_bar::TitleBar;
use crate::views::layout::split_pane::{ActiveDrag, new_active_drag};
use crate::views::overlay_manager::OverlayManager;
use crate::views::panels::project_column::ProjectColumn;
use crate::views::panels::sidebar::Sidebar;
use crate::views::panels::status_bar::StatusBar;
use crate::views::panels::toast::ToastOverlay;
use crate::views::sidebar_controller::SidebarController;
use crate::workspace::request_broker::RequestBroker;
use crate::workspace::state::Workspace;
use gpui::*;
use parking_lot::Mutex;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

/// Shared terminals registry for PTY event routing (re-exported from vryn-terminal)
pub use vryn_terminal::TerminalsRegistry;

/// Registry mapping terminal_id → WeakEntity<TerminalContent> for direct
/// dirty notification from PTY event loop (avoids per-pane polling).
pub type ContentPaneRegistry =
    Arc<Mutex<HashMap<String, WeakEntity<super::layout::terminal_pane::TerminalContent>>>>;

/// Global content pane registry instance.
static CONTENT_PANE_REGISTRY: std::sync::OnceLock<ContentPaneRegistry> = std::sync::OnceLock::new();

/// Get or init the global content pane registry.
pub fn content_pane_registry() -> &'static ContentPaneRegistry {
    CONTENT_PANE_REGISTRY.get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
}

/// Root view of the application
pub struct RootView {
    workspace: Entity<Workspace>,
    request_broker: Entity<RequestBroker>,
    backend: Arc<dyn TerminalBackend>,
    terminals: TerminalsRegistry,
    sidebar: Entity<Sidebar>,
    /// Sidebar state controller
    sidebar_ctrl: SidebarController,
    /// Stored project column entities (created once, not during render)
    project_columns: HashMap<String, Entity<ProjectColumn>>,
    /// Title bar entity
    title_bar: Entity<TitleBar>,
    /// Status bar entity
    status_bar: Entity<StatusBar>,
    /// Centralized overlay manager
    overlay_manager: Entity<OverlayManager>,
    /// Toast notification overlay
    toast_overlay: Entity<ToastOverlay>,
    /// Shared drag state for resize operations
    active_drag: ActiveDrag,
    /// Focus handle for capturing global keybindings
    focus_handle: FocusHandle,
    /// Scroll handle for horizontal scrolling of project columns
    projects_scroll_handle: ScrollHandle,
    /// Persistent container bounds for projects grid (used to compute pixel widths)
    projects_grid_bounds: Rc<RefCell<Bounds<Pixels>>>,
    /// Horizontal scrollbar drag state
    hscroll_dragging: bool,
    hscroll_bounds: Rc<RefCell<Option<Bounds<Pixels>>>>,
    /// Remote connection manager (set after creation)
    remote_manager: Option<Entity<RemoteConnectionManager>>,
    /// Git status watcher (set by Vryn after creation)
    git_watcher: Option<Entity<GitStatusWatcher>>,
    /// Whether the pane switcher overlay is active
    pane_switch_active: bool,
    /// Pane switcher overlay entity (separate entity for proper focus handling)
    pane_switcher_entity: Option<Entity<pane_switcher::PaneSwitcher>>,
    /// Service manager (set by Vryn after creation)
    service_manager: Option<Entity<ServiceManager>>,
    /// Last focused project ID (for scroll-to-focused detection)
    last_scroll_project: Option<String>,
    /// Whether a project was zoomed/focused in the last observation (for detecting unfocus)
    was_project_focused: bool,
    /// Effective project for git-panel following (focused || focused-terminal's project).
    /// Used to detect project-context changes and refresh the git panel.
    last_git_context_project: Option<String>,
    /// Project ID to center-scroll to after the next layout pass
    pending_center_scroll: Option<String>,
    /// Git panel state controller (right-side panel)
    git_panel_ctrl: SidebarController,
    /// Project ID whose git log is shown in the git panel
    git_panel_project_id: Option<String>,
    /// Diff viewer shown in the central project area from the git changes list.
    main_diff_viewer: Option<Entity<vryn_views_git::diff_viewer::DiffViewer>>,
    /// File viewer shown in the central project area from the sidebar explorer.
    main_file_viewer: Option<Entity<vryn_files::file_viewer::FileViewer>>,
    /// Pending debounced full-refresh tasks per project (for `.git/` event
    /// storms during rebase/checkout). Dropping the task cancels it.
    pending_git_internal_refresh: HashMap<String, Task<()>>,
}

impl RootView {
    pub fn new(
        workspace: Entity<Workspace>,
        request_broker: Entity<RequestBroker>,
        pty_manager: Arc<PtyManager>,
        cx: &mut Context<Self>,
    ) -> Self {
        let terminals: TerminalsRegistry = Arc::new(Mutex::new(HashMap::new()));
        vryn_terminal::set_global_registry(terminals.clone());

        // Create sidebar controller from current global settings
        let app_settings = settings(cx);
        let sidebar_ctrl = SidebarController::new(&app_settings);

        // Create git panel controller from settings (reuse SidebarController)
        let git_panel_ctrl = SidebarController::new_with_panel_settings(
            app_settings.git_panel.is_open,
            app_settings.git_panel.width,
        );

        // Create sidebar entity once to preserve state
        let sidebar = cx.new(|cx| {
            Sidebar::new(
                workspace.clone(),
                request_broker.clone(),
                terminals.clone(),
                cx,
            )
        });

        // Create focus handle for global keybindings and titlebar action dispatch.
        let focus_handle = cx.focus_handle();

        // Create title bar entity (sync initial sidebar + git-panel state)
        let sidebar_initially_open = sidebar_ctrl.is_open();
        let git_panel_initially_open = git_panel_ctrl.is_open();
        let workspace_for_title = workspace.clone();
        let title_bar = cx.new(|cx| {
            let mut tb = TitleBar::new("Vryn", workspace_for_title, cx);
            tb.set_sidebar_open(sidebar_initially_open, cx);
            tb.set_git_panel_open(git_panel_initially_open, cx);
            tb.set_action_focus_handle(focus_handle.clone());
            tb
        });

        // Create status bar entity (sync initial sidebar state)
        let workspace_for_status = workspace.clone();
        let status_bar = cx.new(|cx| {
            let mut sb = StatusBar::new(workspace_for_status, cx);
            sb.set_sidebar_open(sidebar_initially_open, cx);
            sb
        });

        // Create overlay manager
        let overlay_manager =
            cx.new(|_cx| OverlayManager::new(workspace.clone(), request_broker.clone()));

        // Create toast overlay
        let toast_overlay = cx.new(ToastOverlay::new);

        // Subscribe to overlay manager events
        cx.subscribe(&overlay_manager, Self::handle_overlay_manager_event)
            .detach();

        // Observe RequestBroker to process overlay requests outside of render()
        cx.observe(&request_broker, |this, _broker, cx| {
            if this.request_broker.read(cx).has_overlay_requests() {
                this.process_pending_requests(cx);
            }
        })
        .detach();

        // Wrap PtyManager in LocalBackend for the TerminalBackend trait
        let backend: Arc<dyn TerminalBackend> = Arc::new(LocalBackend::new(pty_manager));

        // Wire up sidebar callbacks
        {
            let workspace_for_dispatch = workspace.clone();
            let backend_for_dispatch = backend.clone();
            let terminals_for_dispatch = terminals.clone();
            sidebar.update(cx, |s, _cx| {
                // Dispatch action callback
                s.set_dispatch_action(Box::new(move |project_id, action, cx| {
                    if let Some(dispatcher) = crate::action_dispatch::dispatcher_for_project(
                        project_id,
                        &workspace_for_dispatch,
                        &Some(backend_for_dispatch.clone()),
                        &terminals_for_dispatch,
                        &None, // service_manager - wired later
                        &None, // remote_manager - wired later
                        cx,
                    ) {
                        dispatcher.dispatch(action, cx);
                    }
                }));

                // Settings callback
                s.set_settings(Box::new(|cx| {
                    let app_settings = crate::settings::settings(cx);
                    vryn_views_sidebar::SidebarSettings {
                        worktree_path_template: app_settings.worktree.path_template.clone(),
                        hooks: app_settings.hooks.clone(),
                        show_all_projects_on_projects_click: app_settings
                            .show_all_projects_on_projects_click,
                        monochrome_icons: app_settings.monochrome_icons,
                    }
                }));
            });
        }

        let mut view = Self {
            workspace,
            request_broker,
            backend,
            terminals,
            sidebar,
            sidebar_ctrl,
            project_columns: HashMap::new(),
            title_bar,
            status_bar,
            overlay_manager,
            toast_overlay,
            active_drag: new_active_drag(),
            focus_handle,
            projects_scroll_handle: ScrollHandle::new(),
            projects_grid_bounds: Rc::new(RefCell::new(Bounds {
                origin: Point::default(),
                size: Size {
                    width: px(800.0),
                    height: px(600.0),
                },
            })),
            hscroll_dragging: false,
            hscroll_bounds: Rc::new(RefCell::new(None)),
            service_manager: None,
            remote_manager: None,
            git_watcher: None,
            pane_switch_active: false,
            pane_switcher_entity: None,
            last_scroll_project: None,
            was_project_focused: false,
            last_git_context_project: None,
            pending_center_scroll: None,
            git_panel_ctrl,
            git_panel_project_id: None,
            main_diff_viewer: None,
            main_file_viewer: None,
            pending_git_internal_refresh: HashMap::new(),
        };

        // Observe workspace to scroll focused project into view AND keep
        // the git panel in sync with the active project context.
        cx.observe(&view.workspace, |this, workspace, cx| {
            let (is_project_focused, focused_terminal_project, git_context) = {
                let ws = workspace.read(cx);
                let focused_project = ws.focus_manager.focused_project_id().cloned();
                let focused_terminal_project = ws
                    .focus_manager
                    .focused_terminal_state()
                    .map(|f| f.project_id.clone());
                let git_context = focused_project
                    .clone()
                    .or_else(|| focused_terminal_project.clone());
                (
                    focused_project.is_some(),
                    focused_terminal_project,
                    git_context,
                )
            };

            // When project zoom is cleared, defer centering until after next layout pass
            if this.was_project_focused && !is_project_focused {
                this.last_scroll_project = focused_terminal_project.clone();
                this.pending_center_scroll = focused_terminal_project;
            }
            // When the active terminal changes project, ensure it's visible
            else if focused_terminal_project != this.last_scroll_project
                && focused_terminal_project.is_some()
            {
                this.last_scroll_project = focused_terminal_project.clone();
                this.scroll_to_focused_project(focused_terminal_project.as_deref(), false, cx);
            }

            this.was_project_focused = is_project_focused;

            if git_context != this.last_git_context_project {
                this.last_git_context_project = git_context.clone();
                if let Some(pid) = git_context {
                    this.follow_git_panel_to_project(&pid, cx);
                    // Nudge the sidebar's file explorer for this project.
                    let sidebar = this.sidebar.clone();
                    sidebar.update(cx, |sb, cx| sb.refresh_file_explorer(&pid, cx));
                }
            }
        })
        .detach();

        // Initialize project columns
        view.sync_project_columns(cx);

        view
    }

    /// Get the terminals registry (for sharing with detached windows)
    pub fn terminals(&self) -> &TerminalsRegistry {
        &self.terminals
    }

    /// Schedule a debounced full git-status refresh for a project. Called
    /// when `.git/` internal events fire; coalesces rebase/checkout storms
    /// into one refresh 500ms after the last event.
    fn schedule_git_internal_refresh(&mut self, project_id: String, cx: &mut Context<Self>) {
        let pid = project_id.clone();
        let task = cx.spawn(async move |this: WeakEntity<Self>, cx| {
            smol::Timer::after(std::time::Duration::from_millis(500)).await;
            let _ = this.update(cx, |this, cx| {
                let sidebar = this.sidebar.clone();
                sidebar.update(cx, |sb, cx| sb.refresh_file_explorer(&pid, cx));
                if let Some(col) = this.project_columns.get(&pid).cloned() {
                    let gh = col.read(cx).git_header();
                    gh.update(cx, |gh, cx| gh.refresh_working_tree_status(cx));
                }
                this.pending_git_internal_refresh.remove(&pid);
            });
        });
        // Dropping the old task cancels it.
        self.pending_git_internal_refresh.insert(project_id, task);
    }

    /// Set the git watcher entity (called by Vryn after creation).
    pub fn set_git_watcher(&mut self, watcher: Entity<GitStatusWatcher>, cx: &mut Context<Self>) {
        // Observe the watcher so the sidebar's file explorer refreshes when
        // git status changes from the slow status-poll loop. ProjectColumns
        // re-read via their own observers on each render; the file explorer
        // owns its own per-file status cache, so it must be nudged.
        cx.observe(&watcher, |this, _watcher, cx| {
            let sidebar = this.sidebar.clone();
            sidebar.update(cx, |sb, cx| {
                for pid in sb.file_explorer_project_ids() {
                    sb.refresh_file_explorer(&pid, cx);
                }
            });
        })
        .detach();

        // Subscribe to per-project FS change events (notify-driven) so the
        // file explorer + git header can incrementally patch without a full
        // rebuild. `.git/` events route to a 500ms-debounced full refresh so
        // `git rebase` / `git checkout` storms coalesce into one refresh.
        cx.subscribe(
            &watcher,
            |this, _watcher, event: &crate::git::watcher::FsChangeEvent, cx| {
                let pid = event.project_id.clone();
                let files = event.files.clone();
                let is_git_internal = event.is_git_internal;

                if is_git_internal {
                    this.schedule_git_internal_refresh(pid, cx);
                    return;
                }

                // File Explorer — incremental
                {
                    let sidebar = this.sidebar.clone();
                    let files_for_fe = files.clone();
                    let pid_for_fe = pid.clone();
                    sidebar.update(cx, |sb, cx| {
                        sb.patch_file_explorer_paths(&pid_for_fe, &files_for_fe, cx);
                    });
                }

                // Git Header — incremental
                if let Some(col) = this.project_columns.get(&pid).cloned() {
                    let gh = col.read(cx).git_header();
                    let repo_root = gh.read(cx).local_repo_root();
                    let Some(root) = repo_root else {
                        return;
                    };
                    let rel_paths: Vec<String> = files
                        .iter()
                        .filter_map(|p| p.strip_prefix(&root).ok())
                        .map(|p| p.to_string_lossy().replace('\\', "/"))
                        .collect();
                    if rel_paths.is_empty() {
                        return;
                    }
                    gh.update(cx, |gh, cx| {
                        gh.patch_files(rel_paths, cx);
                    });
                }
            },
        )
        .detach();

        self.git_watcher = Some(watcher);
        // Drop existing local columns so they get recreated with the watcher
        self.project_columns
            .retain(|id, _| id.starts_with("remote:"));
        self.sync_project_columns(cx);
    }

    /// Set the remote connection manager (called after creation by Vryn).
    pub fn set_remote_manager(
        &mut self,
        manager: Entity<RemoteConnectionManager>,
        cx: &mut Context<Self>,
    ) {
        // Observe remote manager and sync remote projects into workspace
        let workspace = self.workspace.clone();
        cx.observe(&manager, move |this, rm, cx| {
            Self::sync_remote_projects_into_workspace(&workspace, &rm, cx);
            this.sync_project_columns(cx);
            cx.notify();
        })
        .detach();

        // Wire up remote callbacks on sidebar
        {
            let rm_for_connections = manager.clone();
            let rm_for_send = manager.clone();
            let rm_for_folder = manager.clone();
            self.sidebar.update(cx, |sidebar, _cx| {
                // Get remote connections callback
                sidebar.set_remote_connections(Box::new(move |cx| {
                    rm_for_connections
                        .read(cx)
                        .connections()
                        .iter()
                        .map(|(config, status, _state)| {
                            vryn_views_sidebar::RemoteConnectionSnapshot {
                                config: (*config).clone(),
                                status: (*status).clone(),
                            }
                        })
                        .collect()
                }));

                // Send remote action callback
                sidebar.set_send_remote_action(Box::new(move |conn_id, action, cx| {
                    rm_for_send.update(cx, |rm, cx| {
                        rm.send_action(conn_id, action, cx);
                    });
                }));

                // Get remote folder callback
                sidebar.set_get_remote_folder(Box::new(move |conn_id, prefixed_project_id, cx| {
                    let server_project_id =
                        vryn_core::client::strip_prefix(prefixed_project_id, conn_id);
                    rm_for_folder
                        .read(cx)
                        .connections()
                        .iter()
                        .find(|(config, _, _)| config.id == conn_id)
                        .and_then(|(_, _, state)| state.as_ref())
                        .and_then(|state| {
                            state
                                .folders
                                .iter()
                                .find(|f| f.project_ids.contains(&server_project_id))
                                .map(|f| f.id.clone())
                        })
                }));
            });

            // Observe remote manager for sidebar updates
            let sidebar_for_observe = self.sidebar.clone();
            cx.observe(&manager, move |_this, _rm, cx| {
                sidebar_for_observe.update(cx, |_, cx| cx.notify());
            })
            .detach();
        }

        self.remote_manager = Some(manager);

        // Rebuild dispatch callback with remote manager
        self.rebuild_sidebar_dispatch(cx);
    }

    /// Set the service manager entity (called by Vryn after creation).
    pub fn set_service_manager(&mut self, manager: Entity<ServiceManager>, cx: &mut Context<Self>) {
        cx.observe(&manager, |_this, _sm, cx| {
            cx.notify();
        })
        .detach();

        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_service_manager(manager.clone(), cx);
        });

        // Rebuild dispatch callback with service manager
        self.rebuild_sidebar_dispatch(cx);

        // Wire service manager into existing project columns
        for col in self.project_columns.values() {
            col.update(cx, |col, cx| {
                col.set_service_manager(manager.clone(), cx);
            });
        }

        self.service_manager = Some(manager);
    }

    /// Rebuild the sidebar dispatch action callback with current service/remote managers.
    fn rebuild_sidebar_dispatch(&self, cx: &mut Context<Self>) {
        let workspace = self.workspace.clone();
        let backend = self.backend.clone();
        let terminals = self.terminals.clone();
        let service_manager = self.service_manager.clone();
        let remote_manager = self.remote_manager.clone();
        self.sidebar.update(cx, |s, _cx| {
            s.set_dispatch_action(Box::new(move |project_id, action, cx| {
                if let Some(dispatcher) = crate::action_dispatch::dispatcher_for_project(
                    project_id,
                    &workspace,
                    &Some(backend.clone()),
                    &terminals,
                    &service_manager,
                    &remote_manager,
                    cx,
                ) {
                    dispatcher.dispatch(action, cx);
                }
            }));
        });
    }

    /// Sync remote connection state into workspace as materialized ProjectData entries.
    fn sync_remote_projects_into_workspace(
        workspace: &Entity<Workspace>,
        rm: &Entity<RemoteConnectionManager>,
        cx: &mut Context<Self>,
    ) {
        use crate::workspace::settings::HooksConfig;
        use crate::workspace::state::{FolderData, LayoutNode, ProjectData};
        use vryn_core::client::RemoteConnectionConfig;

        // Snapshot all connection data into owned structures to release the borrow on cx
        struct ConnSnapshot {
            config: RemoteConnectionConfig,
            state: Option<vryn_core::api::StateResponse>,
        }
        let snapshots: Vec<ConnSnapshot> = {
            let rm_read = rm.read(cx);
            rm_read
                .connections()
                .iter()
                .map(|(config, _status, state)| ConnSnapshot {
                    config: (*config).clone(),
                    state: state.cloned(),
                })
                .collect()
        };

        let mut expected_remote_ids: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let active_conn_ids: std::collections::HashSet<String> =
            snapshots.iter().map(|s| s.config.id.clone()).collect();

        // Collect old terminal IDs for projects pending focus, so we can detect new ones after sync.
        let old_terminal_ids: std::collections::HashMap<String, Vec<String>> =
            workspace.update(cx, |ws, _cx| {
                ws.remote_sync
                    .pending_focus()
                    .iter()
                    .map(|pid| {
                        let ids = ws
                            .project(pid)
                            .and_then(|p| p.layout.as_ref())
                            .map(|l| l.collect_terminal_ids())
                            .unwrap_or_default();
                        (pid.clone(), ids)
                    })
                    .collect()
            });

        for snap in &snapshots {
            let conn_id = &snap.config.id;

            if let Some(ref state) = snap.state {
                // Build the server folder lookup
                let server_folder_map: std::collections::HashMap<&str, &vryn_core::api::ApiFolder> =
                    state.folders.iter().map(|f| (f.id.as_str(), f)).collect();

                // Build prefixed project_order and folder entries that mirror the server structure
                let mut remote_order: Vec<String> = Vec::new();
                let mut remote_folders: Vec<FolderData> = Vec::new();

                if !state.project_order.is_empty() {
                    for order_id in &state.project_order {
                        if let Some(sf) = server_folder_map.get(order_id.as_str()) {
                            // This is a folder — create a prefixed FolderData
                            let prefixed_folder_id = format!("remote:{}:{}", conn_id, sf.id);
                            let prefixed_project_ids: Vec<String> = sf
                                .project_ids
                                .iter()
                                .map(|pid| format!("remote:{}:{}", conn_id, pid))
                                .collect();
                            remote_folders.push(FolderData {
                                id: prefixed_folder_id.clone(),
                                name: sf.name.clone(),
                                project_ids: prefixed_project_ids,
                                collapsed: false,
                                folder_color: sf.folder_color,
                            });
                            remote_order.push(prefixed_folder_id);
                        } else {
                            // This is a top-level project
                            remote_order.push(format!("remote:{}:{}", conn_id, order_id));
                        }
                    }
                } else {
                    // Old server without project_order: put all projects as top-level
                    for api_project in &state.projects {
                        remote_order.push(format!("remote:{}:{}", conn_id, api_project.id));
                    }
                };

                for api_project in &state.projects {
                    let prefixed_id = format!("remote:{}:{}", conn_id, api_project.id);
                    expected_remote_ids.insert(prefixed_id.clone());

                    let layout = api_project
                        .layout
                        .as_ref()
                        .map(|l| LayoutNode::from_api_prefixed(l, &format!("remote:{}", conn_id)));

                    let terminal_names: std::collections::HashMap<String, String> = api_project
                        .terminal_names
                        .iter()
                        .map(|(k, v)| (format!("remote:{}:{}", conn_id, k), v.clone()))
                        .collect();

                    let project_color = api_project.folder_color;
                    let conn_id_owned = conn_id.clone();

                    // Build remote services with prefixed terminal IDs
                    let remote_services: Vec<vryn_core::api::ApiServiceInfo> = api_project
                        .services
                        .iter()
                        .map(|s| {
                            let mut svc = s.clone();
                            svc.terminal_id = s
                                .terminal_id
                                .as_ref()
                                .map(|tid| format!("remote:{}:{}", conn_id, tid));
                            svc
                        })
                        .collect();
                    let remote_host = Some(snap.config.host.clone());
                    let remote_git_status = api_project.git_status.clone();

                    workspace.update(cx, |ws, _cx| {
                        if let Some(existing) =
                            ws.data.projects.iter_mut().find(|p| p.id == prefixed_id)
                        {
                            existing.name = api_project.name.clone();
                            existing.path = api_project.path.clone();
                            // Merge server layout with locally-preserved visual state
                            // (split sizes, minimized, detached, active_tab).
                            existing.layout = match (&existing.layout, &layout) {
                                (Some(local), Some(server)) => {
                                    Some(LayoutNode::merge_visual_state(server, local))
                                }
                                _ => layout,
                            };
                            existing.terminal_names = terminal_names;
                            existing.folder_color = project_color;
                            existing.worktree_info = api_project.worktree_info.as_ref().map(|wt| {
                                crate::workspace::state::WorktreeMetadata {
                                    parent_project_id: format!(
                                        "remote:{}:{}",
                                        conn_id, wt.parent_project_id
                                    ),
                                    color_override: wt.color_override,
                                    main_repo_path: String::new(),
                                    worktree_path: String::new(),
                                    branch_name: String::new(),
                                }
                            });
                            existing.worktree_ids = api_project
                                .worktree_ids
                                .iter()
                                .map(|id| format!("remote:{}:{}", conn_id, id))
                                .collect();
                            // Don't overwrite show_in_overview — it's client-side state
                            // (the user may have toggled visibility locally).
                        } else {
                            let worktree_info = api_project.worktree_info.as_ref().map(|wt| {
                                crate::workspace::state::WorktreeMetadata {
                                    parent_project_id: format!(
                                        "remote:{}:{}",
                                        conn_id, wt.parent_project_id
                                    ),
                                    color_override: wt.color_override,
                                    main_repo_path: String::new(),
                                    worktree_path: String::new(),
                                    branch_name: String::new(),
                                }
                            });
                            let worktree_ids: Vec<String> = api_project
                                .worktree_ids
                                .iter()
                                .map(|id| format!("remote:{}:{}", conn_id, id))
                                .collect();
                            ws.data.projects.push(ProjectData {
                                id: prefixed_id.clone(),
                                name: api_project.name.clone(),
                                path: api_project.path.clone(),
                                show_in_overview: api_project.show_in_overview,
                                layout,
                                terminal_names,
                                hidden_terminals: std::collections::HashMap::new(),
                                worktree_info,
                                worktree_ids,
                                folder_color: project_color,
                                hooks: HooksConfig::default(),
                                is_remote: true,
                                connection_id: Some(conn_id_owned),
                                service_terminals: std::collections::HashMap::new(),
                                default_shell: None,
                                hook_terminals: std::collections::HashMap::new(),
                            });
                        }
                        // Update the transient remote snapshot regardless of create/update path.
                        let snapshot = ws.remote_sync.snapshot_mut(&prefixed_id);
                        snapshot.services = remote_services;
                        snapshot.host = remote_host;
                        snapshot.git_status = remote_git_status;
                    });
                }

                // Sync remote folders and project_order into workspace
                let remote_prefix = format!("remote:{}:", conn_id);
                workspace.update(cx, |ws, _cx| {
                    // Remove old remote folders for this connection
                    ws.data
                        .folders
                        .retain(|f| !f.id.starts_with(&remote_prefix));
                    // Remove old remote entries from project_order for this connection
                    ws.data
                        .project_order
                        .retain(|id| !id.starts_with(&remote_prefix));

                    // Add new remote folders
                    for rf in remote_folders {
                        // Preserve collapsed state from previous sync
                        ws.data.folders.push(rf);
                    }

                    // Add new remote project_order entries
                    ws.data.project_order.extend(remote_order);
                });
            } else {
                // No state (disconnected/connecting) — remove materialized projects and folders
                let prefix = format!("remote:{}:", conn_id);
                workspace.update(cx, |ws, _cx| {
                    ws.data.projects.retain(|p| !p.id.starts_with(&prefix));
                    ws.data.folders.retain(|f| !f.id.starts_with(&prefix));
                    ws.data.project_order.retain(|id| !id.starts_with(&prefix));
                });
            }
        }

        // Remove stale remote projects/folders from connections that no longer exist
        workspace.update(cx, |ws, _cx| {
            ws.data.projects.retain(|p| {
                if p.is_remote {
                    expected_remote_ids.contains(&p.id)
                } else {
                    true
                }
            });
            ws.data.folders.retain(|f| {
                if f.id.starts_with("remote:") {
                    // Remote folder IDs are "remote:{conn_id}:{folder_id}"
                    // Extract conn_id (second segment)
                    let rest = f.id.strip_prefix("remote:").unwrap_or("");
                    let conn_id = rest.split(':').next().unwrap_or("");
                    active_conn_ids.contains(conn_id)
                } else {
                    true
                }
            });
            let valid_ids: std::collections::HashSet<&str> = ws
                .data
                .projects
                .iter()
                .map(|p| p.id.as_str())
                .chain(ws.data.folders.iter().map(|f| f.id.as_str()))
                .collect();
            ws.data
                .project_order
                .retain(|id| valid_ids.contains(id.as_str()));
        });

        // Focus newly appeared terminals for projects that had a pending CreateTerminal.
        if !old_terminal_ids.is_empty() {
            workspace.update(cx, |ws, cx| {
                let pending: Vec<String> = ws.drain_pending_remote_focus();
                for pid in pending {
                    let old_ids = match old_terminal_ids.get(&pid) {
                        Some(ids) => ids,
                        None => continue,
                    };
                    let new_ids = match ws.project(&pid).and_then(|p| p.layout.as_ref()) {
                        Some(layout) => layout.collect_terminal_ids(),
                        None => continue,
                    };
                    // Find the first terminal ID that wasn't in the old set
                    let old_set: std::collections::HashSet<&str> =
                        old_ids.iter().map(|s| s.as_str()).collect();
                    if let Some(new_tid) = new_ids.iter().find(|id| !old_set.contains(id.as_str()))
                        && let Some(path) = ws
                            .project(&pid)
                            .and_then(|p| p.layout.as_ref())
                            .and_then(|l| l.find_terminal_path(new_tid))
                    {
                        ws.set_focused_terminal(pid.clone(), path, cx);
                    }
                }
            });
        }

        // Notify UI without bumping data_version (remote changes shouldn't trigger auto-save)
        workspace.update(cx, |ws, cx| {
            ws.notify_ui_only(cx);
        });
    }

    /// Ensure project columns exist for all visible projects
    pub(super) fn sync_project_columns(&mut self, cx: &mut Context<Self>) {
        let visible_projects: Vec<(String, bool, Option<String>)> = {
            let ws = self.workspace.read(cx);
            ws.visible_projects()
                .iter()
                .map(|p| (p.id.clone(), p.is_remote, p.connection_id.clone()))
                .collect()
        };

        // Clean up columns for projects that no longer exist
        let visible_ids: std::collections::HashSet<&str> = visible_projects
            .iter()
            .map(|(id, _, _)| id.as_str())
            .collect();
        self.project_columns.retain(|id, _| {
            // Keep local project columns even when not visible (they may become visible again)
            // But remove remote project columns that are gone
            if id.starts_with("remote:") {
                visible_ids.contains(id.as_str())
            } else {
                true
            }
        });

        // Create columns for new projects
        for (project_id, is_remote, connection_id) in &visible_projects {
            if !self.project_columns.contains_key(project_id) {
                let entity = if *is_remote {
                    self.create_remote_column(project_id, connection_id.as_deref(), cx)
                } else {
                    Some(self.create_local_column(project_id, cx))
                };
                if let Some(entity) = entity {
                    // Observe the column's GitHeader so RootView re-renders
                    // when its state changes (commit log load, working-tree
                    // refresh). Without this, render_git_panel reads stale
                    // state on first project switch — ProjectColumn observes
                    // its own GitHeader, but RootView's render_git_panel
                    // lives outside the ProjectColumn's render subtree.
                    let gh = entity.read(cx).git_header();
                    cx.observe(&gh, |_, _, cx| cx.notify()).detach();
                    self.project_columns.insert(project_id.clone(), entity);
                }
            }
        }
    }

    /// Create a ProjectColumn for a remote project.
    fn create_remote_column(
        &self,
        project_id: &str,
        connection_id: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Option<Entity<ProjectColumn>> {
        let conn_id = connection_id?;
        let backend = self
            .remote_manager
            .as_ref()
            .and_then(|rm| rm.read(cx).backend_for(conn_id))?;

        let workspace_clone = self.workspace.clone();
        let request_broker_clone = self.request_broker.clone();
        let terminals_clone = self.terminals.clone();
        let active_drag_clone = self.active_drag.clone();
        let id = project_id.to_string();
        let workspace_for_dispatch = self.workspace.clone();
        let action_dispatcher = self.remote_manager.as_ref().map(|rm| {
            crate::action_dispatch::ActionDispatcher::Remote {
                connection_id: conn_id.to_string(),
                manager: rm.clone(),
                workspace: workspace_for_dispatch,
            }
        });
        let ws_for_observe = self.workspace.clone();

        let git_provider = self.build_git_provider(project_id, cx)?;

        Some(cx.new(move |cx| {
            let mut col = ProjectColumn::new(
                workspace_clone,
                request_broker_clone,
                id,
                backend,
                terminals_clone,
                active_drag_clone,
                None, // remote projects don't get git watcher
                git_provider,
                cx,
            );
            col.set_action_dispatcher(action_dispatcher);
            // Observe workspace for remote service state changes
            // (instead of local ServiceManager which has no data for remote projects)
            col.observe_remote_services(ws_for_observe, cx);
            col
        }))
    }

    /// Create a ProjectColumn for a local project.
    fn create_local_column(
        &self,
        project_id: &str,
        cx: &mut Context<Self>,
    ) -> Entity<ProjectColumn> {
        let workspace_clone = self.workspace.clone();
        let request_broker_clone = self.request_broker.clone();
        let terminals_clone = self.terminals.clone();
        let active_drag_clone = self.active_drag.clone();
        let id = project_id.to_string();
        let backend_clone = self.backend.clone();
        let workspace_for_dispatch = self.workspace.clone();
        let backend_for_dispatch = self.backend.clone();
        let terminals_for_dispatch = self.terminals.clone();
        let git_watcher = self.git_watcher.clone();

        let git_provider = match self.build_git_provider(project_id, cx) {
            Some(p) => p,
            None => {
                log::warn!("Cannot build git provider for project {}", project_id);
                let path = self
                    .workspace
                    .read(cx)
                    .project(project_id)
                    .map(|p| p.path.clone())
                    .unwrap_or_default();
                Arc::new(vryn_views_git::diff_viewer::provider::LocalGitProvider::new(path))
            }
        };

        let entity = cx.new(move |cx| {
            let mut col = ProjectColumn::new(
                workspace_clone,
                request_broker_clone,
                id,
                backend_clone,
                terminals_clone,
                active_drag_clone,
                git_watcher,
                git_provider,
                cx,
            );
            col.set_action_dispatcher(Some(crate::action_dispatch::ActionDispatcher::Local {
                workspace: workspace_for_dispatch,
                backend: backend_for_dispatch,
                terminals: terminals_for_dispatch,
                service_manager: None, // set later via set_service_manager
            }));
            col
        });
        if let Some(ref sm) = self.service_manager {
            entity.update(cx, |col, cx| col.set_service_manager(sm.clone(), cx));
        }
        entity
    }
}

impl_focusable!(RootView);
