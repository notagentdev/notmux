//! GitHeader — self-contained GPUI entity for git status display,
//! diff popover, and commit log popover in the project column header.
//!
//! Extracted from `ProjectColumn` to keep that view thin.

use notmux_core::types::DiffMode;
use notmux_files::code_view::{
    ScrollbarDrag, get_scrollbar_geometry, start_scrollbar_drag, update_scrollbar_drag,
};
use notmux_git::{
    CommitLogEntry, FileDiffSummary, FileStatus, GitStatus, WorkingFile, WorkingTreeStatus,
};
use notmux_workspace::request_broker::RequestBroker;
use notmux_workspace::requests::OverlayRequest;
use notmux_workspace::state::Workspace;

use crate::diff_hscrollbar::{DiffHScrollbar, HSCROLLBAR_HEIGHT};
use crate::diff_viewer::DiffViewer;
use crate::diff_viewer::provider::GitProvider;
use crate::list_scrollbar::ListScrollbar;
use crate::project_header;
use crate::settings::git_settings;
use std::collections::{HashMap, HashSet};

use gpui::prelude::*;
use gpui::*;
use gpui_component::tooltip::Tooltip;
use gpui_component::{h_flex, v_flex};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use notmux_core::theme::ThemeColors;
use notmux_ui::tokens::{ui_text_md, ui_text_ms, ui_text_sm};
use notmux_ui::vscode_icon::vscode_file_icon_with_options;

/// Delay before showing diff summary popover (ms)
const HOVER_DELAY_MS: u64 = 400;

/// Front-elide a directory string to `max_chars`, keeping the tail (the deepest
/// folders, which are the most informative) and prefixing an ellipsis. Returns
/// the input unchanged when it already fits. GPUI's built-in `text_ellipsis`
/// only truncates at the end, so path tails have to be preserved manually.
fn elide_dir_front(dir: &str, max_chars: usize) -> String {
    let count = dir.chars().count();
    if count <= max_chars || max_chars == 0 {
        return dir.to_string();
    }
    let keep = max_chars.saturating_sub(1).max(1); // leave room for the ellipsis
    let tail: String = dir.chars().skip(count - keep).collect();
    format!("…{tail}")
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum BranchPickerTarget {
    /// Picking branch to show the commit log for
    #[default]
    Graph,
    /// Picking base branch for compare
    CompareBase,
    /// Picking head branch for compare
    CompareHead,
}

/// Section a row belongs to in the commit tab.
#[derive(Clone, Copy, Debug, PartialEq)]
enum FileSectionKind {
    Conflicts,
    Tracked,
    Untracked,
}

/// Flattened row model for the commit-tab list. The whole tab (section headers,
/// file rows, and — for expanded files — diff lines / context expanders) is one
/// viewported `list()`, so the diff shows at full height with only the visible
/// rows realized (Warp-style virtualization) while every row keeps its real
/// rendering and handlers.
#[derive(Clone)]
enum CommitRow {
    Header(FileSectionKind, bool),
    File {
        file: WorkingFile,
        is_untracked: bool,
    },
    /// One diff line or context-expander of `path`'s inline diff, by item index
    /// into the owning `DiffViewer`.
    DiffItem {
        path: String,
        item_index: usize,
    },
    /// A status note under a file (loading / empty / error).
    DiffNote {
        text: SharedString,
    },
    /// Horizontal scrollbar row at the bottom of `path`'s expanded diff.
    DiffHScrollbar {
        path: String,
    },
    /// "Stashed changes" header row (with a close button) atop the stash view.
    StashHeader,
    /// A file in the stash view — filename + stats + disclosure chevron.
    StashFile {
        path: String,
        added: usize,
        removed: usize,
    },
}

/// One file in the stash view (path + diff stats).
#[derive(Clone)]
struct StashFileRow {
    path: String,
    added: usize,
    removed: usize,
}

/// The git ref for the most recent stash, diffed like a commit.
const STASH_REF: &str = "stash@{0}";

/// Whether two commit rows render identically — used to compute a minimal list
/// splice (common prefix/suffix) so the scroll position is preserved across
/// structural changes. Files compare by the fields that affect their row.
fn rows_eq(a: &CommitRow, b: &CommitRow) -> bool {
    match (a, b) {
        (CommitRow::Header(k1, c1), CommitRow::Header(k2, c2)) => k1 == k2 && c1 == c2,
        (
            CommitRow::File { file: f1, is_untracked: u1 },
            CommitRow::File { file: f2, is_untracked: u2 },
        ) => {
            u1 == u2
                && f1.path == f2.path
                && f1.added == f2.added
                && f1.removed == f2.removed
                && f1.effective_status() == f2.effective_status()
                && f1.is_fully_staged() == f2.is_fully_staged()
                && f1.is_partially_staged() == f2.is_partially_staged()
                && f1.has_conflict() == f2.has_conflict()
        }
        (
            CommitRow::DiffItem { path: p1, item_index: i1 },
            CommitRow::DiffItem { path: p2, item_index: i2 },
        ) => p1 == p2 && i1 == i2,
        (CommitRow::DiffNote { text: t1 }, CommitRow::DiffNote { text: t2 }) => t1 == t2,
        (CommitRow::DiffHScrollbar { path: p1 }, CommitRow::DiffHScrollbar { path: p2 }) => {
            p1 == p2
        }
        (CommitRow::StashHeader, CommitRow::StashHeader) => true,
        (
            CommitRow::StashFile { path: p1, added: a1, removed: r1 },
            CommitRow::StashFile { path: p2, added: a2, removed: r2 },
        ) => p1 == p2 && a1 == a2 && r1 == r2,
        _ => false,
    }
}

/// Which tab is active in the git panel.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum GitPanelTab {
    /// Commit tab — staging + commit message + remote operations
    #[default]
    Commit,
    /// Commit graph / history view
    History,
}

/// Running remote operation (for UI spinner).
#[derive(Clone, Copy, Debug, PartialEq)]
enum RemoteOp {
    Fetch,
    Pull,
    Push,
}

/// Self-contained GPUI entity managing git status display, diff summary
/// popover, and commit log popover.
pub struct GitHeader {
    project_id: String,
    request_broker: Entity<RequestBroker>,
    git_provider: Arc<dyn GitProvider>,
    /// Used to clear the workspace focus claim when the user clicks into
    /// the commit-message input — otherwise the focused TerminalPane
    /// re-grabs GPUI focus on the next render and the user can't type.
    workspace: Entity<Workspace>,

    /// Current branch from git watcher (updated externally before rendering).
    current_branch: Option<String>,

    // ── Diff popover state ──────────────────────────────────────────
    diff_popover_visible: bool,
    diff_file_summaries: Vec<FileDiffSummary>,
    hover_token: Arc<AtomicU64>,
    diff_stats_bounds: Bounds<Pixels>,

    // ── Commit log state ────────────────────────────────────────────
    commit_log_visible: bool,
    /// The provider's path is not a git repository (drives the panel's
    /// "No repo found" state; kept current by loads and refreshes).
    repo_missing: bool,
    commit_log_entries: Vec<CommitLogEntry>,
    commit_log_loading: bool,
    commit_log_has_more: bool,
    commit_log_branch: Option<String>,
    commit_log_branches: Vec<String>,
    commit_log_branch_picker: bool,
    commit_log_branch_filter: String,
    commit_log_compare_mode: bool,
    commit_log_compare_base: Option<String>,
    commit_log_compare_head: Option<String>,
    commit_log_picker_target: BranchPickerTarget,
    /// Scroll handle for the History tab's uniform commit list. Rows are all
    /// `COMMIT_ROW_H` tall, so the list virtualizes without measuring —
    /// only visible rows are ever built.
    history_scroll: UniformListScrollHandle,
    /// Active scrollbar-thumb drag on the history list.
    history_scrollbar_drag: Option<ScrollbarDrag>,

    /// Active tab in the git panel (Commit / Changes / History)
    active_tab: GitPanelTab,

    // ── Commit tab state ───────────────────────────────────────────
    /// Working tree status (files + branch + ahead/behind)
    working_tree_status: Option<WorkingTreeStatus>,
    /// Loading flag for working tree status
    working_tree_loading: bool,
    /// Commit message input state
    commit_message_input: Entity<notmux_ui::simple_input::SimpleInputState>,
    /// Commit options
    commit_options_amend: bool,
    commit_options_signoff: bool,
    /// Running commit
    committing: bool,
    /// Running remote operation
    remote_op_running: Option<RemoteOp>,
    /// Section collapse state
    conflicts_collapsed: bool,
    tracked_collapsed: bool,
    untracked_collapsed: bool,
    /// Last operation error message (shown as toast/inline)
    last_error: Option<String>,
    /// Viewported list state for the commit-tab list (headers + file rows +
    /// inline diff rows). One list for the whole tab → virtualized, scrolls as
    /// one, no per-diff scrollbox.
    commit_list_state: ListState,
    /// Flattened row model backing `commit_list_state`; rebuilt on structural
    /// changes (status refresh, expand/collapse, diff load).
    commit_rows: Vec<CommitRow>,
    /// Pixel width of the commit list (captured during render), pushed into the
    /// inline diff viewers so their horizontal-scroll math is correct.
    diff_viewport_width: f32,
    /// When `Some`, the commit tab shows the latest stash's files (read-only)
    /// instead of the working tree; each row's inline diff loads from the stash
    /// commit. `None` = normal working-tree view.
    stash_files: Option<Vec<StashFileRow>>,

    /// Bounds of the panel-header three-dots button, used to anchor
    /// the overflow popover.
    overflow_button_bounds: Bounds<Pixels>,
    /// Whether at least one stash entry exists. Refreshed alongside
    /// the working-tree status. Drives Stash Pop / Show Stash enabled
    /// state in the overflow menu.
    has_stash: bool,
    /// Running soft reset of the last commit.
    uncommitting: bool,
    /// Commit footer options menu state (Amend / Sign-off).
    commit_options_menu_visible: bool,
    commit_options_menu_anchor: Option<Point<Pixels>>,
    /// Header three-dots overflow menu state (Stage/Unstage/Stash/Discard).
    overflow_menu_visible: bool,
    overflow_menu_anchor: Option<Point<Pixels>>,

    // ── Inline diff state (expand a file's diff directly in the list) ──
    /// Paths whose diff is currently expanded inline, in click order.
    inline_expanded: HashSet<String>,
    /// Embedded diff-viewer entities (the real notmux diff rendering), one per
    /// expanded file, created lazily and kept alive while expanded.
    inline_viewers: HashMap<String, Entity<DiffViewer>>,
}

const COMMIT_PAGE_SIZE: usize = 50;

