//! Workspace File Explorer panel — lazy-loading directory tree with
//! per-file git status decoration. Right-click opens a notmux-style context
//! menu (files/folders get full CRUD actions; empty area gets new-file/
//! new-folder/paste/reveal).

use gpui::prelude::*;
use gpui::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use notmux_files::clipboard::ExplorerClipboard;
use notmux_files::dir_listing::{DirEntry, list_directory};
use notmux_files::fs_ops;
use notmux_ui::theme::sidebar_theme as theme;
use notmux_git::{FileStatus, WorkingFile, WorkingTreeStatus};
use notmux_ui::simple_input::{SimpleInput, SimpleInputState};
use notmux_ui::tokens::ui_text_md;
use notmux_ui::vscode_icon::vscode_file_icon_sized_with_options;
use notmux_workspace::request_broker::RequestBroker;
use notmux_workspace::requests::{ExplorerKind, OverlayRequest};

use crate::{ExplorerInputCancel, ExplorerInputConfirm};

/// Active inline-input mode (rename target, or new file/folder under parent).
#[derive(Clone, Debug)]
enum InputMode {
    Rename { target: PathBuf },
    NewFile { parent: PathBuf },
    NewFolder { parent: PathBuf },
}

struct ActiveInput {
    mode: InputMode,
    input: Entity<SimpleInputState>,
    needs_focus: bool,
}

/// Root-level GPUI entity for the file explorer panel.
pub struct FileExplorer {
    project_id: String,
    project_path: PathBuf,
    request_broker: Entity<RequestBroker>,

    /// Entries per directory path. Lazy-populated on expand.
    loaded_children: HashMap<PathBuf, Vec<DirEntry>>,
    expanded_paths: HashSet<PathBuf>,
    loading_paths: HashSet<PathBuf>,

    /// Latest git status snapshot + flattened per-path status lookup.
    working_tree_status: Option<WorkingTreeStatus>,
    git_status_by_relpath: HashMap<String, FileStatus>,
    untracked_relpaths: HashSet<String>,
    staged_relpaths: HashSet<String>,
    conflict_relpaths: HashSet<String>,

    scroll_handle: ScrollHandle,

    active_input: Option<ActiveInput>,
    monochrome_icons: bool,
    /// Whether dotfile entries should be included in directory listings.
    /// `.git` is always excluded regardless of this flag.
    show_hidden: bool,

    /// Path of the row whose context menu is currently open. The row gets a
    /// persistent highlight so the user sees which entry they're operating
    /// on even though the menu backdrop occludes hover events.
    context_menu_target: Option<PathBuf>,
}

impl FileExplorer {
    pub fn new(
        project_id: String,
        project_path: PathBuf,
        request_broker: Entity<RequestBroker>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            project_id,
            project_path: project_path.clone(),
            request_broker,
            loaded_children: HashMap::new(),
            expanded_paths: HashSet::new(),
            loading_paths: HashSet::new(),
            working_tree_status: None,
            git_status_by_relpath: HashMap::new(),
            untracked_relpaths: HashSet::new(),
            staged_relpaths: HashSet::new(),
            conflict_relpaths: HashSet::new(),
            scroll_handle: ScrollHandle::new(),
            active_input: None,
            monochrome_icons: false,
            show_hidden: true,
            context_menu_target: None,
        };
        this.load_directory(project_path, cx);
        this.refresh_git_status(cx);
        this
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    pub fn project_path(&self) -> &Path {
        &self.project_path
    }

    pub fn set_monochrome_icons(&mut self, monochrome_icons: bool, cx: &mut Context<Self>) {
        if self.monochrome_icons == monochrome_icons {
            return;
        }
        self.monochrome_icons = monochrome_icons;
        cx.notify();
    }

    /// Toggle visibility of hidden (dotfile) entries. When the flag flips
    /// the root directory is re-listed so the change is visible immediately.
    pub fn set_show_hidden(&mut self, show_hidden: bool, cx: &mut Context<Self>) {
        if self.show_hidden == show_hidden {
            return;
        }
        self.show_hidden = show_hidden;
        // Re-list the root so the user sees the new entries without having
        // to collapse/expand. Children will lazy-load on next expand.
        self.loaded_children.remove(&self.project_path);
        self.load_directory(self.project_path.clone(), cx);
    }

