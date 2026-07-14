//! Git diff viewer overlay.
//!
//! Provides a read-only view of git diffs with working/staged toggle,
//! file tree sidebar, syntax highlighting, and selection support.

mod line_render;
pub mod provider;
mod render;
mod scrollbar;
pub(crate) mod syntax;
pub(crate) mod types;

use gpui::prelude::*;
use gpui::*;
use std::collections::HashSet;
use std::sync::Arc;
use syntect::parsing::SyntaxSet;
use notmux_core::selection::SelectionState;
use notmux_core::types::DiffViewMode;
use notmux_files::code_view::extract_selected_text;
use notmux_files::file_tree::build_file_tree;
use notmux_files::selection::{Selection2DNonEmpty, copy_to_clipboard};
use notmux_files::syntax::{
    build_syntax_theme, default_text_color_for, get_syntax_for_path, load_syntax_set,
};
// The git bridge, NOT the app-wide terminal bridge: it maps selection,
// syntax palette, and status colors from the app theme, so the diff view
// matches the git/files panels instead of the terminal chrome.
use notmux_ui::theme::git_theme as theme;
use notmux_git::{CommitLogEntry, DiffMode, DiffResult, FileDiff};
use notmux_ui::modal::fullscreen_overlay;

use syntax::process_file;
use types::{
    DiffDisplayFile, DisplayItem, FileStats, FileTreeNode, HScrollbarDrag, ScrollbarDrag,
    SideBySideLine, SideBySideSide,
};

mod side_by_side;

// Re-export for use in settings (and use locally)
pub use types::DiffViewMode as DiffViewModeReexport;

gpui::actions!(notmux_git, [Cancel]);

/// Type alias for diff selection (line index, column).
type Selection = SelectionState<(usize, usize)>;

/// Width of file tree sidebar.
const SIDEBAR_WIDTH: f32 = 240.0;

use crate::settings::{git_settings, set_git_settings};

/// Git diff viewer overlay.
pub struct DiffViewer {
    focus_handle: FocusHandle,
    diff_mode: DiffMode,
    view_mode: DiffViewMode,
    /// Ignore whitespace changes in diff.
    ignore_whitespace: bool,
    /// Provider for fetching diff data (local or remote).
    provider: Arc<dyn provider::GitProvider>,
    /// Whether diff data is currently being loaded.
    loading: bool,
    /// Raw diff data for all files (not syntax highlighted).
    raw_files: Vec<FileDiff>,
    /// Lightweight file stats for sidebar display.
    file_stats: Vec<FileStats>,
    /// Currently processed file with syntax highlighting (lazy loaded).
    current_file: Option<DiffDisplayFile>,
    file_tree: FileTreeNode,
    expanded_folders: HashSet<String>,
    selected_file_index: usize,
    selection: Selection,
    scroll_handle: UniformListScrollHandle,
    tree_scroll_handle: ScrollHandle,
    error_message: Option<String>,
    line_num_width: usize,
    syntax_set: SyntaxSet,
    scrollbar_drag: Option<ScrollbarDrag>,
    file_font_size: f32,
    /// Cached side-by-side lines for current file.
    side_by_side_lines: Vec<SideBySideLine>,
    /// Horizontal scroll offset in pixels.
    scroll_x: f32,
    /// Maximum line length in characters (for horizontal scroll range).
    max_line_chars: usize,
    /// Real pixel width of the widest line, measured via the text system (0 =
    /// not measured yet). Used instead of a char-count estimate, which is wrong
    /// when the font isn't truly monospace.
    measured_content_width: f32,
    /// Cached diff pane viewport width (updated from scroll handle geometry).
    diff_pane_width: f32,
    /// Horizontal scrollbar drag state.
    h_scrollbar_drag: Option<HScrollbarDrag>,
    /// Which side of the side-by-side view the current selection belongs to.
    selection_side: Option<SideBySideSide>,
    /// Measured monospace character width (from font metrics).
    measured_char_width: f32,
    /// Whether the current theme is dark (retained for legacy header tinting).
    is_dark: bool,
    /// Snapshot of the active theme palette; syntax highlighting is derived
    /// from this so the diff view matches the rest of the UI.
    theme_colors: notmux_core::theme::ThemeColors,
    /// Cached old file content for re-highlighting on theme change.
    current_file_old_content: Option<String>,
    /// Cached new file content for re-highlighting on theme change.
    current_file_new_content: Option<String>,
    /// Commit message for display when viewing a commit diff.
    commit_message: Option<String>,
    /// List of commits for prev/next navigation.
    commits: Vec<CommitLogEntry>,
    /// Current index in the commits list.
    commit_index: usize,
    /// When true, the entity renders via `render_embedded` (no fullscreen
    /// overlay / file tree) so it can be embedded inline in another view.
    embedded: bool,
}

