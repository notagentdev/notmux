//! Workspace File Explorer panel — lazy-loading directory tree with
//! per-file git status decoration. Mirrors the visual model of Vryn's
//! `FileTreeView` in GPUI. Right-click on a file row opens the same
//! git file context menu as the git panel.

use gpui::prelude::*;
use gpui::*;
use okena_files::dir_listing::{list_directory, DirEntry};
use okena_files::theme::theme;
use okena_git::{FileStatus, WorkingFile, WorkingTreeStatus};
use okena_ui::tokens::ui_text_sm;
use okena_workspace::request_broker::RequestBroker;
use okena_workspace::requests::OverlayRequest;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

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
            let parent = p.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| p.clone());
            if parent.starts_with(&self.project_path)
                && self.loaded_children.contains_key(&parent)
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
        cx.spawn(async move |this, cx| {
            let entries =
                smol::unblock(move || list_directory(&owned, false)).await;
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
            let status = smol::unblock(move || {
                okena_git::get_working_tree_status(&project_path)
            })
            .await;
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
}

impl Render for FileExplorer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);

        // Flatten visible tree into rows by recursive expansion.
        let mut rows: Vec<TreeRow> = Vec::new();
        collect_rows(
            self.project_path.clone(),
            0,
            &self.loaded_children,
            &self.expanded_paths,
            &mut rows,
        );

        let mut container = div()
            .id("file-explorer-scroll")
            .flex_1()
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll_handle)
            .bg(rgb(t.bg_primary));

        for row in rows {
            container = container.child(self.render_row(row, &t, cx));
        }
        container
    }
}

#[derive(Clone)]
struct TreeRow {
    entry: DirEntry,
    level: usize,
    is_expanded: bool,
}

fn collect_rows(
    dir: PathBuf,
    level: usize,
    loaded: &HashMap<PathBuf, Vec<DirEntry>>,
    expanded: &HashSet<PathBuf>,
    out: &mut Vec<TreeRow>,
) {
    let Some(entries) = loaded.get(&dir) else {
        return;
    };
    for entry in entries {
        let is_expanded = entry.is_dir && expanded.contains(&entry.path);
        out.push(TreeRow {
            entry: entry.clone(),
            level,
            is_expanded,
        });
        if is_expanded {
            collect_rows(entry.path.clone(), level + 1, loaded, expanded, out);
        }
    }
}

impl FileExplorer {
    fn render_row(
        &self,
        row: TreeRow,
        t: &okena_core::theme::ThemeColors,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let indent_px = 8.0 + (row.level as f32) * 16.0;
        let entry = row.entry.clone();
        let abs_path = entry.path.clone();
        let is_dir = entry.is_dir;
        let rel = self.rel_path(&abs_path).unwrap_or_default();

        // Resolve git status (file direct lookup, dir rollup).
        let effective_status: Option<FileStatus> = if is_dir {
            self.dir_rollup(&rel)
        } else {
            self.git_status_by_relpath.get(&rel).copied()
        };

        // Color + badge per Vryn spec.
        let (name_color, badge): (u32, Option<(String, u32)>) = match effective_status {
            Some(FileStatus::Added) | Some(FileStatus::Untracked) => {
                let c = t.success;
                let b = if is_dir { None } else { Some(("U".to_string(), c)) };
                (c, b)
            }
            Some(FileStatus::Deleted) => (t.error, if is_dir { None } else { Some(("D".to_string(), t.error)) }),
            Some(FileStatus::Conflict) => (t.error, if is_dir { None } else { Some(("!".to_string(), t.error)) }),
            Some(_) => {
                let c = t.term_yellow;
                let b = if is_dir { None } else { Some(("M".to_string(), c)) };
                (c, b)
            }
            None => (t.text_primary, None),
        };

        // For directories with any dirty child but no per-file badge, show a dot.
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

        // Icon path (minimal mapping; folder vs file).
        let icon_path = if is_dir {
            "icons/folder.svg"
        } else {
            icon_for_extension(&entry.name)
        };

        // Right-click context-menu payload.
        let is_untracked = self.untracked_relpaths.contains(&rel);
        let is_staged = self.staged_relpaths.contains(&rel);
        let is_conflict = self.conflict_relpaths.contains(&rel);
        let broker = self.request_broker.clone();
        let project_id = self.project_id.clone();
        let rel_for_menu = rel.clone();

        let row_path = abs_path.clone();

        div()
            .id(ElementId::Name(format!("fx-row-{}", rel).into()))
            .flex()
            .flex_row()
            .items_center()
            .w_full()
            .h(px(24.0))
            .pl(px(indent_px))
            .pr(px(8.0))
            .gap(px(4.0))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(t.bg_hover)))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_click(cx.listener(move |this, _, _window, cx| {
                if is_dir {
                    this.toggle_expand(row_path.clone(), cx);
                }
                // Files: no-op (click-to-open is out of scope for this iteration).
            }))
            // Right-click: reuse the git file context menu.
            .when(!is_dir, |d| {
                let broker = broker.clone();
                let project_id = project_id.clone();
                let rel_for_menu = rel_for_menu.clone();
                d.on_mouse_down(MouseButton::Right, move |event: &MouseDownEvent, _window, cx| {
                    cx.stop_propagation();
                    let pid = project_id.clone();
                    let fp = rel_for_menu.clone();
                    broker.update(cx, |b, cx| {
                        b.push_overlay_request(
                            OverlayRequest::GitFileContextMenu {
                                project_id: pid,
                                file_path: fp,
                                is_staged,
                                is_untracked,
                                is_conflict,
                                position: event.position,
                            },
                            cx,
                        );
                    });
                })
            })
            // Chevron (dirs only)
            .child(
                div()
                    .w(px(12.0))
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
                        d.child(
                            svg()
                                .path(p)
                                .size(px(10.0))
                                .text_color(rgb(t.text_muted)),
                        )
                    }),
            )
            // Icon
            .child(
                div().flex_shrink_0().w(px(14.0)).h(px(14.0)).flex().items_center().justify_center().child(
                    svg()
                        .path(icon_path)
                        .size(px(14.0))
                        .text_color(rgb(name_color)),
                ),
            )
            // Name
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(ui_text_sm(cx))
                    .text_color(rgb(name_color))
                    .text_ellipsis()
                    .overflow_hidden()
                    .child(entry.name.clone()),
            )
            // Git decoration (badge or dot) — right-aligned in a fixed slot.
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(14.0))
                    .flex()
                    .items_center()
                    .justify_end()
                    .when_some(badge, |d, (label, color)| {
                        d.child(
                            div()
                                .text_size(px(10.0))
                                .text_color(rgb(color))
                                .child(label),
                        )
                    })
                    .when_some(dir_dot_color, |d, color| {
                        d.child(
                            div()
                                .w(px(6.0))
                                .h(px(6.0))
                                .rounded_full()
                                .bg(rgb(color)),
                        )
                    }),
            )
    }
}

fn icon_for_extension(name: &str) -> &'static str {
    let lower = name.to_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    match ext {
        "rs" => "icons/file.svg",
        "toml" | "yaml" | "yml" | "json" | "lock" => "icons/file.svg",
        "md" | "markdown" => "icons/file.svg",
        "svg" | "png" | "jpg" | "jpeg" | "gif" | "webp" => "icons/file.svg",
        _ => "icons/file.svg",
    }
}