    /// Reload the root directory listing and refresh git status.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let paths: Vec<PathBuf> = self.expanded_paths.iter().cloned().collect();
        self.loaded_children.clear();
        self.load_directory(self.project_path.clone(), cx);
        for p in paths {
            if p != self.project_path {
                self.load_directory(p, cx);
            }
        }
        self.refresh_git_status(cx);
    }

    /// Reload only the listed absolute paths. Intended for Phase B watcher
    /// events. Paths outside the project root are ignored. Each changed
    /// file's parent directory is re-listed so add/delete is reflected.
    pub fn patch_paths(&mut self, changed: &[PathBuf], cx: &mut Context<Self>) {
        let mut dirs: HashSet<PathBuf> = HashSet::new();
        for p in changed {
            let parent = p
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| p.clone());
            if parent.starts_with(&self.project_path) && self.loaded_children.contains_key(&parent)
            {
                dirs.insert(parent);
            }
        }
        for dir in dirs {
            self.load_directory(dir, cx);
        }
        self.refresh_git_status(cx);
    }

    fn load_directory(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.loading_paths.contains(&path) {
            return;
        }
        self.loading_paths.insert(path.clone());
        let owned = path.clone();
        let show_hidden = self.show_hidden;
        cx.spawn(async move |this, cx| {
            let entries = smol::unblock(move || list_directory(&owned, show_hidden)).await;
            let _ = this.update(cx, |this, cx| {
                this.loaded_children.insert(path.clone(), entries);
                this.loading_paths.remove(&path);
                cx.notify();
            });
        })
        .detach();
    }

    fn refresh_git_status(&mut self, cx: &mut Context<Self>) {
        let project_path = self.project_path.clone();
        cx.spawn(async move |this, cx| {
            let status =
                smol::unblock(move || notmux_git::get_working_tree_status(&project_path)).await;
            let _ = this.update(cx, |this, cx| {
                this.apply_status(status);
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_status(&mut self, status: WorkingTreeStatus) {
        let mut map: HashMap<String, FileStatus> = HashMap::new();
        let mut untracked: HashSet<String> = HashSet::new();
        let mut staged: HashSet<String> = HashSet::new();
        let mut conflicts: HashSet<String> = HashSet::new();

        let collect = |map: &mut HashMap<String, FileStatus>, files: &[WorkingFile]| {
            for f in files {
                map.insert(f.path.clone(), f.effective_status());
            }
        };
        collect(&mut map, &status.conflicts);
        collect(&mut map, &status.tracked);
        collect(&mut map, &status.untracked);
        for f in &status.untracked {
            untracked.insert(f.path.clone());
        }
        for f in &status.tracked {
            if f.index_status.is_some() {
                staged.insert(f.path.clone());
            }
        }
        for f in &status.conflicts {
            conflicts.insert(f.path.clone());
        }

        self.git_status_by_relpath = map;
        self.untracked_relpaths = untracked;
        self.staged_relpaths = staged;
        self.conflict_relpaths = conflicts;
        self.working_tree_status = Some(status);
    }

    fn toggle_expand(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.expanded_paths.contains(&path) {
            self.expanded_paths.remove(&path);
        } else {
            self.expanded_paths.insert(path.clone());
            if !self.loaded_children.contains_key(&path) {
                self.load_directory(path, cx);
            }
        }
        cx.notify();
    }

    fn rel_path(&self, abs: &Path) -> Option<String> {
        abs.strip_prefix(&self.project_path)
            .ok()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
    }

    /// Rollup status for a directory: any child under this prefix.
    fn dir_rollup(&self, dir_rel: &str) -> Option<FileStatus> {
        let prefix = if dir_rel.is_empty() {
            String::new()
        } else {
            format!("{}/", dir_rel)
        };
        let mut has_modified = false;
        let mut has_added = false;
        for (path, status) in &self.git_status_by_relpath {
            if !prefix.is_empty() && !path.starts_with(&prefix) {
                continue;
            }
            match status {
                FileStatus::Added | FileStatus::Untracked => has_added = true,
                _ => has_modified = true,
            }
            if has_modified {
                return Some(FileStatus::Modified);
            }
        }
        if has_modified {
            Some(FileStatus::Modified)
        } else if has_added {
            Some(FileStatus::Added)
        } else {
            None
        }
    }

    pub fn set_context_menu_target(&mut self, path: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.context_menu_target != path {
            self.context_menu_target = path;
            cx.notify();
        }
    }

    pub fn clear_context_menu_target(&mut self, cx: &mut Context<Self>) {
        if self.context_menu_target.is_some() {
            self.context_menu_target = None;
            cx.notify();
        }
    }

    // ===== inline input (rename / new file / new folder) =====

    pub fn start_rename(&mut self, target: PathBuf, cx: &mut Context<Self>) {
        let current_name = target
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let is_dir = target.is_dir();
        let input = cx.new(|cx| {
            let mut s = SimpleInputState::new(cx)
                .placeholder(if is_dir {
                    "Folder name..."
                } else {
                    "File name..."
                })
                .default_value(&current_name);
            s.select_all(cx);
            s
        });
        self.active_input = Some(ActiveInput {
            mode: InputMode::Rename { target },
            input,
            needs_focus: true,
        });
        cx.notify();
    }

    pub fn start_new_file(&mut self, parent: PathBuf, cx: &mut Context<Self>) {
        self.ensure_expanded(&parent, cx);
        let input = cx.new(|cx| SimpleInputState::new(cx).placeholder("File name..."));
        self.active_input = Some(ActiveInput {
            mode: InputMode::NewFile { parent },
            input,
            needs_focus: true,
        });
        cx.notify();
    }

    pub fn start_new_folder(&mut self, parent: PathBuf, cx: &mut Context<Self>) {
        self.ensure_expanded(&parent, cx);
        let input = cx.new(|cx| SimpleInputState::new(cx).placeholder("Folder name..."));
        self.active_input = Some(ActiveInput {
            mode: InputMode::NewFolder { parent },
            input,
            needs_focus: true,
        });
        cx.notify();
    }

    fn ensure_expanded(&mut self, parent: &Path, cx: &mut Context<Self>) {
        if parent != self.project_path && !self.expanded_paths.contains(parent) {
            self.expanded_paths.insert(parent.to_path_buf());
        }
        if !self.loaded_children.contains_key(parent) {
            self.load_directory(parent.to_path_buf(), cx);
        }
    }

    fn cancel_input_action(
        &mut self,
        _: &ExplorerInputCancel,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_input.is_some() {
            self.active_input = None;
            cx.notify();
        }
    }

    fn commit_input_action(
        &mut self,
        _: &ExplorerInputConfirm,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(active) = self.active_input.take() else {
            return;
        };
        let raw = active.input.read(cx).value().trim().to_string();
        if raw.is_empty() {
            cx.notify();
            return;
        }
        match active.mode {
            InputMode::Rename { target } => {
                let Some(parent) = target.parent().map(|p| p.to_path_buf()) else {
                    return;
                };
                let new_path = parent.join(&raw);
                let patch_dirs = vec![parent.clone()];
                cx.spawn(async move |this, cx| {
                    let (src, dst) = (target.clone(), new_path.clone());
                    let result = smol::unblock(move || fs_ops::rename(&src, &dst)).await;
                    let _ = this.update(cx, |this, cx| {
                        if let Err(msg) = result {
                            log::warn!("rename failed: {msg}");
                        }
                        this.patch_paths(&patch_dirs, cx);
                    });
                })
                .detach();
            }
            InputMode::NewFile { parent } => {
                let path = parent.join(&raw);
                let patch_dirs = vec![parent.clone()];
                cx.spawn(async move |this, cx| {
                    let p = path.clone();
                    let result = smol::unblock(move || fs_ops::create_file(&p)).await;
                    let _ = this.update(cx, |this, cx| {
                        if let Err(msg) = result {
                            log::warn!("create_file failed: {msg}");
                        }
                        this.patch_paths(&patch_dirs, cx);
                    });
                })
                .detach();
            }
            InputMode::NewFolder { parent } => {
                let path = parent.join(&raw);
                let patch_dirs = vec![parent.clone()];
                cx.spawn(async move |this, cx| {
                    let p = path.clone();
                    let result = smol::unblock(move || fs_ops::create_folder(&p)).await;
                    let _ = this.update(cx, |this, cx| {
                        if let Err(msg) = result {
                            log::warn!("create_folder failed: {msg}");
                        }
                        this.patch_paths(&patch_dirs, cx);
                    });
                })
                .detach();
            }
        }
        cx.notify();
    }
}

impl Render for FileExplorer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);

        // Pull focus into the inline input the first render after start_*.
        if let Some(active) = self.active_input.as_mut()
            && active.needs_focus
        {
            let fh = active.input.read(cx).focus_handle(cx);
            window.focus(&fh, cx);
            active.needs_focus = false;
        }

        // Flatten visible tree into rows by recursive expansion.
        let mut rows: Vec<Row> = Vec::new();
        let ghost_parent = match self.active_input.as_ref().map(|a| &a.mode) {
            Some(InputMode::NewFile { parent }) | Some(InputMode::NewFolder { parent }) => {
                Some(parent.clone())
            }
            _ => None,
        };
        let ghost_is_folder = matches!(
            self.active_input.as_ref().map(|a| &a.mode),
            Some(InputMode::NewFolder { .. })
        );
        collect_rows(
            self.project_path.clone(),
            0,
            &self.loaded_children,
            &self.expanded_paths,
            ghost_parent.as_deref(),
            ghost_is_folder,
            &mut rows,
        );

        let broker = self.request_broker.clone();
        let project_path = self.project_path.clone();
        let has_active_input = self.active_input.is_some();

        let mut container = div()
            .id("file-explorer-scroll")
            .flex_1()
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll_handle)
            .bg(rgb(t.bg_secondary))
            .when(has_active_input, |d| {
                d.key_context("ExplorerInput")
                    .on_action(cx.listener(Self::commit_input_action))
                    .on_action(cx.listener(Self::cancel_input_action))
            })
            // Empty-area right-click: open context menu with project root target
            .on_mouse_down(MouseButton::Right, {
                let broker = broker.clone();
                let project_path = project_path.clone();
                move |event: &MouseDownEvent, _window, cx| {
                    let has_clipboard = cx
                        .try_global::<ExplorerClipboard>()
                        .map(|c| c.is_set())
                        .unwrap_or(false);
                    broker.update(cx, |b, cx| {
                        b.push_overlay_request(
                            OverlayRequest::ExplorerContextMenu {
                                kind: ExplorerKind::Empty,
                                path: project_path.clone(),
                                parent_dir: project_path.clone(),
                                has_clipboard,
                                position: event.position,
                            },
                            cx,
                        );
                    });
                }
            });

        for row in rows {
            container = container.child(self.render_row(row, &t, cx));
        }
        container
    }
}