impl GitHeader {
    pub fn new(
        project_id: String,
        request_broker: Entity<RequestBroker>,
        git_provider: Arc<dyn GitProvider>,
        workspace: Entity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            project_id,
            request_broker,
            git_provider,
            workspace,
            current_branch: None,
            diff_popover_visible: false,
            diff_file_summaries: Vec::new(),
            hover_token: Arc::new(AtomicU64::new(0)),
            diff_stats_bounds: Bounds::default(),
            commit_log_visible: false,
            repo_missing: false,
            commit_log_entries: Vec::new(),
            commit_log_loading: false,
            commit_log_has_more: false,
            commit_log_branch: None,
            commit_log_branches: Vec::new(),
            commit_log_branch_picker: false,
            commit_log_branch_filter: String::new(),
            commit_log_compare_mode: false,
            commit_log_compare_base: None,
            commit_log_compare_head: None,
            commit_log_picker_target: BranchPickerTarget::default(),
            active_tab: GitPanelTab::default(),
            working_tree_status: None,
            working_tree_loading: false,
            commit_message_input: {
                cx.new(|cx| {
                    notmux_ui::simple_input::SimpleInputState::new(cx)
                        .placeholder("Commit message")
                        .multiline()
                })
            },
            commit_options_amend: false,
            commit_options_signoff: false,
            committing: false,
            remote_op_running: None,
            conflicts_collapsed: false,
            tracked_collapsed: false,
            untracked_collapsed: false,
            last_error: None,
            // `measure_all` so the scrollbar is exact immediately (the list
            // measures every row once per structural change instead of lazily
            // while scrolling — otherwise the thumb shrinks as rows are
            // discovered). Re-measure happens only on splice, not per frame.
            commit_list_state: ListState::new(0, ListAlignment::Top, px(400.0)).measure_all(),
            history_scroll: UniformListScrollHandle::new(),
            history_scrollbar_drag: None,
            commit_rows: Vec::new(),
            diff_viewport_width: 0.0,
            stash_files: None,
            overflow_button_bounds: Bounds::default(),
            has_stash: false,
            uncommitting: false,
            commit_options_menu_visible: false,
            commit_options_menu_anchor: None,
            overflow_menu_visible: false,
            overflow_menu_anchor: None,
            inline_expanded: HashSet::new(),
            inline_viewers: HashMap::new(),
        }
    }

    /// Toggle the inline diff for a file. Expanding lazily builds a real
    /// embedded `DiffViewer` for the file (working-tree changes, or staged
    /// changes when the file is fully staged) — the exact notmux diff rendering.
    fn toggle_inline_diff(&mut self, path: String, staged: bool, cx: &mut Context<Self>) {
        if self.inline_expanded.remove(&path) {
            self.rebuild_commit_rows(cx);
            return;
        }
        self.inline_expanded.insert(path.clone());
        if !self.inline_viewers.contains_key(&path) {
            let provider = self.git_provider.clone();
            // Stash view diffs the stash commit; otherwise staged/working tree.
            let mode = if self.stash_files.is_some() {
                DiffMode::Commit(STASH_REF.to_string())
            } else if staged {
                DiffMode::Staged
            } else {
                DiffMode::WorkingTree
            };
            let select = path.clone();
            // Match the file-list font size so the whole panel is uniform.
            let font_size = f32::from(ui_text_md(cx));
            let is_dark = git_settings(cx).is_dark;
            let viewer = cx.new(|cx| {
                let mut v =
                    DiffViewer::new(provider, Some(select), Some(mode), None, None, None, cx);
                v.set_embedded(true);
                v.update_config(font_size, is_dark, cx);
                v
            });
            // The viewer loads/expands async and mutates its line count; rebuild
            // the row model (and resize the list) whenever it notifies.
            cx.observe(&viewer, |this, _, cx| this.rebuild_commit_rows(cx)).detach();
            // A hunk staged/unstaged from within the inline diff changes the
            // working-tree/index split — refresh the file list stats & state.
            cx.subscribe(&viewer, |this, _, event, cx| {
                if matches!(event, crate::diff_viewer::DiffViewerEvent::HunksChanged) {
                    this.refresh_working_tree_status(cx);
                }
            })
            .detach();
            self.inline_viewers.insert(path, viewer);
        }
        self.rebuild_commit_rows(cx);
    }

    /// Load the latest stash's file list and show it in the commit tab (read-only
    /// inline diffs from the stash commit). Reuses the whole inline-diff list.
    fn handle_show_stash(&mut self, cx: &mut Context<Self>) {
        // Reset inline state — viewers must be recreated with the stash mode.
        self.inline_expanded.clear();
        self.inline_viewers.clear();
        let provider = self.git_provider.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let files = smol::unblock(move || {
                match provider.get_diff(DiffMode::Commit(STASH_REF.to_string()), false) {
                    Ok(result) => result
                        .files
                        .iter()
                        .map(|f| StashFileRow {
                            path: f.display_name().to_string(),
                            added: f.lines_added,
                            removed: f.lines_removed,
                        })
                        .collect::<Vec<_>>(),
                    Err(_) => Vec::new(),
                }
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                this.stash_files = Some(files);
                this.rebuild_commit_rows(cx);
            });
        })
        .detach();
    }

    /// Leave the stash view and return to the working tree.
    fn close_stash(&mut self, cx: &mut Context<Self>) {
        self.stash_files = None;
        self.inline_expanded.clear();
        self.inline_viewers.clear();
        self.rebuild_commit_rows(cx);
    }

    /// Update the current branch name (from the git status watcher).
    /// Borrow the underlying git provider so callers (e.g. the overlay
    /// manager wiring up the stash list) can issue the same operations.
    pub fn git_provider(&self) -> Arc<dyn GitProvider> {
        self.git_provider.clone()
    }

    pub fn set_current_branch(&mut self, branch: Option<String>) {
        self.current_branch = branch;
    }

    // ── Diff popover ────────────────────────────────────────────────

    fn show_diff_popover(&mut self, cx: &mut Context<Self>) {
        if self.diff_popover_visible {
            return;
        }

        let token = self.hover_token.fetch_add(1, Ordering::SeqCst) + 1;
        let hover_token = self.hover_token.clone();
        let provider = self.git_provider.clone();

        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            smol::Timer::after(Duration::from_millis(HOVER_DELAY_MS)).await;

            if hover_token.load(Ordering::SeqCst) != token {
                return;
            }

            let summaries = smol::unblock(move || provider.get_diff_file_summary()).await;

            let _ = this.update(cx, |this, cx| {
                if hover_token.load(Ordering::SeqCst) == token && !summaries.is_empty() {
                    this.diff_file_summaries = summaries;
                    this.diff_popover_visible = true;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn hide_diff_popover(&mut self, cx: &mut Context<Self>) {
        let token = self.hover_token.fetch_add(1, Ordering::SeqCst) + 1;

        if !self.diff_popover_visible {
            return;
        }

        let hover_token = self.hover_token.clone();

        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            smol::Timer::after(Duration::from_millis(100)).await;

            if hover_token.load(Ordering::SeqCst) != token {
                return;
            }

            let _ = this.update(cx, |this, cx| {
                if hover_token.load(Ordering::SeqCst) == token && this.diff_popover_visible {
                    this.diff_popover_visible = false;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn latest_commit(&self) -> Option<&CommitLogEntry> {
        self.commit_log_entries.first()
    }

    // ── Commit log ──────────────────────────────────────────────────

    #[allow(dead_code)]
    fn toggle_commit_log(&mut self, cx: &mut Context<Self>) {
        if self.commit_log_visible {
            self.commit_log_visible = false;
            cx.notify();
            return;
        }
        self.diff_popover_visible = false;

        self.commit_log_visible = true;
        self.commit_log_loading = true;
        self.commit_log_entries.clear();
        self.commit_log_has_more = false;
        self.commit_log_branch = None;
        self.commit_log_branch_picker = false;
        self.commit_log_branch_filter.clear();
        self.commit_log_compare_mode = false;
        self.commit_log_compare_base = None;
        self.commit_log_compare_head = None;
        self.commit_log_picker_target = BranchPickerTarget::Graph;
        self.reset_history_scroll();
        cx.notify();

        let page = COMMIT_PAGE_SIZE;
        let provider = self.git_provider.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let (entries, branches) = smol::unblock(move || {
                let entries = provider.get_commit_log(0, page, None);
                let branches = provider.list_branches();
                (entries, branches)
            })
            .await;

            let _ = this.update(cx, |this, cx| {
                this.commit_log_loading = false;
                this.commit_log_has_more = entries.len() >= page;
                this.commit_log_entries = entries;
                this.commit_log_branches = branches;
                cx.notify();
            });
        })
        .detach();
    }

    /// Scroll the history list back to the top (fresh load, branch switch).
    fn reset_history_scroll(&mut self) {
        let state = self.history_scroll.0.borrow_mut();
        state.base_handle.set_offset(point(px(0.0), px(0.0)));
    }

    fn switch_commit_log_branch(&mut self, branch: Option<String>, cx: &mut Context<Self>) {
        self.commit_log_branch = branch.clone();
        self.commit_log_branch_picker = false;
        self.commit_log_branch_filter.clear();
        self.commit_log_loading = true;
        self.commit_log_entries.clear();
        self.commit_log_has_more = false;
        self.reset_history_scroll();
        cx.notify();

        let provider = self.git_provider.clone();
        let page = COMMIT_PAGE_SIZE;

        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let entries =
                smol::unblock(move || provider.get_commit_log(0, page, branch.as_deref())).await;

            let _ = this.update(cx, |this, cx| {
                this.commit_log_loading = false;
                this.commit_log_has_more = entries.len() >= page;
                this.commit_log_entries = entries;
                cx.notify();
            });
        })
        .detach();
    }

    /// Fetch the next page (`--skip=<loaded>`) and append it — earlier pages
    /// are never refetched, so cost stays constant as the log grows.
    fn load_more_commits(&mut self, cx: &mut Context<Self>) {
        if self.commit_log_loading || !self.commit_log_has_more {
            return;
        }

        self.commit_log_loading = true;
        cx.notify();

        let provider = self.git_provider.clone();
        let branch = self.commit_log_branch.clone();
        let skip = self.commit_log_entries.len();
        let page = COMMIT_PAGE_SIZE;

        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let entries =
                smol::unblock(move || provider.get_commit_log(skip, page, branch.as_deref()))
                    .await;

            let _ = this.update(cx, |this, cx| {
                this.commit_log_loading = false;
                this.commit_log_has_more = entries.len() >= page;
                this.commit_log_entries.extend(entries);
                cx.notify();
            });
        })
        .detach();
    }

    /// Hide the commit log.
    pub fn hide_commit_log(&mut self, cx: &mut Context<Self>) {
        if self.commit_log_visible {
            self.commit_log_visible = false;
            cx.notify();
        }
    }

    /// Re-shows the commit log WITHOUT reloading (data is kept). Hosts that
    /// embed the panel permanently (the notagent right panel) call this each
    /// render: internal flows (branch-compare, backdrop clicks) hide the log
    /// for the standalone overlay use-case, which would otherwise leave the
    /// embedded panel blank — "only the Git tab" visible.
    pub fn ensure_commit_log_visible(&mut self) {
        self.commit_log_visible = true;
    }

    /// Open the commit log (loads data, sets visible). Called by the git
    /// panel. Idempotent for visibility — but ALWAYS reloads data, because
    /// this is also the entry point used when switching between projects
    /// (previously the early-return-on-visible check meant switching back
    /// to a project whose log was still "visible" from a prior session
    /// silently skipped the reload, requiring a second switch to see data).
    pub fn open_commit_log(&mut self, cx: &mut Context<Self>) {
        self.diff_popover_visible = false;
        self.commit_log_visible = true;
        self.commit_log_loading = true;
        self.commit_log_entries.clear();
        self.commit_log_has_more = false;
        self.commit_log_branch = None;
        self.commit_log_branch_picker = false;
        self.commit_log_branch_filter.clear();
        self.commit_log_compare_mode = false;
        self.commit_log_compare_base = None;
        self.commit_log_compare_head = None;
        self.commit_log_picker_target = BranchPickerTarget::Graph;
        self.reset_history_scroll();
        cx.notify();

        let page = COMMIT_PAGE_SIZE;
        let provider = self.git_provider.clone();
        self.working_tree_loading = true;
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let (entries, branches, diff_summaries, wt_status, is_repo) =
                smol::unblock(move || {
                    let is_repo = provider.is_git_repo();
                    let entries = provider.get_commit_log(0, page, None);
                    let branches = provider.list_branches();
                    let diff_summaries = provider.get_diff_file_summary();
                    let wt_status = provider.get_working_tree_status();
                    (entries, branches, diff_summaries, wt_status, is_repo)
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                this.commit_log_loading = false;
                this.working_tree_loading = false;
                this.repo_missing = !is_repo;
                this.commit_log_has_more = entries.len() >= page;
                this.commit_log_entries = entries;
                this.commit_log_branches = branches;
                this.diff_file_summaries = diff_summaries;
                this.working_tree_status = Some(wt_status);
                this.rebuild_commit_rows(cx);
            });
        })
        .detach();
    }

    /// Local repo root, if this header's provider is a local one. Used by
    /// the notify-driven FS watcher to translate absolute paths to rel-paths.
    pub fn local_repo_root(&self) -> Option<std::path::PathBuf> {
        self.git_provider.local_repo_root().map(|p| p.to_path_buf())
    }

    /// Incrementally patch the working tree status for a set of rel-paths.
    /// Each path either gets updated in place, moved between sections, or
    /// removed if it's now clean. Falls back to a full refresh if the
    /// provider can't answer per-file queries (remote).
    pub fn patch_files(&mut self, rel_paths: Vec<String>, cx: &mut Context<Self>) {
        if rel_paths.is_empty() {
            return;
        }
        let provider = self.git_provider.clone();
        // Remote provider can't do per-file — fall back.
        if provider.local_repo_root().is_none() {
            self.refresh_working_tree_status(cx);
            return;
        }
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let results = smol::unblock(move || provider.get_file_statuses(&rel_paths)).await;
            let _ = this.update(cx, |this, cx| {
                this.apply_file_refresh(results);
                this.rebuild_commit_rows(cx);
            });
        })
        .detach();
    }

    fn apply_file_refresh(&mut self, results: Vec<notmux_git::FileStatusRefresh>) {
        let Some(status) = self.working_tree_status.as_mut() else {
            return;
        };
        let remove_by_path = |vec: &mut Vec<notmux_git::WorkingFile>, path: &str| {
            vec.retain(|f| f.path != path);
        };
        for r in results {
            let p = r.rel_path.as_str();
            remove_by_path(&mut status.conflicts, p);
            remove_by_path(&mut status.tracked, p);
            remove_by_path(&mut status.untracked, p);
            if let (Some(file), Some(section)) = (r.file, r.section) {
                match section {
                    notmux_git::FileSection::Conflict => status.conflicts.push(file),
                    notmux_git::FileSection::Tracked => status.tracked.push(file),
                    notmux_git::FileSection::Untracked => status.untracked.push(file),
                }
            }
        }

    }

    /// Refresh only the working tree status (after stage/unstage/commit).
    pub fn refresh_working_tree_status(&mut self, cx: &mut Context<Self>) {
        self.working_tree_loading = true;
        let provider = self.git_provider.clone();
        let provider2 = provider.clone();
        let provider3 = provider.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let (wt_status, diff_summaries, has_stash, is_repo) = smol::unblock(move || {
                let wt_status = provider.get_working_tree_status();
                let diff_summaries = provider2.get_diff_file_summary();
                let has_stash = provider3
                    .stash_list()
                    .map(|s| !s.is_empty())
                    .unwrap_or(false);
                let is_repo = provider3.is_git_repo();
                (wt_status, diff_summaries, has_stash, is_repo)
            })
            .await;

            let _ = this.update(cx, |this, cx| {
                this.working_tree_loading = false;
                // A repo can appear (git init) or vanish while the panel is
                // open; the refresh keeps the missing-repo state current.
                this.repo_missing = !is_repo;
                this.working_tree_status = Some(wt_status);
                this.diff_file_summaries = diff_summaries;
                this.has_stash = has_stash;
                this.rebuild_commit_rows(cx);
            });
        })
        .detach();
    }

    /// Whether the commit log is currently visible.
    pub fn is_commit_log_visible(&self) -> bool {
        self.commit_log_visible
    }

    // ── Rendering ───────────────────────────────────────────────────

    /// Render the git status bar (branch, commit log button, diff stats).
    ///
    /// `current_branch` is the branch name from the git status watcher
    /// (passed in because the watcher lives in the main app).
    pub fn render_git_status(
        &self,
        status: Option<GitStatus>,
        t: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entity_handle = cx.entity().clone();

        match status {
            Some(status) if status.branch.is_some() => {
                let has_changes = status.has_changes();
                let lines_added = status.lines_added;
                let lines_removed = status.lines_removed;
                let project_id = self.project_id.clone();

                h_flex()
                    .flex_shrink_0()
                    .gap(px(6.0))
                    .text_size(ui_text_sm(cx))
                    .line_height(px(12.0))
                    // Diff stats (clickable, only if there are changes)
                    .when(has_changes, |d: Div| {
                        let request_broker = self.request_broker.clone();
                        let project_id_for_click = self.project_id.clone();
                        d.child(
                            project_header::render_diff_stats_badge(lines_added, lines_removed, t)
                                .id(ElementId::Name(
                                    format!("git-diff-stats-{}", project_id).into(),
                                ))
                                .relative()
                                .cursor_pointer()
                                .hover(|s| s.bg(rgb(t.bg_hover)))
                                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation();
                                })
                                .on_hover(cx.listener(move |this, hovered: &bool, _window, cx| {
                                    if *hovered {
                                        this.show_diff_popover(cx);
                                    } else {
                                        this.hide_diff_popover(cx);
                                    }
                                }))
                                .on_click(cx.listener(move |this, _, _window, cx| {
                                    cx.stop_propagation();
                                    this.hide_diff_popover(cx);
                                    request_broker.update(cx, |broker, cx| {
                                        broker.push_overlay_request(
                                            OverlayRequest::DiffViewer {
                                                project_id: project_id_for_click.clone(),
                                                file: None,
                                                mode: None,
                                                commit_message: None,
                                                commits: None,
                                                commit_index: None,
                                            },
                                            cx,
                                        );
                                    });
                                }))
                                // Invisible canvas to capture bounds for popover positioning
                                .child(
                                    canvas(
                                        {
                                            let entity_handle = entity_handle.clone();
                                            move |bounds, _window, app| {
                                                entity_handle.update(app, |this, _cx| {
                                                    this.diff_stats_bounds = bounds;
                                                });
                                            }
                                        },
                                        |_, _, _, _| {},
                                    )
                                    .absolute()
                                    .size_full(),
                                ),
                        )
                    })
                    .into_any_element()
            }
            _ => div().into_any_element(), // Not a git repo - show nothing
        }
    }

    /// Render the diff summary popover (anchored below the diff stats badge).
    pub fn render_diff_popover(&self, t: &ThemeColors, cx: &mut Context<Self>) -> AnyElement {
        if !self.diff_popover_visible || self.diff_file_summaries.is_empty() {
            return div().size_0().into_any_element();
        }

        let entity_handle = cx.entity().clone();
        let request_broker = self.request_broker.clone();
        let project_id = self.project_id.clone();
        let tree_elements = project_header::render_diff_file_list_interactive(
            &self.diff_file_summaries,
            move |file_path, _window, cx| {
                let file_path = file_path.to_string();
                let pid = project_id.clone();
                entity_handle.update(cx, |this: &mut GitHeader, cx| {
                    this.hide_diff_popover(cx);
                });
                request_broker.update(cx, |broker, cx| {
                    broker.push_overlay_request(
                        OverlayRequest::MainDiffViewer {
                            project_id: pid,
                            file: Some(file_path),
                            mode: None,
                            commit_message: None,
                            commits: None,
                            commit_index: None,
                        },
                        cx,
                    );
                });
            },
            t,
            cx,
        );

        let bounds = self.diff_stats_bounds;
        let position = point(
            bounds.origin.x,
            bounds.origin.y + bounds.size.height + px(4.0),
        );

        deferred(
            anchored().position(position).snap_to_window().child(
                div()
                    .id("diff-summary-popover")
                    .occlude()
                    .min_w(px(280.0))
                    .max_w(px(400.0))
                    .max_h(px(300.0))
                    .overflow_y_scroll()
                    .bg(rgb(t.bg_primary))
                    .border_1()
                    .border_color(rgb(t.border))
                    .rounded(px(6.0))
                    .shadow_lg()
                    .py(px(6.0))
                    .on_hover(cx.listener(|this, hovered: &bool, _window, cx| {
                        if *hovered {
                            this.hover_token.fetch_add(1, Ordering::SeqCst);
                        } else {
                            this.hide_diff_popover(cx);
                        }
                    }))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .on_scroll_wheel(|_, _, cx| {
                        cx.stop_propagation();
                    })
                    .children(tree_elements),
            ),
        )
        .into_any_element()
    }

    /// Render the commit log as a panel (no popover wrapper).
    /// Used by the right-side git panel in RootView.
    #[allow(clippy::type_complexity)]
    pub fn render_commit_log_panel(&self, t: &ThemeColors, cx: &mut Context<Self>) -> AnyElement {
        if !self.commit_log_visible {
            return div().size_0().into_any_element();
        }

        // Not a git repository: the whole panel is a single, honest empty
        // state — no tabs, no bogus "HEAD" branch bar.
        if self.repo_missing {
            return v_flex()
                .id("git-panel-content")
                .size_full()
                .items_center()
                .justify_center()
                .gap(px(8.0))
                .p(px(20.0))
                .bg(rgb(t.bg_primary))
                .child(
                    svg()
                        .path("icons/git-branch.svg")
                        .size(px(32.0))
                        .text_color(rgb(t.text_muted)),
                )
                .child(
                    div()
                        .text_size(ui_text_md(cx))
                        .text_color(rgb(t.text_muted))
                        .child("No repo found"),
                )
                .into_any_element();
        }

        v_flex()
            .id("git-panel-content")
            .size_full()
            .relative()
            .bg(rgb(t.bg_primary))
            // Header row with tab-switch buttons
            .child(self.render_panel_header(t, cx))
            // Active tab content
            .child(match self.active_tab {
                GitPanelTab::Commit => self.render_commit_tab(t, cx),
                GitPanelTab::History => self.render_history_tab(t, cx),
            })
            .child(self.render_commit_options_menu(t, cx))
            .child(self.render_overflow_menu(t, cx))
            .into_any_element()
    }

    /// Render a single header tab for switching panel views — icon + label with
    /// an active-tab underline.
    fn render_panel_header_button(
        &self,
        id: &'static str,
        icon_path: &'static str,
        label: &'static str,
        target: GitPanelTab,
        t: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.active_tab == target;
        let color = rgb(if active { t.text_primary } else { t.text_muted });
        h_flex()
            .id(id)
            .h(px(26.0))
            .px(px(10.0))
            .gap(px(6.0))
            .items_center()
            .justify_center()
            .rounded_md()
            .cursor_pointer()
            // Labeled pill: the active one is filled, others highlight on hover.
            .when(active, |d| d.bg(rgb(t.bg_hover)))
            .when(!active, |d| d.hover(|s| s.bg(rgb(t.bg_hover))))
            .child(svg().path(icon_path).size(px(13.0)).text_color(color))
            .child(
                div()
                    .text_size(ui_text_md(cx))
                    .text_color(color)
                    .child(label),
            )
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_click(cx.listener(move |this, _, _window, cx| {
                this.active_tab = target;
                cx.notify();
            }))
    }

    /// Panel header — Changes / History switch buttons.
    fn render_panel_header(&self, t: &ThemeColors, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .h(px(34.0))
            .px(px(6.0))
            .gap(px(4.0))
            .items_center()
            .border_b_1()
            .border_color(rgb(t.border))
            .bg(rgb(t.bg_primary))
            .child(self.render_panel_header_button(
                "git-btn-commit",
                "icons/git-commit.svg",
                "Changes",
                GitPanelTab::Commit,
                t,
                cx,
            ))
            .child(self.render_panel_header_button(
                "git-btn-history",
                "icons/history.svg",
                "History",
                GitPanelTab::History,
                t,
                cx,
            ))
    }

    /// Render the vertical three-dots overflow button. Sits to the right
    /// of the "N Changes / Stage All" header bar at the top of the commit tab.
    fn render_overflow_button(&self, t: &ThemeColors, cx: &mut Context<Self>) -> impl IntoElement {
        let entity_handle = cx.entity().clone();
        div()
            .id("git-btn-overflow")
            .relative()
            .flex_shrink_0()
            .w(px(22.0))
            .h(px(22.0))
            .ml(px(4.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(3.0))
            .cursor_pointer()
            .hover(|s| s.opacity(0.85))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_click(cx.listener(|this, _, _window, cx| {
                this.open_overflow_menu(cx);
            }))
            .tooltip(|_window, cx| Tooltip::new("More").build(_window, cx))
            .child(
                svg()
                    .path("icons/more-vertical.svg")
                    .size(px(14.0))
                    .text_color(rgb(t.text_muted)),
            )
            .child(
                canvas(
                    move |bounds, _window, app| {
                        entity_handle.update(app, |this, _cx| {
                            this.overflow_button_bounds = bounds;
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0()
                .size_full(),
            )
    }

    /// Push an overlay request to open the overflow popover, anchored
    /// just below the three-dots button.
    fn open_overflow_menu(&mut self, cx: &mut Context<Self>) {
        // Popover opens downward from the three-dots button, right edge aligned
        // with the button so the menu doesn't overhang.
        let bounds = self.overflow_button_bounds;
        const MENU_W: Pixels = px(220.0);
        self.overflow_menu_anchor = Some(point(
            bounds.origin.x + bounds.size.width - MENU_W,
            bounds.origin.y + bounds.size.height + px(4.0),
        ));
        self.overflow_menu_visible = true;
        cx.notify();
    }

    /// The header three-dots overflow menu (Stage/Unstage All, Stash All/Pop,
    /// Discard All Tracked). Rendered inline (deferred popover), like the commit
    /// options menu. Item enablement reflects the current working-tree status.
    fn render_overflow_menu(&self, t: &ThemeColors, cx: &mut Context<Self>) -> AnyElement {
        if !self.overflow_menu_visible {
            return div().size_0().into_any_element();
        }
        let status = self.working_tree_status.as_ref();
        let has_staged = status.map(|s| s.staged_count() > 0).unwrap_or(false);
        let has_tracked = status.map(|s| !s.tracked.is_empty()).unwrap_or(false);
        let all_staged = status.map(|s| s.all_staged()).unwrap_or(false);
        let has_unstaged = has_tracked && !all_staged;
        let has_changes = has_tracked || status.map(|s| !s.untracked.is_empty()).unwrap_or(false);
        let has_stash = self.has_stash;
        let position = self.overflow_menu_anchor.unwrap_or_default();

        let sep = || div().h(px(1.0)).mx(px(8.0)).my(px(4.0)).bg(rgb(t.border));

        deferred(
            anchored()
                .position(position)
                .anchor(Anchor::TopLeft)
                .snap_to_window_with_margin(px(8.0))
                .child(
                    v_flex()
                        .id("git-overflow-menu")
                        .occlude()
                        .w(px(220.0))
                        .bg(rgb(t.bg_primary))
                        .border_1()
                        .border_color(rgb(t.border))
                        .rounded(px(6.0))
                        .shadow_lg()
                        .py(px(4.0))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                        .on_mouse_down_out(cx.listener(|this, _, _window, cx| {
                            this.overflow_menu_visible = false;
                            cx.notify();
                        }))
                        .child(self.render_overflow_menu_item(
                            "ovf-stage-all",
                            "icons/chevron-up.svg",
                            "Stage All",
                            has_unstaged,
                            false,
                            |this, cx| this.handle_stage_all(cx),
                            t,
                            cx,
                        ))
                        .child(self.render_overflow_menu_item(
                            "ovf-unstage-all",
                            "icons/chevron-up.svg",
                            "Unstage All",
                            has_staged,
                            false,
                            |this, cx| this.handle_unstage_all(cx),
                            t,
                            cx,
                        ))
                        .child(sep())
                        .child(self.render_overflow_menu_item(
                            "ovf-stash-all",
                            "icons/bookmark.svg",
                            "Stash All",
                            has_changes,
                            false,
                            |this, cx| this.handle_stash_all(cx),
                            t,
                            cx,
                        ))
                        .child(self.render_overflow_menu_item(
                            "ovf-stash-pop",
                            "icons/chevron-up.svg",
                            "Stash Pop",
                            has_stash,
                            false,
                            |this, cx| this.handle_stash_pop(cx),
                            t,
                            cx,
                        ))
                        .child(self.render_overflow_menu_item(
                            "ovf-show-stash",
                            "icons/file.svg",
                            "Show Stash",
                            has_stash,
                            false,
                            |this, cx| this.handle_show_stash(cx),
                            t,
                            cx,
                        ))
                        .child(sep())
                        .child(self.render_overflow_menu_item(
                            "ovf-discard-all",
                            "icons/trash.svg",
                            "Discard All Tracked",
                            has_tracked,
                            true,
                            |this, cx| this.handle_discard_all_tracked(cx),
                            t,
                            cx,
                        )),
                ),
        )
        .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_overflow_menu_item(
        &self,
        id: &'static str,
        icon: &'static str,
        label: &'static str,
        enabled: bool,
        danger: bool,
        action: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        t: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let color = if danger { t.error } else { t.text_secondary };
        h_flex()
            .id(id)
            .h(px(30.0))
            .px(px(10.0))
            .gap(px(10.0))
            .items_center()
            .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(rgb(t.bg_hover))))
            .when(!enabled, |d| d.opacity(0.45))
            .when(enabled, |d| {
                d.on_click(cx.listener(move |this, _, _window, cx| {
                    this.overflow_menu_visible = false;
                    action(this, cx);
                    cx.notify();
                }))
            })
            .child(svg().path(icon).size(px(14.0)).text_color(rgb(color)))
            .child(div().text_size(ui_text_sm(cx)).text_color(rgb(color)).child(label))
    }

    // ── Commit tab ─────────────────────────────────────────────────

    /// Render the Commit tab (staging + commit message + remote operations).
    fn render_commit_tab(&self, t: &ThemeColors, cx: &mut Context<Self>) -> AnyElement {
        let status_ref = self.working_tree_status.as_ref();
        let is_empty =
            self.stash_files.is_none() && status_ref.map(|s| s.total_files() == 0).unwrap_or(true);
        let loading = self.working_tree_loading && status_ref.is_none();

        if loading {
            return v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .p(px(20.0))
                .child(
                    div()
                        .text_size(ui_text_md(cx))
                        .text_color(rgb(t.text_muted))
                        .child("Loading..."),
                )
                .into_any_element();
        }

        v_flex()
            .flex_1()
            .min_h_0()
            // Header bar (N changes + Stage/Unstage All)
            .child(self.render_commit_header_bar(t, cx))
            // Error toast (if any)
            .when_some(self.last_error.clone(), |d, err| {
                d.child(
                    div()
                        .px(px(10.0))
                        .py(px(6.0))
                        .bg(rgb(t.error))
                        .text_color(rgb(t.bg_primary))
                        .text_size(ui_text_sm(cx))
                        .child(err),
                )
            })
            // File sections (or empty state)
            .child(if is_empty {
                self.render_empty_commit_state(t, cx).into_any_element()
            } else {
                self.render_file_sections(t, cx).into_any_element()
            })
            // Branch/remote footer bar
            .child(self.render_branch_bar(t, cx))
            // Commit message input + button
            .child(self.render_commit_footer(t, cx))
            .into_any_element()
    }

    /// Render the commit tab header — "N Changes" text on the left,
    /// Stage/Unstage All button on the right.
    fn render_commit_header_bar(
        &self,
        t: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let status = self.working_tree_status.as_ref();
        let total = status.map(|s| s.total_files()).unwrap_or(0);
        let all_staged = status.map(|s| s.all_staged()).unwrap_or(false);
        let has_any_changes = total > 0;

        // Total added/removed line counts across all changed files.
        let (total_added, total_removed) = status
            .map(|s| {
                let sum = |files: &[WorkingFile]| {
                    files.iter().fold((0usize, 0usize), |(a, r), f| (a + f.added, r + f.removed))
                };
                let (ca, cr) = sum(&s.conflicts);
                let (ta, tr) = sum(&s.tracked);
                let (ua, ur) = sum(&s.untracked);
                (ca + ta + ua, cr + tr + ur)
            })
            .unwrap_or((0, 0));

        let label = match total {
            0 => "No Changes".to_string(),
            1 => "1 Change".to_string(),
            n => format!("{} Changes", n),
        };

        h_flex()
            .pl(px(12.0))
            .pr(px(6.0))
            .py(px(5.0))
            .gap(px(8.0))
            .items_center()
            .border_b_1()
            .border_color(rgb(t.border))
            .child(
                div()
                    .text_size(ui_text_sm(cx))
                    .text_color(rgb(t.text_muted))
                    .child(label),
            )
            // Aggregate +added / -removed line counts for all changes.
            .when(has_any_changes && (total_added > 0 || total_removed > 0), |d| {
                d.child(
                    h_flex()
                        .gap(px(5.0))
                        .text_size(ui_text_sm(cx))
                        .child(div().text_color(rgb(t.success)).child(format!("+{total_added}")))
                        .child(div().text_color(rgb(t.error)).child(format!("-{total_removed}"))),
                )
            })
            .child(div().flex_1())
            .when(has_any_changes, |d| {
                d.child(
                    div()
                        .id("stage-all-btn")
                        .px(px(8.0))
                        .py(px(2.0))
                        .rounded(px(3.0))
                        .text_size(ui_text_sm(cx))
                        .text_color(rgb(t.text_secondary))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(t.bg_hover)))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_click(cx.listener(move |this, _, _window, cx| {
                            if all_staged {
                                this.handle_unstage_all(cx);
                            } else {
                                this.handle_stage_all(cx);
                            }
                        }))
                        .child(if all_staged {
                            "Unstage All"
                        } else {
                            "Stage All"
                        }),
                )
            })
            .child(self.render_overflow_button(t, cx))
    }

    /// Render empty state when working tree is clean.
    fn render_empty_commit_state(
        &self,
        t: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .gap(px(8.0))
            .p(px(20.0))
            .child(
                svg()
                    .path("icons/git-commit.svg")
                    .size(px(32.0))
                    .text_color(rgb(t.text_muted)),
            )
            .child(
                div()
                    .text_size(ui_text_md(cx))
                    .text_color(rgb(t.text_muted))
                    .child("Working tree clean"),
            )
            .child(
                div()
                    .text_size(ui_text_sm(cx))
                    .text_color(rgb(t.text_muted))
                    .child("No changes to commit"),
            )
    }

    /// Append a file's row and, when its diff is expanded, the diff rows
    /// (one per diff line / context expander, or a status note).
    fn append_file_diff_rows(
        &self,
        rows: &mut Vec<CommitRow>,
        file: &WorkingFile,
        is_untracked: bool,
        cx: &App,
    ) {
        rows.push(CommitRow::File { file: file.clone(), is_untracked });
        self.append_inline_diff_rows(rows, &file.path, cx);
    }

    /// Append the inline diff rows for `path` when it's expanded (diff lines +
    /// optional h-scrollbar, or a status note). Shared by file and stash rows.
    fn append_inline_diff_rows(&self, rows: &mut Vec<CommitRow>, path: &str, cx: &App) {
        if !self.inline_expanded.contains(path) {
            return;
        }
        let Some(viewer) = self.inline_viewers.get(path) else {
            return;
        };
        let v = viewer.read(cx);
        if let Some(err) = v.inline_error() {
            rows.push(CommitRow::DiffNote { text: format!("Error: {err}").into() });
        } else if v.inline_loading() {
            rows.push(CommitRow::DiffNote { text: "Loading…".into() });
        } else {
            let n = v.inline_item_count();
            if n == 0 {
                rows.push(CommitRow::DiffNote { text: "No changes".into() });
            } else {
                for i in 0..n {
                    rows.push(CommitRow::DiffItem { path: path.to_string(), item_index: i });
                }
                if v.inline_max_scroll_x() > 0.5 {
                    rows.push(CommitRow::DiffHScrollbar { path: path.to_string() });
                }
            }
        }
    }

    /// Rebuild the flattened commit-tab row model and resize the list state to
    /// match. Call on any structural change (status refresh, expand/collapse,
    /// diff load, context expansion, section collapse).
    fn rebuild_commit_rows(&mut self, cx: &mut Context<Self>) {
        // Push the current viewport width into the inline viewers first so their
        // horizontal-overflow check (which decides whether to add a scrollbar
        // row) and scroll math are correct.
        let vw = self.diff_viewport_width;
        if vw > 0.0 {
            let viewers: Vec<_> = self.inline_viewers.values().cloned().collect();
            for v in viewers {
                v.update(cx, |v, _| v.set_inline_viewport_width(vw));
            }
        }

        let mut rows: Vec<CommitRow> = Vec::new();

        // Stash view: a header + the stash's files (read-only) with inline diffs.
        if let Some(stash) = self.stash_files.clone() {
            rows.push(CommitRow::StashHeader);
            for f in &stash {
                rows.push(CommitRow::StashFile {
                    path: f.path.clone(),
                    added: f.added,
                    removed: f.removed,
                });
                self.append_inline_diff_rows(&mut rows, &f.path, cx);
            }
            self.sync_commit_list(rows);
            cx.notify();
            return;
        }

        if let Some(status) = self.working_tree_status.as_mut() {
            status.tracked.sort_by(|a, b| a.path.cmp(&b.path));
            status.untracked.sort_by(|a, b| a.path.cmp(&b.path));
            status.conflicts.sort_by(|a, b| a.path.cmp(&b.path));
        }
        let status = self.working_tree_status.as_ref();
        let conflicts: Vec<WorkingFile> = status.map(|s| s.conflicts.clone()).unwrap_or_default();
        let tracked: Vec<WorkingFile> = status.map(|s| s.tracked.clone()).unwrap_or_default();
        let untracked: Vec<WorkingFile> = status.map(|s| s.untracked.clone()).unwrap_or_default();

        if !conflicts.is_empty() {
            rows.push(CommitRow::Header(FileSectionKind::Conflicts, self.conflicts_collapsed));
            if !self.conflicts_collapsed {
                for f in &conflicts {
                    self.append_file_diff_rows(&mut rows, f, false, cx);
                }
            }
        }
        if !tracked.is_empty() {
            rows.push(CommitRow::Header(FileSectionKind::Tracked, self.tracked_collapsed));
            if !self.tracked_collapsed {
                for f in &tracked {
                    self.append_file_diff_rows(&mut rows, f, false, cx);
                }
            }
        }
        if !untracked.is_empty() {
            rows.push(CommitRow::Header(FileSectionKind::Untracked, self.untracked_collapsed));
            if !self.untracked_collapsed {
                for f in &untracked {
                    self.append_file_diff_rows(&mut rows, f, true, cx);
                }
            }
        }

        self.sync_commit_list(rows);
        cx.notify();
    }

    /// Apply `new_rows` to the list with a minimal splice (replace only the
    /// changed middle, keeping the common prefix/suffix) so the scroll anchor
    /// survives — e.g. expanding hidden context mid-file must not jump to the
    /// top. A full `splice(0..old, new)` would reset the scroll.
    fn sync_commit_list(&mut self, new_rows: Vec<CommitRow>) {
        let old = &self.commit_rows;
        let (old_len, new_len) = (old.len(), new_rows.len());

        let mut prefix = 0;
        while prefix < old_len
            && prefix < new_len
            && rows_eq(&old[prefix], &new_rows[prefix])
        {
            prefix += 1;
        }
        let mut suffix = 0;
        while suffix < old_len - prefix
            && suffix < new_len - prefix
            && rows_eq(&old[old_len - 1 - suffix], &new_rows[new_len - 1 - suffix])
        {
            suffix += 1;
        }

        let removed = old_len - prefix - suffix;
        let added = new_len - prefix - suffix;
        self.commit_rows = new_rows;
        if removed != 0 || added != 0 {
            self.commit_list_state.splice(prefix..(prefix + removed), added);
            // `splice` inserts the new rows as *unmeasured* and does NOT clear
            // the one-shot `measure_all` flag, so without this they'd be measured
            // lazily while scrolling (shrinking the scrollbar). Re-arm full
            // measurement so the next layout measures every row → exact scrollbar
            // immediately after expand/collapse. (`measure_all` only sets the
            // measuring flag; it does not touch the scroll position.)
            self.commit_list_state = self.commit_list_state.clone().measure_all();
        }
    }

    /// Render one commit-tab row by index (the `list()` render closure target).
    /// Runs after the parent render borrow is released, so re-entering the
    /// entity is safe.
    fn render_commit_row(
        entity: &Entity<Self>,
        ix: usize,
        t: &ThemeColors,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let Some(row) = entity.read(cx).commit_rows.get(ix).cloned() else {
            return div().into_any_element();
        };
        match row {
            CommitRow::Header(kind, collapsed) => {
                let (title, color) = match kind {
                    FileSectionKind::Conflicts => ("Conflicts", Some(t.error)),
                    FileSectionKind::Tracked => ("Tracked", None),
                    FileSectionKind::Untracked => ("Untracked", None),
                };
                entity.update(cx, |this, cx| {
                    this.render_section_header_kind(title, collapsed, kind, color, t, cx)
                        .into_any_element()
                })
            }
            CommitRow::File { file, is_untracked } => entity.update(cx, |this, cx| {
                this.render_file_entry(&file, is_untracked, t, cx).into_any_element()
            }),
            CommitRow::DiffItem { path, item_index } => {
                let (viewer, vw) = {
                    let this = entity.read(cx);
                    (this.inline_viewers.get(&path).cloned(), this.diff_viewport_width)
                };
                match viewer {
                    Some(v) => v.update(cx, |v, cx| {
                        v.set_inline_viewport_width(vw);
                        v.render_inline_item(item_index, t, window, cx)
                    }),
                    None => div().into_any_element(),
                }
            }
            CommitRow::DiffNote { text } => h_flex()
                .w_full()
                .px(px(16.0))
                .py(px(4.0))
                .text_color(rgb(t.text_muted))
                .child(text)
                .into_any_element(),
            CommitRow::DiffHScrollbar { path } => {
                let viewer = entity.read(cx).inline_viewers.get(&path).cloned();
                match viewer {
                    Some(v) => div()
                        .w_full()
                        .h(px(HSCROLLBAR_HEIGHT))
                        .child(DiffHScrollbar::new(
                            v,
                            rgb(t.scrollbar).into(),
                            rgb(t.scrollbar_hover).into(),
                        ))
                        .into_any_element(),
                    None => div().into_any_element(),
                }
            }
            CommitRow::StashHeader => entity.update(cx, |this, cx| {
                this.render_stash_header(t, cx).into_any_element()
            }),
            CommitRow::StashFile { path, added, removed } => entity.update(cx, |this, cx| {
                this.render_stash_file(&path, added, removed, t, cx).into_any_element()
            }),
        }
    }

    /// The "Stashed changes" header row, with a close button to return to the
    /// working tree.
    fn render_stash_header(&self, t: &ThemeColors, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .h(px(30.0))
            .pl(px(12.0))
            .pr(px(8.0))
            .items_center()
            .gap(px(6.0))
            .border_b_1()
            .border_color(rgb(t.border))
            .bg(rgb(t.bg_header))
            .child(svg().path("icons/bookmark.svg").size(px(13.0)).text_color(rgb(t.text_secondary)))
            .child(
                div()
                    .flex_1()
                    .text_size(ui_text_sm(cx))
                    .text_color(rgb(t.text_primary))
                    .child("Stashed changes"),
            )
            .child(
                div()
                    .id("stash-close")
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(20.0))
                    .rounded(px(3.0))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(t.bg_hover)))
                    .child(svg().path("icons/close.svg").size(px(11.0)).text_color(rgb(t.text_muted)))
                    .on_click(cx.listener(|this, _, _window, cx| this.close_stash(cx))),
            )
    }

    /// A stash file row — filename + diff stats + disclosure chevron; clicking
    /// toggles its inline diff (loaded from the stash commit).
    fn render_stash_file(
        &self,
        path: &str,
        added: usize,
        removed: usize,
        t: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_expanded = self.inline_expanded.contains(path);
        let (dir_part, file_name) = match path.rfind('/') {
            Some(i) => (path[..=i].to_string(), path[i + 1..].to_string()),
            None => (String::new(), path.to_string()),
        };
        let path_owned = path.to_string();
        h_flex()
            .id(ElementId::Name(format!("stash-{path}").into()))
            // Inset rounded hover pill — matches the sidebar's rows.
            .mx(px(6.0))
            .pl(px(8.0))
            .pr(px(8.0))
            .h(px(32.0))
            .gap(px(6.0))
            .items_center()
            .cursor_pointer()
            .rounded_lg()
            .hover(|s| s.bg(rgb(t.bg_hover)))
            .on_click(cx.listener(move |this, _, _window, cx| {
                this.toggle_inline_diff(path_owned.clone(), false, cx);
            }))
            // Disclosure chevron at the row start — same as the sidebar explorer
            .child(
                div()
                    .w(px(16.0))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        svg()
                            .path(if is_expanded {
                                "icons/chevron-down.svg"
                            } else {
                                "icons/chevron-right.svg"
                            })
                            .size(px(14.0))
                            .text_color(rgb(t.text_muted)),
                    ),
            )
            .child(vscode_file_icon_with_options(
                &file_name,
                t,
                git_settings(cx).monochrome_icons,
                cx,
            ))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .items_baseline()
                    .gap(px(4.0))
                    .text_size(ui_text_md(cx))
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(div().flex_shrink_0().text_color(rgb(t.text_primary)).child(file_name))
                    .when(!dir_part.is_empty(), |d| {
                        d.child(
                            div()
                                .text_color(rgb(t.text_muted))
                                .text_ellipsis()
                                .overflow_hidden()
                                .child(dir_part),
                        )
                    }),
            )
            .when(added > 0 || removed > 0, |d| {
                d.child(
                    h_flex()
                        .flex_shrink_0()
                        .gap(px(4.0))
                        .text_size(ui_text_sm(cx))
                        .child(div().text_color(rgb(t.success)).child(format!("+{added}")))
                        .child(div().text_color(rgb(t.error)).child(format!("-{removed}"))),
                )
            })
    }

    fn render_file_sections(&self, t: &ThemeColors, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *t;
        let entity = cx.entity();
        let entity_w = cx.entity();
        // One viewported list for the whole tab: virtualized (only visible rows
        // realized), scrolls as one, no per-diff scrollbox. Diffs show at full
        // height because every diff line is its own list row.
        div()
            .id("commit-file-list")
            .relative()
            .flex_1()
            .min_h_0()
            .size_full()
            // Capture the list width once (and on resize) and push it into the
            // inline diff viewers so horizontal-scroll math + scrollbar
            // visibility are correct. Runs before the list prepaints (first
            // child), so the rebuilt rows apply the same frame.
            .child(
                canvas(
                    move |bounds, _window, app| {
                        let w = f32::from(bounds.size.width);
                        entity_w.update(app, |this, cx| {
                            if (this.diff_viewport_width - w).abs() > 0.5 {
                                this.diff_viewport_width = w;
                                this.rebuild_commit_rows(cx);
                            }
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(
                list(self.commit_list_state.clone(), move |ix, window, cx| {
                    Self::render_commit_row(&entity, ix, &t, window, cx)
                })
                .size_full(),
            )
            // Draggable scrollbar overlaid on the right edge.
            .child(
                div().absolute().top_0().right_0().bottom_0().w(px(8.0)).py(px(4.0)).child(
                    ListScrollbar::new(
                        self.commit_list_state.clone(),
                        gpui::transparent_black(),
                        rgb(t.scrollbar).into(),
                        rgb(t.scrollbar_hover).into(),
                    ),
                ),
            )
    }

    fn render_section_header_kind(
        &self,
        title: &'static str,
        collapsed: bool,
        kind: FileSectionKind,
        accent_color: Option<u32>,
        t: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.render_section_header(
            title,
            collapsed,
            move |this| match kind {
                FileSectionKind::Conflicts => this.conflicts_collapsed = !this.conflicts_collapsed,
                FileSectionKind::Tracked => this.tracked_collapsed = !this.tracked_collapsed,
                FileSectionKind::Untracked => this.untracked_collapsed = !this.untracked_collapsed,
            },
            accent_color,
            t,
            cx,
        )
    }

    /// Render a collapsible section header — just the label, muted,
    /// no count, no chevron. Click area stays for toggling.
    fn render_section_header(
        &self,
        title: &'static str,
        collapsed: bool,
        toggle: impl Fn(&mut Self) + 'static,
        accent_color: Option<u32>,
        t: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let color = accent_color.unwrap_or(t.text_muted);

        h_flex()
            .id(ElementId::Name(format!("section-hdr-{}", title).into()))
            .pl(px(12.0))
            .pr(px(8.0))
            .pt(px(6.0))
            .pb(px(2.0))
            .items_end()
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_click(cx.listener(move |this, _, _window, cx| {
                toggle(this);
                this.rebuild_commit_rows(cx);
            }))
            .child(
                div()
                    .text_size(ui_text_sm(cx))
                    .text_color(rgb(color))
                    .when(collapsed, |d| d.opacity(0.6))
                    .child(title),
            )
    }

    /// Render a single file entry — compact (~24px), filename with
    /// muted parent path, status-color on the filename, checkbox on the right.
    fn render_file_entry(
        &self,
        file: &WorkingFile,
        is_untracked_section: bool,
        t: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let file_path = file.path.clone();
        let file_path_click = file.path.clone();
        let file_path_ctx = file.path.clone();
        let is_fully_staged = file.is_fully_staged();
        let is_expanded = self.inline_expanded.contains(&file.path);
        let is_partial = file.is_partially_staged();
        let is_conflict = file.has_conflict();
        let status = file.effective_status();
        let added = file.added;
        let removed = file.removed;

        // Filename always uses the primary text color; the status letter
        // after the name carries the status signal.
        let name_color = t.text_primary;

        // Status letter + its color (shown after filename) — VS Code convention.
        let (status_letter, status_color) = match status {
            _ if is_conflict => ("!", t.error),
            FileStatus::Modified => ("M", t.warning),
            FileStatus::Added => ("A", t.success),
            FileStatus::Deleted => ("D", t.error),
            FileStatus::Renamed => ("R", t.border_active),
            FileStatus::Copied => ("C", t.border_active),
            FileStatus::Untracked => ("U", t.success),
            FileStatus::Conflict => ("!", t.error),
        };

        // Split path into directory and filename for two-tone display
        let (dir_part, file_name) = match file.path.rfind('/') {
            Some(i) => (&file.path[..=i], &file.path[i + 1..]),
            None => ("", file.path.as_str()),
        };
        let file_name = file_name.to_string();
        // Front-elide the directory so its meaningful tail (deepest folders)
        // stays visible — e.g. "crates/notmux-views-terminal/src/layout/tabs/"
        // → "…-terminal/src/layout/tabs/". Budget is derived from the captured
        // panel width minus the row's fixed chrome and the filename, so the
        // diff stats, status letter and stage checkbox stay pinned to the right.
        let dir_part = {
            let panel_w = if self.diff_viewport_width > 1.0 {
                self.diff_viewport_width
            } else {
                320.0
            };
            // Fixed chrome: margins/padding, file icon, gaps, diff stats,
            // status letter, chevron, checkbox.
            let reserved_px = 170.0 + file_name.chars().count() as f32 * 7.0;
            let dir_px = (panel_w - reserved_px).max(48.0);
            let max_chars = (dir_px / 6.5) as usize;
            elide_dir_front(dir_part, max_chars.max(10))
        };

        let request_broker_ctx = self.request_broker.clone();
        let project_id_ctx = self.project_id.clone();

        h_flex()
            .id(ElementId::Name(format!("file-{}", file.path).into()))
            // Inset rounded hover pill — matches the sidebar's rows.
            .mx(px(6.0))
            // Full width so the name column's `flex_1` actually grows and the
            // diff stats / status letter / checkbox sit in a right-aligned
            // column instead of clustering right after the path string.
            .w_full()
            .pl(px(8.0))
            .pr(px(8.0))
            .h(px(32.0))
            .gap(px(6.0))
            .items_center()
            .rounded_lg()
            .cursor_pointer()
            .hover(|s| s.bg(rgb(t.bg_hover)))
            // Whole row (chevron included) toggles the inline diff; the
            // checkbox stops propagation and keeps its own action.
            .on_click(cx.listener(move |this, _, _window, cx| {
                this.toggle_inline_diff(file_path_click.clone(), is_fully_staged, cx);
            }))
            // Right-click context menu
            .on_mouse_down(MouseButton::Right, {
                let pid = project_id_ctx.clone();
                let fp = file_path_ctx.clone();
                let broker = request_broker_ctx.clone();
                move |event: &MouseDownEvent, _window, cx| {
                    cx.stop_propagation();
                    broker.update(cx, |broker, cx| {
                        broker.push_overlay_request(
                            OverlayRequest::GitFileContextMenu {
                                project_id: pid.clone(),
                                file_path: fp.clone(),
                                is_staged: is_fully_staged || is_partial,
                                is_untracked: is_untracked_section,
                                is_conflict,
                                position: event.position,
                            },
                            cx,
                        );
                    });
                }
            })
            // Disclosure chevron at the row start — same as the sidebar
            // explorer: right when collapsed, down when the diff is expanded.
            .child(
                div()
                    .w(px(16.0))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        svg()
                            .path(if is_expanded {
                                "icons/chevron-down.svg"
                            } else {
                                "icons/chevron-right.svg"
                            })
                            .size(px(14.0))
                            .text_color(rgb(t.text_muted)),
                    ),
            )
            // VSCode-icons file-type icon (real shape, language-tinted).
            .child(vscode_file_icon_with_options(
                &file_name,
                t,
                git_settings(cx).monochrome_icons,
                cx,
            ))
            // Filename (status color) + parent dir (muted):
            // `min_w_0` lets the flex item shrink below content size,
            // `flex_1` makes it grow to consume the available space so the
            // diff stats + status letter sit on the right edge.
            .child(
                div()
                    .id(ElementId::Name(format!("fn-{}", file.path).into()))
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .items_baseline()
                    .gap(px(4.0))
                    .text_size(ui_text_md(cx))
                    .when(matches!(status, FileStatus::Deleted), |d| d.line_through())
                    .overflow_hidden()
                    .child(div().flex_shrink_0().text_color(rgb(name_color)).child(file_name))
                    .when(!dir_part.is_empty(), |d| {
                        // The directory is the shrinking element: `min_w_0` lets
                        // it clip below its content width (the string is already
                        // front-elided above) while staying next to the filename,
                        // so the diff stats / status letter / checkbox stay pinned
                        // to the right and the chevron hugs the name cluster.
                        d.child(
                            div()
                                .min_w_0()
                                .text_color(rgb(t.text_muted))
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .overflow_hidden()
                                .child(dir_part),
                        )
                    }),
            )
            // Diff stats — always show BOTH +N and -M together (or nothing if 0/0)
            .when(added > 0 || removed > 0, |d| {
                d.child(
                    h_flex()
                        .flex_shrink_0()
                        .gap(px(4.0))
                        .text_size(ui_text_sm(cx))
                        .child(
                            div()
                                .text_color(rgb(t.success))
                                .child(format!("+{}", added)),
                        )
                        .child(
                            div()
                                .text_color(rgb(t.error))
                                .child(format!("-{}", removed)),
                        ),
                )
            })
            // Status letter (M/A/D/R/C/?/U) AFTER the diff stats
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(12.0))
                    .text_size(ui_text_sm(cx))
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgb(status_color))
                    .child(status_letter),
            )
            // Checkbox on the RIGHT — after the status letter (20x20
            // outer, 16x16 inner, neutral border, darker fill, accent check).
            .child({
                let path = file_path.clone();
                div()
                    .id(ElementId::Name(format!("cb-{}", file.path).into()))
                    .flex_shrink_0()
                    .w(px(20.0))
                    .h(px(20.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        cx.stop_propagation();
                        if is_fully_staged {
                            this.handle_unstage_file(&path, cx);
                        } else {
                            this.handle_stage_file(&path, cx);
                        }
                    }))
                    .child(
                        div()
                            .w(px(16.0))
                            .h(px(16.0))
                            .rounded(px(3.0))
                            .border_1()
                            .border_color(rgb(t.border))
                            .bg(rgb(t.bg_hover))
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|s| s.border_color(rgb(t.border_active)))
                            .when(is_fully_staged, |d| {
                                d.child(
                                    svg()
                                        .path("icons/check.svg")
                                        .size(px(12.0))
                                        .text_color(rgb(t.border_active)),
                                )
                            })
                            .when(is_partial && !is_fully_staged, |d| {
                                d.child(
                                    div()
                                        .w(px(8.0))
                                        .h(px(2.0))
                                        .rounded(px(1.0))
                                        .bg(rgb(t.border_active)),
                                )
                            }),
                    )
            })
    }

    /// Render the branch/remote bar (branch name + ahead/behind + fetch/pull/push).
    fn render_branch_bar(&self, t: &ThemeColors, cx: &mut Context<Self>) -> impl IntoElement {
        let status = self.working_tree_status.as_ref();
        let branch = status
            .and_then(|s| s.branch.clone())
            .or_else(|| self.current_branch.clone())
            .unwrap_or_else(|| "HEAD".to_string());
        let ahead = status.map(|s| s.ahead).unwrap_or(0);
        let behind = status.map(|s| s.behind).unwrap_or(0);
        let has_upstream = status.map(|s| s.has_upstream).unwrap_or(false);

        h_flex()
            .px(px(10.0))
            .py(px(6.0))
            .gap(px(6.0))
            .items_center()
            .border_t_1()
            .border_color(rgb(t.border))
            .bg(rgb(t.bg_header))
            // Branch + ahead/behind
            .child(
                svg()
                    .path("icons/git-branch.svg")
                    .size(px(11.0))
                    .text_color(rgb(t.text_secondary)),
            )
            .child(
                div()
                    .text_size(ui_text_sm(cx))
                    .text_color(rgb(t.text_primary))
                    .max_w(px(160.0))
                    .text_ellipsis()
                    .overflow_hidden()
                    .child(branch),
            )
            .when(has_upstream && ahead > 0, |d| {
                d.child(
                    div()
                        .text_size(ui_text_sm(cx))
                        .text_color(rgb(t.success))
                        .child(format!("↑{}", ahead)),
                )
            })
            .when(has_upstream && behind > 0, |d| {
                d.child(
                    div()
                        .text_size(ui_text_sm(cx))
                        .text_color(rgb(t.warning))
                        .child(format!("↓{}", behind)),
                )
            })
            // Spacer
            .child(div().flex_1())
            // Fetch / Pull / Push buttons
            .child(self.render_remote_button(
                "git-btn-fetch",
                "Fetch",
                RemoteOp::Fetch,
                "icons/refresh.svg",
                t,
                cx,
            ))
            .child(self.render_remote_button(
                "git-btn-pull",
                "Pull",
                RemoteOp::Pull,
                "icons/chevron-down.svg",
                t,
                cx,
            ))
            .child(self.render_remote_button(
                "git-btn-push",
                "Push",
                RemoteOp::Push,
                "icons/chevron-up.svg",
                t,
                cx,
            ))
    }

    /// Render a single remote operation button (Fetch/Pull/Push).
    fn render_remote_button(
        &self,
        id: &'static str,
        tooltip_text: &'static str,
        op: RemoteOp,
        icon_path: &'static str,
        t: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let running = self.remote_op_running == Some(op);
        let disabled = self.remote_op_running.is_some();

        div()
            .id(id)
            .w(px(24.0))
            .h(px(20.0))
            .rounded(px(3.0))
            .flex()
            .items_center()
            .justify_center()
            .when(!disabled, |d| {
                d.cursor_pointer().hover(|s| s.bg(rgb(t.bg_hover)))
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .when(!disabled, |d| {
                d.on_click(cx.listener(move |this, _, _window, cx| match op {
                    RemoteOp::Fetch => this.handle_fetch(cx),
                    RemoteOp::Pull => this.handle_pull(cx),
                    RemoteOp::Push => this.handle_push(cx),
                }))
            })
            .tooltip(move |_w, cx| Tooltip::new(tooltip_text).build(_w, cx))
            .child(
                svg()
                    .path(icon_path)
                    .size(px(11.0))
                    .text_color(rgb(if running {
                        t.border_active
                    } else if disabled {
                        t.text_muted
                    } else {
                        t.text_secondary
                    })),
            )
    }

    /// Render the commit message footer (multi-line input + commit button + options).
    fn render_commit_footer(&self, t: &ThemeColors, cx: &mut Context<Self>) -> impl IntoElement {
        let status = self.working_tree_status.as_ref();
        let has_staged = status.map(|s| s.staged_count() > 0).unwrap_or(false);
        let has_tracked = status.map(|s| !s.tracked.is_empty()).unwrap_or(false);
        let amend = self.commit_options_amend;
        let message_empty = self.commit_message_input.read(cx).value().is_empty();

        let can_commit = !message_empty && !self.committing && (has_staged || has_tracked || amend);

        let button_label = commit_button_label(self.committing, amend, has_staged, has_tracked);

        v_flex()
            .border_t_1()
            .border_color(rgb(t.border))
            // Message input — borderless, taller than the default SimpleInput height.
            .child({
                let input = self.commit_message_input.clone();
                div()
                    .id("commit-message-wrap")
                    .pb(px(8.0))
                    // Clear the workspace's "currently focused terminal" claim
                    // and explicitly focus the input. Without this the
                    // TerminalPane re-grabs GPUI focus on the next render,
                    // because its render-loop sees its layout_path is still
                    // the active one in `focus_manager`.
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            this.workspace.update(cx, |ws, cx| {
                                ws.focus_manager.clear_focus();
                                cx.notify();
                            });
                            input.update(cx, |i, cx| i.focus(window, cx));
                        }),
                    )
                    .child(
                        div()
                            .min_h(px(96.0))
                            .max_h(px(180.0))
                            .bg(rgb(t.bg_primary))
                            .px(px(8.0))
                            .py(px(6.0))
                            // Match the composer's "Send follow-up" typeface/size
                            // so the commit input reads as the same input.
                            .text_size(ui_text_md(cx))
                            .child(self.commit_message_input.clone()),
                    )
            })
            // Options row + commit button
            .child(
                h_flex()
                    .px(px(8.0))
                    .pb(px(8.0))
                    .gap(px(6.0))
                    .items_center()
                    .child(self.render_commit_options_button(t, cx))
                    // Spacer
                    .child(div().flex_1())
                    // Commit button
                    .child(
                        div()
                            .id("commit-btn")
                            .h(px(24.0))
                            .px(px(12.0))
                            .rounded(px(4.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(ui_text_sm(cx))
                            .font_weight(FontWeight::MEDIUM)
                            .when(can_commit, |d| {
                                d.bg(rgb(t.bg_secondary))
                                    .text_color(rgb(t.text_primary))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgb(t.bg_hover)))
                            })
                            .when(!can_commit, |d| {
                                d.bg(rgb(t.bg_hover)).text_color(rgb(t.text_muted))
                            })
                            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                cx.stop_propagation();
                            })
                            .when(can_commit, |d| {
                                d.on_click(cx.listener(|this, _, _window, cx| {
                                    this.handle_commit(cx);
                                }))
                            })
                            .child(button_label),
                    ),
            )
    }

    fn render_commit_options_button(
        &self,
        t: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.commit_options_amend || self.commit_options_signoff;

        div()
            .id("commit-options-btn")
            .flex_shrink_0()
            .w(px(28.0))
            .h(px(24.0))
            .rounded(px(4.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|s| s.opacity(0.85))
            .on_any_mouse_down(cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                if event.button == MouseButton::Left {
                    this.commit_options_menu_anchor = Some(event.position);
                    this.commit_options_menu_visible = !this.commit_options_menu_visible;
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .tooltip(|_window, cx| Tooltip::new("Commit Options").build(_window, cx))
            .child(
                svg()
                    .path("icons/more-vertical.svg")
                    .size(px(14.0))
                    .text_color(rgb(if active {
                        t.text_primary
                    } else {
                        t.text_muted
                    })),
            )
    }

    fn render_commit_options_menu(&self, t: &ThemeColors, cx: &mut Context<Self>) -> AnyElement {
        if !self.commit_options_menu_visible || self.active_tab != GitPanelTab::Commit {
            return div().size_0().into_any_element();
        }

        let position = self.commit_options_menu_anchor.unwrap_or_default();
        let can_uncommit = self.latest_commit().is_some() && !self.uncommitting && !self.committing;

        deferred(
            anchored()
                .position(position)
                .anchor(Anchor::BottomLeft)
                .snap_to_window_with_margin(px(8.0))
                .child(
                    v_flex()
                        .id("commit-options-menu")
                        .occlude()
                        .w(px(180.0))
                        .bg(rgb(t.bg_primary))
                        .border_1()
                        .border_color(rgb(t.border))
                        .rounded(px(6.0))
                        .shadow_lg()
                        .py(px(4.0))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_mouse_down(MouseButton::Right, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_mouse_down_out(cx.listener(|this, _, _window, cx| {
                            this.commit_options_menu_visible = false;
                            cx.notify();
                        }))
                        .child(self.render_commit_option_menu_item(
                            "commit-menu-amend",
                            "Amend",
                            self.commit_options_amend,
                            |this| this.commit_options_amend = !this.commit_options_amend,
                            t,
                            cx,
                        ))
                        .child(self.render_commit_action_menu_item(
                            "commit-menu-uncommit",
                            "Undo last commit",
                            can_uncommit,
                            |this, cx| this.handle_uncommit(cx),
                            t,
                            cx,
                        ))
                        .child(self.render_commit_option_menu_item(
                            "commit-menu-signoff",
                            "Sign-off",
                            self.commit_options_signoff,
                            |this| this.commit_options_signoff = !this.commit_options_signoff,
                            t,
                            cx,
                        ))
                ),
        )
        .into_any_element()
    }

    fn render_commit_option_menu_item(
        &self,
        id: &'static str,
        label: &'static str,
        active: bool,
        toggle: impl Fn(&mut Self) + 'static,
        t: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .id(id)
            .h(px(28.0))
            .px(px(10.0))
            .gap(px(8.0))
            .items_center()
            .cursor_pointer()
            .hover(|s| s.bg(rgb(t.bg_hover)))
            .on_click(cx.listener(move |this, _, _window, cx| {
                toggle(this);
                cx.notify();
            }))
            .child(
                div()
                    .w(px(14.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(if active {
                        svg()
                            .path("icons/check.svg")
                            .size(px(12.0))
                            .text_color(rgb(t.border_active))
                            .into_any_element()
                    } else {
                        div().size(px(12.0)).into_any_element()
                    }),
            )
            .child(
                div()
                    .text_size(ui_text_sm(cx))
                    .text_color(rgb(if active {
                        t.border_active
                    } else {
                        t.text_secondary
                    }))
                    .child(label),
            )
    }

    fn render_commit_action_menu_item(
        &self,
        id: &'static str,
        label: &'static str,
        enabled: bool,
        action: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        t: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .id(id)
            .h(px(28.0))
            .px(px(10.0))
            .gap(px(8.0))
            .items_center()
            .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(rgb(t.bg_hover))))
            .when(!enabled, |d| d.opacity(0.55))
            .when(enabled, |d| {
                d.on_click(cx.listener(move |this, _, _window, cx| {
                    this.commit_options_menu_visible = false;
                    action(this, cx);
                    cx.notify();
                }))
            })
            .child(
                div()
                    .w(px(14.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(div().size(px(12.0))),
            )
            .child(
                div()
                    .text_size(ui_text_sm(cx))
                    .text_color(rgb(if enabled {
                        t.text_secondary
                    } else {
                        t.text_muted
                    }))
                    .child(label),
            )
    }

    // ── Commit tab action handlers ─────────────────────────────────

    fn handle_stage_file(&mut self, path: &str, cx: &mut Context<Self>) {
        let provider = self.git_provider.clone();
        let path_str = path.to_string();
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let result = smol::unblock(move || provider.stage_file(&path_str)).await;
            let _ = this.update(cx, |this, cx| {
                if let Err(e) = result {
                    this.last_error = Some(e);
                } else {
                    this.last_error = None;
                }
                this.refresh_working_tree_status(cx);
            });
        })
        .detach();
    }

    fn handle_unstage_file(&mut self, path: &str, cx: &mut Context<Self>) {
        let provider = self.git_provider.clone();
        let path_str = path.to_string();
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let result = smol::unblock(move || provider.unstage_file(&path_str)).await;
            let _ = this.update(cx, |this, cx| {
                if let Err(e) = result {
                    this.last_error = Some(e);
                } else {
                    this.last_error = None;
                }
                this.refresh_working_tree_status(cx);
            });
        })
        .detach();
    }

    pub fn handle_stage_all(&mut self, cx: &mut Context<Self>) {
        let provider = self.git_provider.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let result = smol::unblock(move || provider.stage_all()).await;
            let _ = this.update(cx, |this, cx| {
                if let Err(e) = result {
                    this.last_error = Some(e);
                } else {
                    this.last_error = None;
                }
                this.refresh_working_tree_status(cx);
            });
        })
        .detach();
    }

    pub fn handle_unstage_all(&mut self, cx: &mut Context<Self>) {
        let provider = self.git_provider.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let result = smol::unblock(move || provider.unstage_all()).await;
            let _ = this.update(cx, |this, cx| {
                if let Err(e) = result {
                    this.last_error = Some(e);
                } else {
                    this.last_error = None;
                }
                this.refresh_working_tree_status(cx);
            });
        })
        .detach();
    }

    fn handle_commit(&mut self, cx: &mut Context<Self>) {
        let message = self.commit_message_input.read(cx).value().to_string();
        if message.is_empty() {
            return;
        }
        let provider = self.git_provider.clone();
        let amend = self.commit_options_amend;
        let signoff = self.commit_options_signoff;

        // If nothing is staged but tracked files have changes, auto-stage them
        // before committing — matches the "Commit Tracked" semantics on the button.
        // Untracked files are intentionally left out.
        let needs_auto_stage = self
            .working_tree_status
            .as_ref()
            .map(|s| s.staged_count() == 0 && !s.tracked.is_empty())
            .unwrap_or(false);
        let tracked_paths: Vec<String> = if needs_auto_stage {
            self.working_tree_status
                .as_ref()
                .map(|s| s.tracked.iter().map(|f| f.path.clone()).collect())
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        self.committing = true;
        cx.notify();

        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let result = smol::unblock(move || -> Result<(), String> {
                if needs_auto_stage {
                    for p in &tracked_paths {
                        provider.stage_file(p)?;
                    }
                }
                provider.commit(&message, amend, signoff)
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                this.committing = false;
                if let Err(e) = result {
                    this.last_error = Some(e);
                } else {
                    this.last_error = None;
                    // Clear message input and amend flag after successful commit
                    this.commit_message_input.update(cx, |input, cx| {
                        input.set_value("", cx);
                    });
                    this.commit_options_amend = false;
                    // Refresh commit log too since a new commit exists
                    this.refresh_after_commit(cx);
                }
                this.refresh_working_tree_status(cx);
            });
        })
        .detach();
    }

    fn handle_uncommit(&mut self, cx: &mut Context<Self>) {
        if self.uncommitting || self.latest_commit().is_none() {
            return;
        }

        self.uncommitting = true;
        self.last_error = None;
        cx.notify();

        let provider = self.git_provider.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let result = smol::unblock(move || provider.uncommit()).await;
            let _ = this.update(cx, |this, cx| {
                this.uncommitting = false;
                if let Err(e) = result {
                    this.last_error = Some(e);
                } else {
                    this.last_error = None;
                    this.refresh_after_commit(cx);
                }
                this.refresh_working_tree_status(cx);
            });
        })
        .detach();
    }

    pub fn handle_stash_all(&mut self, cx: &mut Context<Self>) {
        let provider = self.git_provider.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let result = smol::unblock(move || provider.stash_all()).await;
            let _ = this.update(cx, |this, cx| {
                if let Err(e) = result {
                    this.last_error = Some(e);
                } else {
                    this.last_error = None;
                }
                this.refresh_working_tree_status(cx);
            });
        })
        .detach();
    }

    pub fn handle_stash_pop(&mut self, cx: &mut Context<Self>) {
        let provider = self.git_provider.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let result = smol::unblock(move || provider.stash_pop()).await;
            let _ = this.update(cx, |this, cx| {
                if let Err(e) = result {
                    this.last_error = Some(e);
                } else {
                    this.last_error = None;
                }
                this.refresh_working_tree_status(cx);
            });
        })
        .detach();
    }

    pub fn handle_stash_apply(&mut self, index: usize, cx: &mut Context<Self>) {
        let provider = self.git_provider.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let result = smol::unblock(move || provider.stash_apply(index)).await;
            let _ = this.update(cx, |this, cx| {
                if let Err(e) = result {
                    this.last_error = Some(e);
                } else {
                    this.last_error = None;
                }
                this.refresh_working_tree_status(cx);
            });
        })
        .detach();
    }

    pub fn handle_stash_drop(&mut self, index: usize, cx: &mut Context<Self>) {
        let provider = self.git_provider.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let result = smol::unblock(move || provider.stash_drop(index)).await;
            let _ = this.update(cx, |this, cx| {
                if let Err(e) = result {
                    this.last_error = Some(e);
                } else {
                    this.last_error = None;
                }
                this.refresh_working_tree_status(cx);
            });
        })
        .detach();
    }

    pub fn handle_discard_all_tracked(&mut self, cx: &mut Context<Self>) {
        let provider = self.git_provider.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let result = smol::unblock(move || provider.discard_all_tracked()).await;
            let _ = this.update(cx, |this, cx| {
                if let Err(e) = result {
                    this.last_error = Some(e);
                } else {
                    this.last_error = None;
                }
                this.refresh_working_tree_status(cx);
            });
        })
        .detach();
    }

    fn handle_fetch(&mut self, cx: &mut Context<Self>) {
        self.run_remote_op(RemoteOp::Fetch, cx);
    }

    fn handle_pull(&mut self, cx: &mut Context<Self>) {
        self.run_remote_op(RemoteOp::Pull, cx);
    }

    fn handle_push(&mut self, cx: &mut Context<Self>) {
        self.run_remote_op(RemoteOp::Push, cx);
    }

    fn run_remote_op(&mut self, op: RemoteOp, cx: &mut Context<Self>) {
        if self.remote_op_running.is_some() {
            return;
        }
        self.remote_op_running = Some(op);
        self.last_error = None;
        cx.notify();

        let provider = self.git_provider.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let result = smol::unblock(move || match op {
                RemoteOp::Fetch => provider.fetch(),
                RemoteOp::Pull => provider.pull(),
                RemoteOp::Push => provider.push(),
            })
            .await;

            let _ = this.update(cx, |this, cx| {
                this.remote_op_running = None;
                if let Err(e) = result {
                    this.last_error = Some(e);
                }
                this.refresh_working_tree_status(cx);
            });
        })
        .detach();
    }

    /// Reload the commit log in place (watcher-driven: a commit was made
    /// from outside, a rebase finished, HEAD moved). Refetches everything
    /// currently loaded so pagination depth and scroll position survive.
    pub fn refresh_commit_log(&mut self, cx: &mut Context<Self>) {
        if !self.commit_log_visible || self.commit_log_loading {
            return;
        }
        let provider = self.git_provider.clone();
        let branch = self.commit_log_branch.clone();
        let count = self.commit_log_entries.len().max(COMMIT_PAGE_SIZE);
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let entries =
                smol::unblock(move || provider.get_commit_log(0, count, branch.as_deref())).await;
            let _ = this.update(cx, |this, cx| {
                this.commit_log_has_more = entries.len() >= count;
                this.commit_log_entries = entries;
                cx.notify();
            });
        })
        .detach();
    }

    /// Refresh both commit log and working tree status after a new commit.
    fn refresh_after_commit(&mut self, cx: &mut Context<Self>) {
        let provider = self.git_provider.clone();
        let page = COMMIT_PAGE_SIZE;
        let branch = self.commit_log_branch.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let entries =
                smol::unblock(move || provider.get_commit_log(0, page, branch.as_deref())).await;
            let _ = this.update(cx, |this, cx| {
                this.commit_log_has_more = entries.len() >= page;
                // The new commit prepends — reload the first page and jump to
                // the top (the user just committed; showing it is the right
                // outcome).
                this.commit_log_entries = entries;
                this.reset_history_scroll();
                cx.notify();
            });
        })
        .detach();
    }

    /// Render the History tab (commit log with graph).
    fn render_history_tab(&self, t: &ThemeColors, cx: &mut Context<Self>) -> AnyElement {
        let branch_name = self.current_branch.clone();

        let compare_bar = if self.commit_log_compare_mode {
            self.render_compare_bar(t, cx).into_any_element()
        } else {
            div().size_0().into_any_element()
        };
        let branch_picker = if self.commit_log_branch_picker {
            self.render_branch_picker(t, cx).into_any_element()
        } else {
            div().size_0().into_any_element()
        };

        // Placeholder for the empty/initial-loading states; `None` renders
        // the virtualized commit list instead.
        let placeholder: Option<AnyElement> = if self.commit_log_entries.is_empty() {
            let text = if self.commit_log_loading {
                "Loading\u{2026}"
            } else {
                "No commits"
            };
            Some(
                div()
                    .px(px(14.0))
                    .py(px(16.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .text_size(ui_text_ms(cx))
                            .text_color(rgb(t.text_muted))
                            .child(text),
                    )
                    .into_any_element(),
            )
        } else {
            None
        };

        v_flex()
            .flex_1()
            .min_h_0()
            // Header with branch selector and compare toggle
            .child(self.render_commit_log_header(branch_name, t, cx))
            // Compare bar (conditional)
            .child(compare_bar)
            // Branch picker (conditional)
            .child(branch_picker)
            // Uniform virtualized commit list: rows are all COMMIT_ROW_H tall,
            // so only the visible range is ever built and the scrollbar is
            // exact from the row count — no measuring pass.
            .child(match placeholder {
                Some(placeholder) => div()
                    .flex_1()
                    .min_h_0()
                    .py(px(4.0))
                    .child(placeholder)
                    .into_any_element(),
                None => {
                    let entity = cx.entity().clone();
                    let row_theme = *t;
                    let count = self.commit_log_entries.len();
                    let scrollbar_geometry = get_scrollbar_geometry(&self.history_scroll);
                    div()
                        .id("git-panel-scroll")
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .py(px(4.0))
                        .child(
                            uniform_list("git-history-list", count, move |range, _window, cx| {
                                // Approaching the tail: fetch the next page.
                                // Deferred because the entity is mid-layout;
                                // load_more_commits guards re-entry.
                                let near_end = {
                                    let this = entity.read(cx);
                                    range.end + 20 >= this.commit_log_entries.len()
                                        && this.commit_log_has_more
                                        && !this.commit_log_loading
                                };
                                if near_end {
                                    let entity = entity.clone();
                                    cx.defer(move |cx| {
                                        entity.update(cx, |this, cx| this.load_more_commits(cx));
                                    });
                                }
                                entity.update(cx, |this, cx| {
                                    range
                                        .map(|ix| this.render_history_row(ix, &row_theme, cx))
                                        .collect()
                                })
                            })
                            .size_full()
                            .track_scroll(&self.history_scroll),
                        )
                        .when(scrollbar_geometry.is_some(), |d| {
                            let (_, _, thumb_y, thumb_height) =
                                scrollbar_geometry.expect("guarded by is_some() in when()");
                            d.child(self.render_history_scrollbar(thumb_y, thumb_height, t, cx))
                        })
                        .into_any_element()
                }
            })
            .into_any_element()
    }

    /// One uniform-height commit row of the History tab: message + ref
    /// pills + author. Click opens the commit in the diff viewer with
    /// prev/next navigation over the loaded log.
    fn render_history_row(&self, ix: usize, t: &ThemeColors, cx: &mut Context<Self>) -> AnyElement {
        let Some(entry) = self.commit_log_entries.get(ix) else {
            return div().into_any_element();
        };

        h_flex()
            .id(ElementId::Name(format!("history-row-{}", ix).into()))
            // Inset rounded hover pill — matches the sidebar's rows.
            .mx(px(6.0))
            .pl(px(8.0))
            .pr(px(12.0))
            .h(px(project_header::COMMIT_ROW_H))
            .items_center()
            .gap(px(6.0))
            .cursor_pointer()
            .rounded_lg()
            .hover(|s| s.bg(rgb(t.bg_hover)))
            .on_click(cx.listener(move |this, _, _window, cx| {
                this.open_commit_diff(ix, cx);
            }))
            .child(
                div()
                    .text_size(ui_text_md(cx))
                    .text_color(rgb(t.text_primary))
                    .text_ellipsis()
                    .overflow_hidden()
                    .flex_shrink_1()
                    .min_w_0()
                    .child(entry.message.clone()),
            )
            .children(
                entry
                    .refs
                    .iter()
                    .map(|r| project_header::render_ref_label(r, t, cx)),
            )
            .child(
                div()
                    .text_size(ui_text_ms(cx))
                    .text_color(rgb(t.text_muted))
                    .flex_shrink_0()
                    .child(entry.author.clone()),
            )
            .into_any_element()
    }

    /// Open the commit at `ix` in the diff viewer overlay, with the loaded
    /// log as the prev/next navigation list.
    fn open_commit_diff(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.commit_log_entries.get(ix) else {
            return;
        };
        let commit_hash = entry.hash.clone();
        let commit_msg = entry.message.clone();
        let commits_vec = self.commit_log_entries.clone();
        let project_id = self.project_id.clone();
        self.request_broker.update(cx, |broker, cx| {
            broker.push_overlay_request(
                OverlayRequest::DiffViewer {
                    project_id,
                    file: None,
                    mode: Some(DiffMode::Commit(commit_hash)),
                    commit_message: Some(commit_msg),
                    commits: Some(commits_vec),
                    commit_index: Some(ix),
                },
                cx,
            );
        });
    }

    /// Draggable scrollbar for the history list (uniform-list geometry).
    fn render_history_scrollbar(
        &self,
        thumb_y: f32,
        thumb_height: f32,
        t: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let is_dragging = self.history_scrollbar_drag.is_some();
        let t = *t;
        div()
            .id("git-history-scrollbar-track")
            .absolute()
            .top_0()
            .bottom_0()
            .right_0()
            .w(px(12.0))
            .cursor(CursorStyle::Arrow)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                    if get_scrollbar_geometry(&this.history_scroll).is_some() {
                        let mut drag = start_scrollbar_drag(&this.history_scroll);
                        drag.start_y = f32::from(event.position.y);
                        this.history_scrollbar_drag = Some(drag);
                        cx.notify();
                    }
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                if let Some(drag) = this.history_scrollbar_drag {
                    update_scrollbar_drag(
                        &this.history_scroll,
                        drag,
                        f32::from(event.position.y),
                    );
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _window, cx| {
                    this.history_scrollbar_drag = None;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .absolute()
                    .top(px(thumb_y))
                    .right(px(3.0))
                    .w(px(6.0))
                    .h(px(thumb_height))
                    .rounded(px(3.0))
                    .bg(rgb(if is_dragging {
                        t.scrollbar_hover
                    } else {
                        t.scrollbar
                    }))
                    .hover(move |s| s.bg(rgb(t.scrollbar_hover))),
            )
    }

    /// Render the commit log header bar (GRAPH label, compare toggle, branch selector).
    fn render_commit_log_header(
        &self,
        branch_name: Option<String>,
        t: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let display_branch = self.commit_log_branch.clone().or(branch_name);
        let is_compare = self.commit_log_compare_mode;
        h_flex()
            .px(px(10.0))
            .py(px(6.0))
            .gap(px(6.0))
            .items_center()
            .border_b_1()
            .border_color(rgb(t.border))
            .child(
                svg()
                    .path("icons/git-commit.svg")
                    .size(px(11.0))
                    .text_color(rgb(t.text_muted)),
            )
            .child(
                div()
                    .text_size(ui_text_ms(cx))
                    .text_color(rgb(t.text_secondary))
                    .child("COMMITS"),
            )
            // Right side: Compare toggle + branch selector
            .child({
                h_flex()
                    .flex_1()
                    .justify_end()
                    .gap(px(4.0))
                    .items_center()
                    // Compare toggle
                    .child(
                        div()
                            .id("commit-log-compare-toggle")
                            .cursor_pointer()
                            .px(px(6.0))
                            .py(px(2.0))
                            .rounded(px(4.0))
                            .bg(rgb(if is_compare {
                                t.bg_selection
                            } else {
                                t.bg_hover
                            }))
                            .hover(|s| s.bg(rgb(t.bg_selection)))
                            .text_size(ui_text_sm(cx))
                            .text_color(rgb(if is_compare {
                                t.term_cyan
                            } else {
                                t.text_muted
                            }))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                cx.stop_propagation();
                            })
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.commit_log_compare_mode = !this.commit_log_compare_mode;
                                if this.commit_log_compare_mode {
                                    this.commit_log_compare_base = this.current_branch.clone();
                                    this.commit_log_compare_head = this.commit_log_branch.clone();
                                }
                                this.commit_log_branch_picker = false;
                                cx.notify();
                            }))
                            .child("Compare"),
                    )
                    // Branch selector pill (only when not in compare mode)
                    .when(!is_compare, |d| {
                        d.when_some(display_branch, |d, name| {
                            d.child(
                                h_flex()
                                    .id("commit-log-branch-btn")
                                    .gap(px(4.0))
                                    .items_center()
                                    .px(px(6.0))
                                    .py(px(2.0))
                                    .rounded(px(4.0))
                                    .bg(rgb(t.bg_hover))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgb(t.bg_selection)))
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation();
                                    })
                                    .on_click(cx.listener(|this, _, _window, cx| {
                                        this.commit_log_picker_target = BranchPickerTarget::Graph;
                                        this.commit_log_branch_picker =
                                            !this.commit_log_branch_picker;
                                        this.commit_log_branch_filter.clear();
                                        cx.notify();
                                    }))
                                    .child(
                                        svg()
                                            .path("icons/git-branch.svg")
                                            .size(px(10.0))
                                            .text_color(rgb(t.term_green)),
                                    )
                                    .child(
                                        div()
                                            .text_size(ui_text_sm(cx))
                                            .text_color(rgb(t.text_secondary))
                                            .max_w(px(140.0))
                                            .text_ellipsis()
                                            .overflow_hidden()
                                            .child(name),
                                    ),
                            )
                        })
                    })
            })
    }

    /// Render the compare bar (two branch selectors + view diff button).
    fn render_compare_bar(&self, t: &ThemeColors, cx: &mut Context<Self>) -> impl IntoElement {
        let base = self.commit_log_compare_base.clone();
        let head = self.commit_log_compare_head.clone();
        let pid = self.project_id.clone();
        let broker = self.request_broker.clone();
        let both_selected = base.is_some() && head.is_some();
        h_flex()
            .px(px(10.0))
            .py(px(6.0))
            .gap(px(6.0))
            .items_center()
            .border_b_1()
            .border_color(rgb(t.border))
            // Base branch pill
            .child(
                div()
                    .id("compare-base-btn")
                    .cursor_pointer()
                    .px(px(6.0))
                    .py(px(2.0))
                    .rounded(px(4.0))
                    .bg(rgb(t.bg_hover))
                    .hover(|s| s.bg(rgb(t.bg_selection)))
                    .text_size(ui_text_sm(cx))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.commit_log_picker_target = BranchPickerTarget::CompareBase;
                        this.commit_log_branch_picker = !this.commit_log_branch_picker;
                        this.commit_log_branch_filter.clear();
                        cx.notify();
                    }))
                    .child(
                        h_flex()
                            .gap(px(3.0))
                            .items_center()
                            .child(
                                svg()
                                    .path("icons/git-branch.svg")
                                    .size(px(9.0))
                                    .text_color(rgb(t.term_green)),
                            )
                            .child(
                                div()
                                    .text_color(rgb(t.text_secondary))
                                    .max_w(px(120.0))
                                    .text_ellipsis()
                                    .overflow_hidden()
                                    .child(base.clone().unwrap_or_else(|| "base...".to_string())),
                            ),
                    ),
            )
            // Arrow
            .child(
                div()
                    .text_size(ui_text_sm(cx))
                    .text_color(rgb(t.text_muted))
                    .child("\u{2192}"),
            )
            // Head branch pill
            .child(
                div()
                    .id("compare-head-btn")
                    .cursor_pointer()
                    .px(px(6.0))
                    .py(px(2.0))
                    .rounded(px(4.0))
                    .bg(rgb(t.bg_hover))
                    .hover(|s| s.bg(rgb(t.bg_selection)))
                    .text_size(ui_text_sm(cx))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.commit_log_picker_target = BranchPickerTarget::CompareHead;
                        this.commit_log_branch_picker = !this.commit_log_branch_picker;
                        this.commit_log_branch_filter.clear();
                        cx.notify();
                    }))
                    .child(
                        h_flex()
                            .gap(px(3.0))
                            .items_center()
                            .child(
                                svg()
                                    .path("icons/git-branch.svg")
                                    .size(px(9.0))
                                    .text_color(rgb(t.term_cyan)),
                            )
                            .child(
                                div()
                                    .text_color(rgb(t.text_secondary))
                                    .max_w(px(120.0))
                                    .text_ellipsis()
                                    .overflow_hidden()
                                    .child(head.clone().unwrap_or_else(|| "head...".to_string())),
                            ),
                    ),
            )
            // View Diff button
            .child(
                div().flex_1().flex().justify_end().child(
                    div()
                        .id("compare-view-diff")
                        .cursor_pointer()
                        .px(px(8.0))
                        .py(px(3.0))
                        .rounded(px(4.0))
                        .when(both_selected, |d| {
                            d.bg(rgb(t.term_cyan))
                                .text_color(rgb(t.bg_primary))
                                .hover(|s| s.opacity(0.9))
                        })
                        .when(!both_selected, |d| {
                            d.bg(rgb(t.bg_hover)).text_color(rgb(t.text_muted))
                        })
                        .text_size(ui_text_sm(cx))
                        .font_weight(FontWeight::MEDIUM)
                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .when(both_selected, |d| {
                            d.on_click(cx.listener(move |this, _, _window, cx| {
                                let base = this
                                    .commit_log_compare_base
                                    .clone()
                                    .expect("both_selected implies compare_base is Some");
                                let head = this
                                    .commit_log_compare_head
                                    .clone()
                                    .expect("both_selected implies compare_head is Some");
                                broker.update(cx, |broker, cx| {
                                    broker.push_overlay_request(
                                        OverlayRequest::DiffViewer {
                                            project_id: pid.clone(),
                                            file: None,
                                            mode: Some(DiffMode::BranchCompare { base, head }),
                                            commit_message: None,
                                            commits: None,
                                            commit_index: None,
                                        },
                                        cx,
                                    );
                                });
                            }))
                        })
                        .child("View Diff"),
                ),
            )
    }

    /// Render the branch picker panel.
    fn render_branch_picker(&self, t: &ThemeColors, cx: &mut Context<Self>) -> impl IntoElement {
        let filter = self.commit_log_branch_filter.to_lowercase();
        let filtered: Vec<&String> = self
            .commit_log_branches
            .iter()
            .filter(|b| filter.is_empty() || b.to_lowercase().contains(&filter))
            .collect();
        v_flex()
            .border_b_1()
            .border_color(rgb(t.border))
            .max_h(px(200.0))
            // Filter input
            .child(
                div().px(px(10.0)).py(px(6.0)).child(
                    div()
                        .px(px(8.0))
                        .py(px(4.0))
                        .rounded(px(4.0))
                        .bg(rgb(t.bg_secondary))
                        .text_size(ui_text_ms(cx))
                        .text_color(rgb(t.text_primary))
                        .child(if filter.is_empty() {
                            format!("{} branches", self.commit_log_branches.len())
                        } else {
                            format!(
                                "\"{}\" \u{2014} {} matches",
                                self.commit_log_branch_filter,
                                filtered.len()
                            )
                        }),
                ),
            )
            // Branch list
            .child(
                div()
                    .id("branch-picker-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(filtered.iter().enumerate().map(|(i, branch)| {
                        let b = (*branch).clone();
                        let target = self.commit_log_picker_target;
                        let is_selected = match target {
                            BranchPickerTarget::Graph => {
                                self.commit_log_branch.as_ref() == Some(*branch)
                            }
                            BranchPickerTarget::CompareBase => {
                                self.commit_log_compare_base.as_ref() == Some(*branch)
                            }
                            BranchPickerTarget::CompareHead => {
                                self.commit_log_compare_head.as_ref() == Some(*branch)
                            }
                        };
                        div()
                            .id(ElementId::Name(format!("branch-{}-{}", i, branch).into()))
                            .px(px(10.0))
                            .py(px(3.0))
                            .cursor_pointer()
                            .text_size(ui_text_ms(cx))
                            .text_color(rgb(if is_selected {
                                t.text_primary
                            } else {
                                t.text_secondary
                            }))
                            .when(is_selected, |d| d.font_weight(FontWeight::SEMIBOLD))
                            .hover(|s| s.bg(rgb(t.bg_hover)))
                            .on_click(cx.listener(move |this, _, _window, cx| match target {
                                BranchPickerTarget::Graph => {
                                    this.switch_commit_log_branch(Some(b.clone()), cx);
                                }
                                BranchPickerTarget::CompareBase => {
                                    this.commit_log_compare_base = Some(b.clone());
                                    this.commit_log_branch_picker = false;
                                    cx.notify();
                                }
                                BranchPickerTarget::CompareHead => {
                                    this.commit_log_compare_head = Some(b.clone());
                                    this.commit_log_branch_picker = false;
                                    cx.notify();
                                }
                            }))
                            .child((*branch).clone())
                            .into_any_element()
                    })),
            )
    }

}