impl DiffViewer {
    /// Create a new diff viewer with the given provider, optionally selecting a specific file, mode, commit message, and commit navigation list.
    pub fn new(
        provider: Arc<dyn provider::GitProvider>,
        select_file: Option<String>,
        mode: Option<DiffMode>,
        commit_message: Option<String>,
        commits: Option<Vec<CommitLogEntry>>,
        commit_index: Option<usize>,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let gs = git_settings(cx);
        let font_size = gs.diff_font_size;
        let view_mode = gs.diff_view_mode;
        let ignore_whitespace = gs.diff_ignore_whitespace;
        let is_dark = gs.is_dark;
        let theme_colors = theme(cx);

        let mut viewer = Self {
            focus_handle,
            diff_mode: DiffMode::WorkingTree,
            view_mode,
            ignore_whitespace,
            provider: provider.clone(),
            loading: false,
            raw_files: Vec::new(),
            file_stats: Vec::new(),
            current_file: None,
            file_tree: FileTreeNode::default(),
            expanded_folders: HashSet::new(),
            selected_file_index: 0,
            selection: Selection::default(),
            scroll_handle: UniformListScrollHandle::new(),
            tree_scroll_handle: ScrollHandle::new(),
            error_message: None,
            line_num_width: 4,
            syntax_set: load_syntax_set(),
            scrollbar_drag: None,
            file_font_size: font_size,
            side_by_side_lines: Vec::new(),
            scroll_x: 0.0,
            max_line_chars: 0,
            measured_content_width: 0.0,
            diff_pane_width: 0.0,
            h_scrollbar_drag: None,
            selection_side: None,
            measured_char_width: font_size * 0.6,
            is_dark,
            theme_colors,
            current_file_old_content: None,
            current_file_new_content: None,
            commit_message,
            commits: commits.unwrap_or_default(),
            commit_index: commit_index.unwrap_or(0),
            embedded: false,
        };

        if !provider.is_git_repo() {
            viewer.error_message = Some("Not a git repository".to_string());
            return viewer;
        }

        viewer.load_diff_async(mode.unwrap_or(DiffMode::WorkingTree), select_file, cx);
        viewer
    }

    /// Mark this viewer as embedded (renders via `render_embedded` — no
    /// fullscreen overlay or file-tree sidebar — for inline use in the panel).
    pub fn set_embedded(&mut self, embedded: bool) {
        self.embedded = embedded;
    }

    // ── Host-virtualized inline rendering ──────────────────────────────
    // A host (e.g. the git panel) can place the diff in its own viewported
    // list and render one item at a time, so the diff shows at full natural
    // height with only the visible lines realized (Warp-style virtualization),
    // while keeping the real per-line rendering (selection, syntax, clickable
    // context expanders).

    /// Number of display items (diff lines + context expanders) in the file.
    pub fn inline_item_count(&self) -> usize {
        self.current_file.as_ref().map(|f| f.items.len()).unwrap_or(0)
    }

    /// True while the diff is still being fetched/highlighted (no items yet,
    /// no error). The host shows a single stable "Loading…" row for this.
    pub fn inline_loading(&self) -> bool {
        self.current_file.is_none() && self.error_message.is_none()
    }

    /// The load error, if any.
    pub fn inline_error(&self) -> Option<String> {
        self.error_message.clone()
    }

    /// Whether hunks can be staged/unstaged in the current mode (only the
    /// working tree and the index are mutable; commit / branch-compare diffs
    /// are read-only).
    pub(super) fn hunk_staging_available(&self) -> bool {
        matches!(self.diff_mode, DiffMode::WorkingTree | DiffMode::Staged)
    }