#[derive(Clone)]
struct Row {
    entry: Option<DirEntry>, // None → ghost input row
    level: usize,
    is_expanded: bool,
    ghost_is_folder: bool,
}

fn collect_rows(
    dir: PathBuf,
    level: usize,
    loaded: &HashMap<PathBuf, Vec<DirEntry>>,
    expanded: &HashSet<PathBuf>,
    ghost_parent: Option<&Path>,
    ghost_is_folder: bool,
    out: &mut Vec<Row>,
) {
    let Some(entries) = loaded.get(&dir) else {
        // Even if no entries loaded, still drop a ghost row when this is the
        // ghost parent (e.g. empty folder getting a new file).
        if ghost_parent == Some(dir.as_path()) {
            out.push(Row {
                entry: None,
                level,
                is_expanded: false,
                ghost_is_folder,
            });
        }
        return;
    };
    // Ghost at the top of the folder so it's visible without scrolling.
    if ghost_parent == Some(dir.as_path()) {
        out.push(Row {
            entry: None,
            level,
            is_expanded: false,
            ghost_is_folder,
        });
    }
    for entry in entries {
        let is_expanded = entry.is_dir && expanded.contains(&entry.path);
        out.push(Row {
            entry: Some(entry.clone()),
            level,
            is_expanded,
            ghost_is_folder: false,
        });
        if is_expanded {
            collect_rows(
                entry.path.clone(),
                level + 1,
                loaded,
                expanded,
                ghost_parent,
                ghost_is_folder,
                out,
            );
        }
    }
}