/// Button label for the commit footer.
///
/// Captures the small state machine that the user sees: which of
/// "Commit" / "Commit Tracked" / "Amend" / "Amend Tracked" / "Committing..."
/// is shown depends on whether we're already in flight, whether the user
/// asked for an amend, and whether they've staged anything.
fn commit_button_label(
    committing: bool,
    amend: bool,
    has_staged: bool,
    has_tracked: bool,
) -> &'static str {
    if committing {
        return "Committing...";
    }
    match (amend, has_staged, has_tracked) {
        (true, false, true) => "Amend Tracked",
        (true, _, _) => "Amend",
        (false, true, _) => "Commit",
        (false, false, true) => "Commit Tracked",
        (false, false, false) => "Commit",
    }
}

#[cfg(test)]
mod elide_tests {
    use super::elide_dir_front;

    #[test]
    fn keeps_short_paths_unchanged() {
        assert_eq!(elide_dir_front("src/", 10), "src/");
        assert_eq!(elide_dir_front("src/foo/", 8), "src/foo/");
    }

    #[test]
    fn front_elides_long_paths_keeping_the_tail() {
        let dir = "crates/notmux-views-terminal/src/layout/tabs/";
        let out = elide_dir_front(dir, 20);
        assert!(out.starts_with('…'), "expected leading ellipsis: {out}");
        assert!(out.ends_with("layout/tabs/"), "tail must survive: {out}");
        // Budget respected: ellipsis + (max-1) tail chars.
        assert_eq!(out.chars().count(), 20);
    }