    /// Button label for the current mode: staged hunks unstage, working-tree
    /// hunks stage.
    pub(super) fn hunk_stage_label(&self) -> &'static str {
        if matches!(self.diff_mode, DiffMode::Staged) {
            "Unstage"
        } else {
            "Stage"
        }
    }

    /// Path of the file currently shown, used to address hunk staging.
    fn current_file_path(&self) -> Option<String> {
        self.raw_files
            .get(self.selected_file_index)
            .and_then(|f| f.new_path.clone().or_else(|| f.old_path.clone()))
    }

    /// Zero-based hunk ordinal for the header display item at `item_idx`
    /// (headers appear in `items` in the same order as `FileDiff::hunks`).
    pub(super) fn hunk_index_for_item(&self, item_idx: usize) -> Option<usize> {
        let file = self.current_file.as_ref()?;
        if !matches!(
            file.items.get(item_idx),
            Some(DisplayItem::Line(l)) if l.line_type == notmux_git::DiffLineType::Header
        ) {
            return None;
        }
        let ordinal = file.items[..item_idx]
            .iter()
            .filter(|it| {
                matches!(it, DisplayItem::Line(l) if l.line_type == notmux_git::DiffLineType::Header)
            })
            .count();
        Some(ordinal)
    }

    /// Zero-based hunk ordinal for the side-by-side header row at `sbs_idx`.
    pub(super) fn hunk_index_for_sbs(&self, sbs_idx: usize) -> Option<usize> {
        if !self.side_by_side_lines.get(sbs_idx)?.is_header {
            return None;
        }
        Some(
            self.side_by_side_lines[..sbs_idx]
                .iter()
                .filter(|l| l.is_header)
                .count(),
        )
    }

    /// Stage (working-tree mode) or unstage (staged mode) the given hunk, then
    /// reload the diff and notify the owner so its file list refreshes.
    pub(super) fn toggle_hunk_stage(&mut self, hunk_index: usize, cx: &mut Context<Self>) {
        let Some(file_path) = self.current_file_path() else {
            return;
        };
        let reverse = matches!(self.diff_mode, DiffMode::Staged);
        let mode = self.diff_mode.clone();
        let provider = self.provider.clone();
        cx.spawn(async move |this, cx| {
            let path = file_path.clone();
            let result = smol::unblock(move || {
                if reverse {
                    provider.unstage_hunk(&path, hunk_index)
                } else {
                    provider.stage_hunk(&path, hunk_index)
                }
            })
            .await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => {
                    this.load_diff_async(mode, Some(file_path), cx);
                    cx.emit(DiffViewerEvent::HunksChanged);
                }
                Err(e) => {
                    this.error_message = Some(e);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The fixed line height (whole pixels) the host should reserve per item.
    pub fn inline_line_height(&self) -> f32 {
        self.line_height()
    }

    // Horizontal-scroll geometry, exposed so the host can draw a horizontal
    // scrollbar at the bottom of the diff block.
    /// Current horizontal scroll offset (px).
    pub fn inline_scroll_x(&self) -> f32 {
        self.scroll_x
    }
    /// Maximum horizontal scroll offset (px) — 0 when the code fits.
    pub fn inline_max_scroll_x(&self) -> f32 {
        self.max_scroll_x()
    }
    /// Total code width (px).
    pub fn inline_content_width(&self) -> f32 {
        self.max_text_width()
    }
    /// Visible code width (viewport minus gutter, px).
    pub fn inline_viewport_width(&self) -> f32 {
        self.available_text_width()
    }
    /// Set the horizontal scroll offset (clamped), e.g. from a scrollbar drag.
    pub fn set_inline_scroll_x(&mut self, x: f32, cx: &mut Context<Self>) {
        let max = self.max_scroll_x();
        self.scroll_x = x.clamp(0.0, max);
        cx.notify();
    }

    /// Set the viewport width (the diff column's pixel width) so horizontal
    /// scroll math (`max_scroll_x`, scrollbar geometry) is correct. The host
    /// captures this once from the list and pushes it in.
    pub fn set_inline_viewport_width(&mut self, width: f32) {
        if width > 0.0 {
            self.diff_pane_width = width;
            // Re-clamp in case the viewport grew past the current offset.
            let max = self.max_scroll_x();
            if self.scroll_x > max {
                self.scroll_x = max;
            }
        }
    }

    /// Render a single inline diff item (a line or a context expander) by index,
    /// using the real diff rendering — full functionality intact.
    pub fn render_inline_item(
        &mut self,
        index: usize,
        t: &notmux_core::theme::ThemeColors,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Measure the monospace character width from font metrics so the
        // line-number gutter is sized correctly (mirrors `render_embedded`).
        let font = Font {
            family: "monospace".into(),
            weight: FontWeight::NORMAL,
            style: FontStyle::Normal,
            features: FontFeatures::default(),
            fallbacks: None,
        };
        let font_id = window.text_system().resolve_font(&font);
        self.measured_char_width = window
            .text_system()
            .advance(font_id, px(self.file_font_size), 'm')
            .map(|size| f32::from(size.width))
            .unwrap_or(self.file_font_size * 0.6);

        let char_width = self.char_width();
        let num_col_width = (self.line_num_width as f32) * char_width + 12.0;
        let gutter_width = 2.0 * num_col_width + 1.0;

        // Measure the real pixel width of the widest line once (per content
        // change) via the text system — char-count × char-width is wrong when
        // the font isn't truly monospace, which made the horizontal scroll
        // overshoot (text scrolled off into empty space).
        if self.measured_content_width <= 0.0 {
            let longest = self.current_file.as_ref().and_then(|f| {
                f.items
                    .iter()
                    .filter_map(|i| match i {
                        DisplayItem::Line(l) => Some(l.plain_text.clone()),
                        _ => None,
                    })
                    .max_by_key(|t| t.chars().count())
            });
            if let Some(text) = longest.filter(|t| !t.is_empty()) {
                let run = TextRun {
                    len: text.len(),
                    font: font.clone(),
                    color: gpui::black(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let layout =
                    window
                        .text_system()
                        .layout_line(&text, px(self.file_font_size), &[run], None);
                self.measured_content_width = f32::from(layout.width);
            }
        }

        let Some(file) = &self.current_file else {
            return div().into_any_element();
        };
        let line_el = match file.items.get(index) {
            Some(DisplayItem::Line(line)) => {
                self.render_line(index, line, t, gutter_width, cx).into_any_element()
            }
            // Expanders never overflow horizontally — render them plainly.
            Some(DisplayItem::Expander(expander)) => {
                return self.render_expander_row(index, expander, t, cx).into_any_element();
            }
            None => return div().into_any_element(),
        };

        // Wrap content lines so they scroll horizontally (per file) when the code
        // runs past the viewport: a scroll-wheel handler drives the shared
        // `scroll_x` (every line reads it, so they shift together, gutter fixed).
        // The viewport width for clamping is pushed in by the host via
        // `set_inline_viewport_width`.
        div()
            .w_full()
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _window, cx| {
                this.handle_scroll_x(event, cx);
            }))
            .child(line_el)
            .into_any_element()
    }

    /// Current diff view mode (for persisting on close).
    pub fn view_mode(&self) -> DiffViewMode {
        self.view_mode
    }

    /// Current ignore-whitespace setting (for persisting on close).
    pub fn ignore_whitespace(&self) -> bool {
        self.ignore_whitespace
    }

    /// Update configuration (font size, theme) from outside.
    pub fn update_config(&mut self, font_size: f32, is_dark: bool, cx: &mut Context<Self>) {
        self.file_font_size = font_size;
        let new_colors = theme(cx);
        let theme_changed = is_dark != self.is_dark
            || new_colors.bg_primary != self.theme_colors.bg_primary
            || new_colors.text_primary != self.theme_colors.text_primary;
        self.is_dark = is_dark;
        self.theme_colors = new_colors;
        if theme_changed {
            self.rehighlight_current_file();
            self.update_side_by_side_cache();
        }
    }

    fn load_diff_async(
        &mut self,
        mode: DiffMode,
        select_file: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.diff_mode = mode.clone();
        self.loading = true;
        self.error_message = None;
        self.raw_files.clear();
        self.file_stats.clear();
        self.current_file = None;
        self.current_file_old_content = None;
        self.current_file_new_content = None;
        self.file_tree = FileTreeNode::default();
        self.selected_file_index = 0;
        self.selection.clear();
        self.selection_side = None;
        self.side_by_side_lines.clear();
        self.scroll_x = 0.0;
        self.max_line_chars = 0;
        cx.notify();

        let provider = self.provider.clone();
        let ignore_whitespace = self.ignore_whitespace;
        let selected_file = select_file.clone();

        cx.spawn(async move |this, cx| {
            let mode_for_fallback = mode.clone();
            let result = smol::unblock(move || {
                if let Some(file_path) = selected_file.as_deref() {
                    provider.get_diff_for_file(mode, ignore_whitespace, file_path)
                } else {
                    provider.get_diff(mode, ignore_whitespace)
                }
            })
            .await;

            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(diff_result) => {
                        if diff_result.is_empty() {
                            // Auto-fallback: if WorkingTree is empty, try Staged
                            if mode_for_fallback == DiffMode::WorkingTree {
                                this.load_diff_async(DiffMode::Staged, select_file, cx);
                                return;
                            }
                            this.error_message = Some(format!(
                                "No {} changes",
                                mode_for_fallback.display_name().to_lowercase()
                            ));
                        } else {
                            this.store_diff_result(diff_result);
                            this.build_file_tree();

                            // Select specific file if requested
                            if let Some(ref file_path) = select_file
                                && let Some(index) =
                                    this.file_stats.iter().position(|f| f.path == *file_path)
                            {
                                this.selected_file_index = index;
                            }

                            this.process_current_file_async(cx);
                        }
                    }
                    Err(e) => {
                        this.error_message = Some(e);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Store raw diff data and extract lightweight stats (no syntax highlighting).
    fn store_diff_result(&mut self, result: DiffResult) {
        for file in result.files {
            self.file_stats.push(FileStats::from(&file));
            self.raw_files.push(file);
        }
    }

    /// Process the currently selected file with syntax highlighting (async).
    fn process_current_file_async(&mut self, cx: &mut Context<Self>) {
        let Some(raw_file) = self.raw_files.get(self.selected_file_index).cloned() else {
            self.current_file = None;
            self.current_file_old_content = None;
            self.current_file_new_content = None;
            return;
        };

        let provider = self.provider.clone();
        let file_path = raw_file.display_name().to_string();
        let diff_mode = self.diff_mode.clone();
        let syntax_set = self.syntax_set.clone();
        let theme_colors = self.theme_colors;

        cx.spawn(async move |this, cx| {
            let (old_content, new_content, display_file, max_line_num) = smol::unblock(move || {
                let (old_content, new_content) = provider.get_file_contents(&file_path, diff_mode);
                let mut max_line_num = 0usize;
                let display_file = process_file(
                    &raw_file,
                    &mut max_line_num,
                    &syntax_set,
                    old_content.clone(),
                    new_content.clone(),
                    &theme_colors,
                );
                (old_content, new_content, display_file, max_line_num)
            })
            .await;

            let _ = this.update(cx, |this, cx| {
                this.current_file_old_content = old_content;
                this.current_file_new_content = new_content;
                this.line_num_width = max_line_num.to_string().len().max(3);
                this.max_line_chars = Self::calc_max_line_chars(&display_file);
                this.current_file = Some(display_file);
                this.measured_content_width = 0.0;
                this.update_side_by_side_cache();
                cx.notify();
            });
        })
        .detach();
    }

    /// Re-highlight current file using cached content (for theme changes).
    fn rehighlight_current_file(&mut self) {
        let Some(raw_file) = self.raw_files.get(self.selected_file_index) else {
            return;
        };

        let mut max_line_num = 0usize;
        let display_file = process_file(
            raw_file,
            &mut max_line_num,
            &self.syntax_set,
            self.current_file_old_content.clone(),
            self.current_file_new_content.clone(),
            &self.theme_colors,
        );

        self.line_num_width = max_line_num.to_string().len().max(3);
        self.max_line_chars = Self::calc_max_line_chars(&display_file);
        self.measured_content_width = 0.0;
        self.current_file = Some(display_file);
    }

    fn build_file_tree(&mut self) {
        self.file_tree = build_file_tree(
            self.file_stats
                .iter()
                .enumerate()
                .map(|(i, f)| (i, &f.path)),
        );
        // Auto-expand all folders in diff view
        self.expanded_folders.clear();
        Self::collect_folder_paths(&self.file_tree, "", &mut self.expanded_folders);
    }

    fn collect_folder_paths(node: &FileTreeNode, parent: &str, out: &mut HashSet<String>) {
        for (name, child) in &node.children {
            let path = if parent.is_empty() {
                name.clone()
            } else {
                format!("{parent}/{name}")
            };
            out.insert(path.clone());
            Self::collect_folder_paths(child, &path, out);
        }
    }

    fn toggle_folder(&mut self, path: &str, cx: &mut Context<Self>) {
        if self.expanded_folders.contains(path) {
            self.expanded_folders.remove(path);
        } else {
            self.expanded_folders.insert(path.to_string());
        }
        cx.notify();
    }

    fn toggle_mode(&mut self, cx: &mut Context<Self>) {
        let new_mode = self.diff_mode.toggle();
        self.load_diff_async(new_mode, None, cx);
    }

    fn toggle_view_mode(&mut self, cx: &mut Context<Self>) {
        self.view_mode = self.view_mode.toggle();
        self.finish_view_mode_change(cx);
    }

    /// Explicit counterpart to `toggle_view_mode` (ported API); unused on this
    /// branch but kept for symmetry with the notmux original.
    #[allow(dead_code)]
    fn set_view_mode(&mut self, view_mode: DiffViewMode, cx: &mut Context<Self>) {
        if self.view_mode == view_mode {
            return;
        }
        self.view_mode = view_mode;
        self.finish_view_mode_change(cx);
    }

    fn finish_view_mode_change(&mut self, cx: &mut Context<Self>) {
        self.selection.clear();
        self.selection_side = None;
        self.update_side_by_side_cache();
        // Persist through ExtensionSettingsStore
        let mut gs = git_settings(cx);
        gs.diff_view_mode = self.view_mode;
        set_git_settings(&gs, cx);
        cx.notify();
    }

    fn toggle_ignore_whitespace(&mut self, cx: &mut Context<Self>) {
        self.ignore_whitespace = !self.ignore_whitespace;
        let mode = self.diff_mode.clone();
        self.load_diff_async(mode, None, cx);
        // Persist through ExtensionSettingsStore
        let mut gs = git_settings(cx);
        gs.diff_ignore_whitespace = self.ignore_whitespace;
        set_git_settings(&gs, cx);
    }

    fn update_side_by_side_cache(&mut self) {
        if self.view_mode == DiffViewMode::SideBySide {
            if let Some(file) = &self.current_file {
                self.side_by_side_lines = side_by_side::to_side_by_side(&file.items);
            } else {
                self.side_by_side_lines.clear();
            }
        } else {
            self.side_by_side_lines.clear();
        }
    }

    fn select_file(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.file_stats.len() {
            self.selected_file_index = index;
            self.selection.clear();
            self.selection_side = None;
            self.scroll_x = 0.0;
            self.current_file = None;
            self.side_by_side_lines.clear();

            self.process_current_file_async(cx);
            cx.notify();
        }
    }

    fn prev_file(&mut self, cx: &mut Context<Self>) {
        if self.selected_file_index > 0 {
            self.select_file(self.selected_file_index - 1, cx);
        }
    }

    fn next_file(&mut self, cx: &mut Context<Self>) {
        if self.selected_file_index + 1 < self.file_stats.len() {
            self.select_file(self.selected_file_index + 1, cx);
        }
    }

    fn close(&self, cx: &mut Context<Self>) {
        cx.emit(DiffViewerEvent::Close);
    }

    fn has_commits(&self) -> bool {
        !self.commits.is_empty()
    }

    fn can_prev_commit(&self) -> bool {
        self.has_commits() && self.commit_index > 0
    }

    fn can_next_commit(&self) -> bool {
        self.has_commits() && self.commit_index + 1 < self.commits.len()
    }

    fn prev_commit(&mut self, cx: &mut Context<Self>) {
        if !self.can_prev_commit() {
            return;
        }
        self.commit_index -= 1;
        self.navigate_to_current_commit(cx);
    }

    fn next_commit(&mut self, cx: &mut Context<Self>) {
        if !self.can_next_commit() {
            return;
        }
        self.commit_index += 1;
        self.navigate_to_current_commit(cx);
    }

    fn navigate_to_current_commit(&mut self, cx: &mut Context<Self>) {
        let commit = &self.commits[self.commit_index];
        self.commit_message = Some(commit.message.clone());
        let mode = DiffMode::Commit(commit.hash.clone());
        self.load_diff_async(mode, None, cx);
    }

    fn calc_max_line_chars(file: &DiffDisplayFile) -> usize {
        file.items
            .iter()
            .filter_map(|item| match item {
                DisplayItem::Line(l) => Some(l.plain_text.chars().count()),
                DisplayItem::Expander(_) => None,
            })
            .max()
            .unwrap_or(0)
    }

    /// Expand all hidden context lines. Finds the expander by matching old/new range.
    fn expand_context_by_range(
        &mut self,
        old_range: (usize, usize),
        new_range: (usize, usize),
        cx: &mut Context<Self>,
    ) {
        let file = match self.current_file.as_ref() {
            Some(f) => f,
            None => return,
        };
        let item_index = file.items.iter().position(|item| {
            matches!(item, DisplayItem::Expander(e) if e.old_range == old_range && e.new_range == new_range)
        });
        if let Some(idx) = item_index {
            self.expand_context(idx, cx);
        }
    }

    /// Expand all hidden context lines at the given item index.
    fn expand_context(&mut self, item_index: usize, cx: &mut Context<Self>) {
        // Read the expander's hidden ranges (immutable).
        let expander = match self
            .current_file
            .as_ref()
            .and_then(|f| f.items.get(item_index))
        {
            Some(DisplayItem::Expander(e)) => e.clone(),
            _ => return,
        };

        let (old_start, old_end) = expander.old_range;
        let (new_start, new_end) = expander.new_range;
        if new_start == 0 || new_end < new_start || old_end < old_start {
            return;
        }

        self.selection.clear();
        self.selection_side = None;

        // Syntax-highlight the just-revealed ranges on demand. `process_file`
        // only highlights the hunk ranges (to keep loading O(diff)), so these
        // previously-hidden lines have no precomputed spans — highlight exactly
        // the revealed ranges here (still proportional to what's shown).
        let path = self
            .file_stats
            .get(self.selected_file_index)
            .map(|f| f.path.clone())
            .unwrap_or_default();
        let syntax = get_syntax_for_path(std::path::Path::new(&path), &self.syntax_set);
        let theme = build_syntax_theme(&self.theme_colors);
        let default_color = default_text_color_for(&self.theme_colors);
        let new_spans = self
            .current_file_new_content
            .as_deref()
            .map(|c| {
                syntax::highlight_ranges(
                    c,
                    &[(new_start, new_end)],
                    syntax,
                    &theme,
                    &self.syntax_set,
                    default_color,
                )
            })
            .unwrap_or_default();
        let old_spans = self
            .current_file_old_content
            .as_deref()
            .map(|c| {
                syntax::highlight_ranges(
                    c,
                    &[(old_start, old_end)],
                    syntax,
                    &theme,
                    &self.syntax_set,
                    default_color,
                )
            })
            .unwrap_or_default();

        let old_lines: Vec<&str> = self
            .current_file_old_content
            .as_deref()
            .map(|c| c.lines().collect())
            .unwrap_or_default();
        let new_lines: Vec<&str> = self
            .current_file_new_content
            .as_deref()
            .map(|c| c.lines().collect())
            .unwrap_or_default();
        let (old_line_count, new_line_count) = self
            .current_file
            .as_ref()
            .map(|f| (f.old_line_count, f.new_line_count))
            .unwrap_or((0, 0));

        let count = new_end - new_start + 1;
        let mut new_items: Vec<DisplayItem> = Vec::with_capacity(count);

        for i in 0..count {
            let new_ln = new_start + i;
            let old_ln = old_start + i;

            let plain_text = new_lines
                .get(new_ln - 1)
                .or_else(|| old_lines.get(old_ln - 1))
                .unwrap_or(&"")
                .replace('\t', "    ");

            let spans = new_spans
                .get(&new_ln)
                .or_else(|| old_spans.get(&old_ln))
                .cloned()
                .unwrap_or_else(|| {
                    vec![types::HighlightedSpan {
                        color: default_color,
                        text: plain_text.clone(),
                    }]
                });

            new_items.push(DisplayItem::Line(types::DisplayLine {
                line_type: notmux_git::DiffLineType::Context,
                old_line_num: (old_ln >= 1 && old_ln <= old_line_count).then_some(old_ln),
                new_line_num: (new_ln >= 1 && new_ln <= new_line_count).then_some(new_ln),
                spans,
                plain_text,
            }));
        }

        let file = self
            .current_file
            .as_mut()
            .expect("current_file verified Some at function entry");
        file.items.splice(item_index..=item_index, new_items);

        self.max_line_chars = Self::calc_max_line_chars(file);
        self.measured_content_width = 0.0;
        self.update_side_by_side_cache();
        cx.notify();
    }

    fn get_selected_text(&self) -> Option<String> {
        if let Some(side) = self.selection_side {
            let lines = &self.side_by_side_lines;
            extract_selected_text(&self.selection, lines.len(), |i| {
                let sbs_line = &lines[i];
                if sbs_line.expander.is_some() {
                    return "";
                }
                let content = match side {
                    SideBySideSide::Left => &sbs_line.left,
                    SideBySideSide::Right => &sbs_line.right,
                };
                content
                    .as_ref()
                    .map(|c| c.plain_text.as_str())
                    .unwrap_or("")
            })
        } else {
            let file = self.current_file.as_ref()?;
            extract_selected_text(&self.selection, file.items.len(), |i| {
                match &file.items[i] {
                    DisplayItem::Line(line) => &line.plain_text,
                    DisplayItem::Expander(_) => "",
                }
            })
        }
    }

    fn copy_selection(&self, cx: &mut Context<Self>) {
        copy_to_clipboard(cx, self.get_selected_text());
    }

    fn select_all(&mut self, cx: &mut Context<Self>) {
        // Use effective view mode (new/deleted files forced to unified)
        let is_new_or_deleted = self
            .file_stats
            .get(self.selected_file_index)
            .map(|f| f.is_new || f.is_deleted)
            .unwrap_or(false);
        let effective_mode = if is_new_or_deleted {
            DiffViewMode::Unified
        } else {
            self.view_mode
        };

        match effective_mode {
            DiffViewMode::Unified => {
                if let Some(file) = &self.current_file {
                    if file.items.is_empty() {
                        return;
                    }
                    let last_line = file.items.len() - 1;
                    let last_col = match &file.items[last_line] {
                        DisplayItem::Line(l) => l.plain_text.len(),
                        DisplayItem::Expander(_) => 0,
                    };
                    self.selection.start = Some((0, 0));
                    self.selection.end = Some((last_line, last_col));
                    self.selection_side = None;
                    cx.notify();
                }
            }
            DiffViewMode::SideBySide => {
                if self.side_by_side_lines.is_empty() {
                    return;
                }
                let side = self.selection_side.unwrap_or(SideBySideSide::Right);
                let last_line = self.side_by_side_lines.len() - 1;
                let last_col = {
                    let sbs_line = &self.side_by_side_lines[last_line];
                    let content = match side {
                        SideBySideSide::Left => &sbs_line.left,
                        SideBySideSide::Right => &sbs_line.right,
                    };
                    content.as_ref().map(|c| c.plain_text.len()).unwrap_or(0)
                };
                self.selection.start = Some((0, 0));
                self.selection.end = Some((last_line, last_col));
                self.selection_side = Some(side);
                cx.notify();
            }
        }
    }
}

/// Events emitted by the diff viewer.
#[derive(Clone, Debug)]
pub enum DiffViewerEvent {
    Close,
    /// A hunk was staged or unstaged from within the viewer — the owner should
    /// refresh its working-tree status / file list.
    HunksChanged,
}

impl EventEmitter<DiffViewerEvent> for DiffViewer {}

impl notmux_ui::overlay::CloseEvent for DiffViewerEvent {
    fn is_close(&self) -> bool {
        matches!(self, Self::Close)
    }
}

impl Render for DiffViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.embedded {
            self.render_embedded(window, cx)
        } else {
            self.render_view(window, cx)
        }
    }
}

impl DiffViewer {
    /// Render the diff inline at its full natural height — every line and
    /// context expander rendered flat (NO virtualized internal scroll, NO fixed
    /// height). The host's list scroll handles scrolling, so the whole diff is
    /// shown without a bounded scrollbox.
    pub fn render_embedded(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // Measure actual monospace character width from font metrics.
        let font = Font {
            family: "monospace".into(),
            weight: FontWeight::NORMAL,
            style: FontStyle::Normal,
            features: FontFeatures::default(),
            fallbacks: None,
        };
        let text_system = window.text_system();
        let font_id = text_system.resolve_font(&font);
        self.measured_char_width = text_system
            .advance(font_id, px(self.file_font_size), 'm')
            .map(|size| f32::from(size.width))
            .unwrap_or(self.file_font_size * 0.6);

        let t = theme(cx);

        if let Some(err) = self.error_message.clone() {
            return div()
                .w_full()
                .py(px(10.0))
                .px(px(16.0))
                .bg(rgb(t.bg_secondary))
                .text_color(rgb(t.text_muted))
                .child(err)
                .into_any_element();
        }
        let is_binary = self
            .file_stats
            .get(self.selected_file_index)
            .map(|f| f.is_binary)
            .unwrap_or(false);
        if is_binary {
            return div()
                .w_full()
                .py(px(10.0))
                .px(px(16.0))
                .bg(rgb(t.bg_secondary))
                .text_color(rgb(t.text_muted))
                .child("Binary file — cannot display diff")
                .into_any_element();
        }

        let char_width = self.char_width();
        let num_col_width = (self.line_num_width as f32) * char_width + 12.0;
        let gutter_width = 2.0 * num_col_width + 1.0;

        // `current_file` is None both while loading AND in the brief gap between
        // the diff-structure load and the async syntax-highlight. Show one stable
        // "Loading…" for both so the panel never flashes an empty frame.
        let Some(file) = &self.current_file else {
            return div()
                .w_full()
                .py(px(10.0))
                .flex()
                .justify_center()
                .bg(rgb(t.bg_secondary))
                .child(div().text_color(rgb(t.text_muted)).child("Loading…"))
                .into_any_element();
        };
        let count = file.items.len();

        let mut col = div()
            .id("main-diff-pane")
            .w_full()
            .flex()
            .flex_col()
            .bg(rgb(t.bg_secondary))
            .overflow_hidden()
            .cursor(CursorStyle::IBeam);
        for i in 0..count {
            let el = match &file.items[i] {
                DisplayItem::Line(line) => {
                    self.render_line(i, line, &t, gutter_width, cx).into_any_element()
                }
                DisplayItem::Expander(expander) => {
                    self.render_expander_row(i, expander, &t, cx).into_any_element()
                }
            };
            col = col.child(el);
        }
        col.into_any_element()
    }

    fn render_view(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // Measure actual monospace character width from font metrics
        let font = Font {
            family: "monospace".into(),
            weight: FontWeight::NORMAL,
            style: FontStyle::Normal,
            features: FontFeatures::default(),
            fallbacks: None,
        };
        let text_system = window.text_system();
        let font_id = text_system.resolve_font(&font);
        self.measured_char_width = text_system
            .advance(font_id, px(self.file_font_size), 'm')
            .map(|size| f32::from(size.width))
            .unwrap_or(self.file_font_size * 0.6);

        let t = theme(cx);
        let focus_handle = self.focus_handle.clone();
        let has_error = self.error_message.is_some();
        let error_message = self.error_message.clone();
        let diff_mode = self.diff_mode.clone();
        let has_files = !self.file_stats.is_empty();
        // Gutter: two number columns + separator, matching render_line layout
        let char_width = self.char_width();
        let num_col_width = (self.line_num_width as f32) * char_width + 12.0;
        let gutter_width = 2.0 * num_col_width + 1.0;

        let current_stats = self.file_stats.get(self.selected_file_index);
        let file_path = current_stats.map(|f| f.path.clone()).unwrap_or_default();
        let is_binary = current_stats.map(|f| f.is_binary).unwrap_or(false);
        let line_count = self
            .current_file
            .as_ref()
            .map(|f| f.items.len())
            .unwrap_or(0);

        let tree_elements = if has_files {
            self.render_tree_node(&self.file_tree.clone(), 0, "", &t, cx)
        } else {
            Vec::new()
        };

        let total_added: usize = self.file_stats.iter().map(|f| f.added).sum();
        let total_removed: usize = self.file_stats.iter().map(|f| f.removed).sum();

        let theme_colors = Arc::new(t);

        if !focus_handle.is_focused(window) {
            window.focus(&focus_handle, cx);
        }

        fullscreen_overlay("diff-viewer", &t)
            // Start below the title-bar strip so the overlay header aligns
            // with the app chrome instead of covering its bottom edge.
            .when(cfg!(target_os = "macos") && !window.is_fullscreen(), |d| {
                d.top(px(notmux_ui::tokens::TITLE_BAR_STRIP_H))
            })
            .track_focus(&focus_handle)
            .key_context("DiffViewer")
            .on_action(cx.listener(|this, _: &Cancel, _window, cx| {
                if this.selection.normalized_non_empty().is_some() {
                    this.selection.clear();
                    this.selection_side = None;
                    cx.notify();
                } else {
                    this.close(cx);
                }
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                let key = event.keystroke.key.as_str();
                let modifiers = &event.keystroke.modifiers;

                match key {
                    "tab" => this.toggle_mode(cx),
                    "s" => this.toggle_view_mode(cx),
                    "w" => this.toggle_ignore_whitespace(cx),
                    "up" => this.prev_file(cx),
                    "down" => this.next_file(cx),
                    "left" => {
                        this.scroll_x = (this.scroll_x - 40.0).max(0.0);
                        cx.notify();
                    }
                    "right" => {
                        let max = this.max_scroll_x();
                        this.scroll_x = (this.scroll_x + 40.0).min(max);
                        cx.notify();
                    }
                    "[" => this.prev_commit(cx),
                    "]" => this.next_commit(cx),
                    "c" if modifiers.platform || modifiers.control => this.copy_selection(cx),
                    "a" if modifiers.platform || modifiers.control => this.select_all(cx),
                    _ => {}
                }
            }))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                if this.scrollbar_drag.is_some() {
                    let y = f32::from(event.position.y);
                    this.update_scrollbar_drag(y, cx);
                }
                if let Some(drag) = this.h_scrollbar_drag {
                    let x = f32::from(event.position.x);
                    let delta_x = x - drag.start_x;
                    let max = this.max_scroll_x();
                    let text_w = this.max_text_width();
                    let avail_w = this.available_text_width();
                    let scale = if avail_w > 0.0 { text_w / avail_w } else { 1.0 };
                    this.scroll_x = (drag.start_scroll_x + delta_x * scale).clamp(0.0, max);
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _window, cx| {
                    if this.scrollbar_drag.is_some() {
                        this.end_scrollbar_drag(cx);
                    }
                    if this.h_scrollbar_drag.is_some() {
                        this.h_scrollbar_drag = None;
                        cx.notify();
                    }
                }),
            )
            .child(self.render_header(
                &t,
                has_files,
                self.file_stats.len(),
                total_added,
                total_removed,
                &diff_mode,
                self.ignore_whitespace,
                self.commit_message.as_deref(),
                cx,
            ))
            // Commit info bar (when viewing a commit with navigation)
            .when(self.has_commits(), |d| {
                d.child(self.render_commit_info_bar(&t, cx))
            })
            .child(self.render_content(
                &t,
                self.loading,
                has_error,
                error_message,
                has_files,
                is_binary,
                file_path,
                line_count,
                gutter_width,
                tree_elements,
                theme_colors,
                cx,
            ))
            .child(self.render_footer(&t, cx))
            .into_any_element()
    }
}

impl Focusable for DiffViewer {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