impl FileExplorer {
    fn render_row(
        &self,
        row: Row,
        t: &notmux_core::theme::ThemeColors,
        cx: &Context<Self>,
    ) -> AnyElement {
        // Ghost row: inline input for NewFile / NewFolder
        let Some(entry) = row.entry.clone() else {
            return self.render_ghost_row(row, t, cx).into_any_element();
        };

        let indent_px = 8.0 + (row.level as f32) * 20.0;
        let abs_path = entry.path.clone();
        let is_dir = entry.is_dir;
        let rel = self.rel_path(&abs_path).unwrap_or_default();

        // Are we renaming this row?
        let is_renaming = matches!(
            &self.active_input,
            Some(ActiveInput {
                mode: InputMode::Rename { target },
                ..
            }) if target == &abs_path
        );

        let effective_status: Option<FileStatus> = if is_dir {
            self.dir_rollup(&rel)
        } else {
            self.git_status_by_relpath.get(&rel).copied()
        };

        let (name_color, badge): (u32, Option<(String, u32)>) = match effective_status {
            Some(FileStatus::Added) | Some(FileStatus::Untracked) => {
                let c = t.success;
                let b = if is_dir {
                    None
                } else {
                    Some(("U".to_string(), c))
                };
                (c, b)
            }
            Some(FileStatus::Deleted) => (
                t.error,
                if is_dir {
                    None
                } else {
                    Some(("D".to_string(), t.error))
                },
            ),
            Some(FileStatus::Conflict) => (
                t.error,
                if is_dir {
                    None
                } else {
                    Some(("!".to_string(), t.error))
                },
            ),
            Some(_) => {
                let c = t.term_yellow;
                let b = if is_dir {
                    None
                } else {
                    Some(("M".to_string(), c))
                };
                (c, b)
            }
            None => (t.text_primary, None),
        };

        let dir_dot_color: Option<u32> = if is_dir {
            match effective_status {
                Some(FileStatus::Added) | Some(FileStatus::Untracked) => Some(t.success),
                Some(FileStatus::Modified) => Some(t.term_yellow),
                Some(FileStatus::Conflict) | Some(FileStatus::Deleted) => Some(t.error),
                _ => None,
            }
        } else {
            None
        };

        let icon_path = "icons/folder.svg";
        let row_path = abs_path.clone();
        let broker = self.request_broker.clone();
        let click_broker = broker.clone();
        let project_root = self.project_path.clone();
        let is_context_target = self.context_menu_target.as_deref() == Some(abs_path.as_path());

        let name_element: AnyElement = if is_renaming {
            let Some(input) = self.active_input.as_ref().map(|a| a.input.clone()) else {
                return div().into_any_element();
            };
            div()
                .flex_1()
                .min_w_0()
                .child(SimpleInput::new(&input))
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_w_0()
                .text_size(ui_text_md(cx))
                .text_color(rgb(name_color))
                .text_ellipsis()
                .overflow_hidden()
                .child(entry.name.clone())
                .into_any_element()
        };

        div()
            .id(ElementId::Name(format!("fx-row-{}", rel).into()))
            .flex()
            .flex_row()
            .items_center()
            .w_full()
            .h(px(32.0))
            .pl(px(indent_px))
            .pr(px(8.0))
            .gap(px(8.0))
            .cursor_pointer()
            .when(is_context_target, |d| d.bg(rgb(t.bg_hover)))
            .when(!is_context_target, |d| d.hover(|s| s.bg(rgb(t.bg_hover))))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_click(cx.listener(move |this, _, _window, cx| {
                if is_dir {
                    this.toggle_expand(row_path.clone(), cx);
                } else {
                    click_broker.update(cx, |b, cx| {
                        b.push_overlay_request(
                            OverlayRequest::MainFileViewer {
                                project_id: this.project_id.clone(),
                                file: rel.clone(),
                            },
                            cx,
                        );
                    });
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener({
                    let abs_path = abs_path.clone();
                    move |this, event: &MouseDownEvent, _window, cx| {
                        cx.stop_propagation();
                        let (kind, parent_dir) = if is_dir {
                            (ExplorerKind::Folder, abs_path.clone())
                        } else {
                            let parent = abs_path
                                .parent()
                                .map(|p| p.to_path_buf())
                                .unwrap_or_else(|| project_root.clone());
                            (ExplorerKind::File, parent)
                        };
                        let has_clipboard = cx
                            .try_global::<ExplorerClipboard>()
                            .map(|c| c.is_set())
                            .unwrap_or(false);
                        this.context_menu_target = Some(abs_path.clone());
                        broker.update(cx, |b, cx| {
                            b.push_overlay_request(
                                OverlayRequest::ExplorerContextMenu {
                                    kind,
                                    path: abs_path.clone(),
                                    parent_dir,
                                    has_clipboard,
                                    position: event.position,
                                },
                                cx,
                            );
                        });
                        cx.notify();
                    }
                }),
            )
            .child(
                div()
                    .w(px(16.0))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(is_dir, |d| {
                        let p = if row.is_expanded {
                            "icons/chevron-down.svg"
                        } else {
                            "icons/chevron-right.svg"
                        };
                        d.child(svg().path(p).size(px(14.0)).text_color(rgb(t.text_muted)))
                    }),
            )
            .child(if is_dir {
                div()
                    .flex_shrink_0()
                    .w(px(18.0))
                    .h(px(18.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        svg()
                            .path(icon_path)
                            .size(px(18.0))
                            .text_color(rgb(name_color)),
                    )
                    .into_any_element()
            } else {
                vscode_file_icon_sized_with_options(
                    &entry.name,
                    px(18.0),
                    t,
                    self.monochrome_icons,
                    cx,
                )
                .into_any_element()
            })
            .child(name_element)
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(18.0))
                    .flex()
                    .items_center()
                    .justify_end()
                    .when_some(badge, |d, (label, color)| {
                        d.child(
                            div()
                                .text_size(px(12.0))
                                .text_color(rgb(color))
                                .child(label),
                        )
                    })
                    .when_some(dir_dot_color, |d, color| {
                        d.child(div().w(px(6.0)).h(px(6.0)).rounded_full().bg(rgb(color)))
                    }),
            )
            .into_any_element()
    }

    fn render_ghost_row(
        &self,
        row: Row,
        t: &notmux_core::theme::ThemeColors,
        _cx: &Context<Self>,
    ) -> Div {
        let indent_px = 8.0 + ((row.level + 1) as f32) * 20.0;
        let icon = if row.ghost_is_folder {
            "icons/folder.svg"
        } else {
            "icons/file.svg"
        };
        let input = match self.active_input.as_ref() {
            Some(a) => a.input.clone(),
            None => return div(),
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .w_full()
            .h(px(32.0))
            .pl(px(indent_px))
            .pr(px(8.0))
            .gap(px(8.0))
            .child(div().w(px(16.0)).flex_shrink_0())
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(18.0))
                    .h(px(18.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        svg()
                            .path(icon)
                            .size(px(18.0))
                            .text_color(rgb(t.text_muted)),
                    ),
            )
            .child(div().flex_1().min_w_0().child(SimpleInput::new(&input)))
    }
}