    #[test]
    fn zero_budget_is_noop() {
        assert_eq!(elide_dir_front("a/b/c/", 0), "a/b/c/");
    }
}

#[cfg(test)]
mod commit_label_tests {
    use super::commit_button_label;

    #[test]
    fn committing_overrides_everything() {
        for &amend in &[false, true] {
            for &staged in &[false, true] {
                for &tracked in &[false, true] {
                    assert_eq!(
                        commit_button_label(true, amend, staged, tracked),
                        "Committing...",
                    );
                }
            }
        }
    }

    // arg order: committing, amend, has_staged, has_tracked

    #[test]
    fn no_amend_no_staged_with_tracked_says_commit_tracked() {
        assert_eq!(
            commit_button_label(false, false, false, true),
            "Commit Tracked"
        );
    }

    #[test]
    fn no_amend_with_staged_says_commit() {
        assert_eq!(commit_button_label(false, false, true, true), "Commit");
        assert_eq!(commit_button_label(false, false, true, false), "Commit");
    }

    #[test]
    fn no_amend_no_changes_says_commit() {
        assert_eq!(commit_button_label(false, false, false, false), "Commit");
    }

    #[test]
    fn amend_no_staged_with_tracked_says_amend_tracked() {
        assert_eq!(
            commit_button_label(false, true, false, true),
            "Amend Tracked"
        );
    }

    #[test]
    fn amend_with_staged_says_amend() {
        assert_eq!(commit_button_label(false, true, true, true), "Amend");
        assert_eq!(commit_button_label(false, true, true, false), "Amend");
    }

    #[test]
    fn amend_no_changes_says_amend() {
        assert_eq!(commit_button_label(false, true, false, false), "Amend");
    }
}
