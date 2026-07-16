//! File viewer overlay for displaying file contents with syntax highlighting.
//!
//! Provides a read-only view of files with syntax highlighting via syntect.
//! Markdown files can be viewed in rendered preview mode.

mod context_menu;
mod buffer;
mod diff;
mod editing;
mod loading;
mod render;
mod search;
mod selection;

use crate::code_view::ScrollbarDrag;
use crate::file_search::FileEntry;
use crate::file_tree::{FileTreeNode, build_file_tree};
use crate::selection::SelectionState;
use crate::syntax::{HighlightedLine, load_syntax_set};
use buffer::{Cursor, EditorBuffer};
use context_menu::{DeleteConfirmState, FileRenameState, FileTreeContextMenu, TabContextMenu};
use gpui::*;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use syntect::parsing::SyntaxSet;
use notmux_core::theme::ThemeColors;
use notmux_markdown::{MarkdownDocument, MarkdownSelection};

/// Maximum file size to load (5MB)
const MAX_FILE_SIZE: u64 = 5 * 1024 * 1024;

/// Maximum number of lines to display
const MAX_LINES: usize = 10000;

/// Maximum number of open tabs
const MAX_TABS: usize = 20;

/// Maximum navigation history stack size
const MAX_HISTORY: usize = 50;

/// Display mode for file viewer.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum DisplayMode {
    #[default]
    Source,
    Preview,
}

/// Type alias for source view selection (line, column).
type Selection = SelectionState<(usize, usize)>;

#[derive(Clone, Copy, Debug)]
pub(super) struct SelectionAutoscrollState {
    pub pointer_y_in_viewport: f32,
    pub viewport_height: f32,
    pub line_height: f32,
    pub token: u64,
}

/// Width of file tree sidebar.
const SIDEBAR_WIDTH: f32 = 240.0;

/// Per-file state for a single tab in the file viewer.
pub(super) struct FileViewerTab {
    pub file_path: PathBuf,
    pub buffer: EditorBuffer,
    pub cursor: Cursor,
    pub highlighted_lines: Vec<HighlightedLine>,
    pub line_count: usize,
    pub line_num_width: usize,
    pub error_message: Option<String>,
    pub selection: Selection,
    pub display_mode: DisplayMode,
    pub is_markdown: bool,
    pub markdown_doc: Option<MarkdownDocument>,
    pub markdown_selection: MarkdownSelection,
    pub markdown_scroll_handle: ScrollHandle,
    pub source_scroll_handle: UniformListScrollHandle,
    pub scrollbar_drag: Option<ScrollbarDrag>,
    pub selection_autoscroll: Option<SelectionAutoscrollState>,
    pub selected_file_index: Option<usize>,
    /// Last known modification time of the file (for detecting external changes).
    pub modified_at: Option<SystemTime>,
    pub save_error: Option<String>,
    /// Whether the tab content is still being loaded asynchronously.
    pub loading: bool,
    /// Baseline (file at `HEAD`) for the diff editor; `None` when untracked or
    /// not yet fetched. `diff_baseline_loaded` guards the one-time git lookup.
    pub diff_baseline: Option<String>,
    pub diff_baseline_loaded: bool,
    /// Live line-diff decorations (additions / deleted lines) vs the baseline;
    /// `None` outside diff mode or before the first computation.
    pub line_diff: Option<diff::LineDiff>,
    /// Interleaved render plan (buffer + red deleted rows), rebuilt whenever
    /// `line_diff` is recomputed.
    pub diff_rows: Vec<diff::DiffRow>,
}

impl FileViewerTab {
    /// Create a new tab for browsing (no file loaded).
    pub(super) fn new_empty() -> Self {
        Self {
            file_path: PathBuf::new(),
            buffer: EditorBuffer::empty(),
            cursor: Cursor::default(),
            highlighted_lines: Vec::new(),
            line_count: 0,
            line_num_width: 3,
            error_message: None,
            selection: Selection::default(),
            display_mode: DisplayMode::Source,
            is_markdown: false,
            markdown_doc: None,
            markdown_selection: MarkdownSelection::default(),
            markdown_scroll_handle: ScrollHandle::new(),
            source_scroll_handle: UniformListScrollHandle::new(),
            scrollbar_drag: None,
            selection_autoscroll: None,
            selected_file_index: None,
            modified_at: None,
            save_error: None,
            loading: false,
            diff_baseline: None,
            diff_baseline_loaded: false,
            line_diff: None,
            diff_rows: Vec::new(),
        }
    }

    /// Create a tab in loading state (content will be filled asynchronously).
    fn new_loading(file_path: PathBuf, file_index: Option<usize>) -> Self {
        let is_markdown = Self::is_markdown_file(&file_path);
        Self {
            file_path,
            buffer: EditorBuffer::empty(),
            cursor: Cursor::default(),
            highlighted_lines: Vec::new(),
            line_count: 0,
            line_num_width: 3,
            error_message: None,
            selection: Selection::default(),
            display_mode: if is_markdown {
                DisplayMode::Preview
            } else {
                DisplayMode::Source
            },
            is_markdown,
            markdown_doc: None,
            markdown_selection: MarkdownSelection::default(),
            markdown_scroll_handle: ScrollHandle::new(),
            source_scroll_handle: UniformListScrollHandle::new(),
            scrollbar_drag: None,
            selection_autoscroll: None,
            selected_file_index: file_index,
            modified_at: None,
            save_error: None,
            loading: true,
            diff_baseline: None,
            diff_baseline_loaded: false,
            line_diff: None,
            diff_rows: Vec::new(),
        }
    }

    /// Get the filename for display in the tab bar.
    pub fn filename(&self) -> String {
        self.file_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "Untitled".to_string())
    }

    /// Check if this tab has no file loaded.
    pub fn is_empty(&self) -> bool {
        self.file_path.as_os_str().is_empty()
    }
}

/// A single entry in the navigation history.
struct HistoryEntry {
    file_path: PathBuf,
}

/// Back/forward navigation history.
pub(super) struct NavigationHistory {
    back_stack: Vec<HistoryEntry>,
    forward_stack: Vec<HistoryEntry>,
}

impl NavigationHistory {
    fn new() -> Self {
        Self {
            back_stack: Vec::new(),
            forward_stack: Vec::new(),
        }
    }

    /// Record a navigation from `current_file` to a new file.
    fn push(&mut self, current_file: &Path) {
        if current_file.as_os_str().is_empty() {
            return;
        }
        self.back_stack.push(HistoryEntry {
            file_path: current_file.to_path_buf(),
        });
        self.forward_stack.clear();
        if self.back_stack.len() > MAX_HISTORY {
            self.back_stack.remove(0);
        }
    }

    /// Go back. Returns the file path to navigate to.
    fn go_back(&mut self, current_file: &Path) -> Option<PathBuf> {
        let entry = self.back_stack.pop()?;
        if !current_file.as_os_str().is_empty() {
            self.forward_stack.push(HistoryEntry {
                file_path: current_file.to_path_buf(),
            });
        }
        Some(entry.file_path)
    }

    /// Go forward. Returns the file path to navigate to.
    fn go_forward(&mut self, current_file: &Path) -> Option<PathBuf> {
        let entry = self.forward_stack.pop()?;
        if !current_file.as_os_str().is_empty() {
            self.back_stack.push(HistoryEntry {
                file_path: current_file.to_path_buf(),
            });
        }
        Some(entry.file_path)
    }

    #[cfg(test)]
    fn can_go_back(&self) -> bool {
        !self.back_stack.is_empty()
    }

    #[cfg(test)]
    fn can_go_forward(&self) -> bool {
        !self.forward_stack.is_empty()
    }
}

/// File viewer overlay for displaying file contents.
pub struct FileViewer {
    focus_handle: FocusHandle,
    project_fs: std::sync::Arc<dyn crate::project_fs::ProjectFs>,
    /// Syntax set for highlighting
    syntax_set: SyntaxSet,
    /// File font size from settings
    file_font_size: f32,
    theme_colors: ThemeColors,
    /// Measured monospace character width (from font metrics)
    measured_char_width: f32,
    /// Whether the current theme is dark (for syntax highlighting)
    is_dark: bool,
    /// Whether files are still loading
    loading: bool,
    /// All files in the project (from file search scan)
    files: Vec<FileEntry>,
    /// File tree for sidebar navigation
    file_tree: FileTreeNode,
    /// Which folder paths are currently expanded
    expanded_folders: HashSet<String>,
    /// Scroll handle for the file tree sidebar
    tree_scroll_handle: ScrollHandle,
    /// Whether the sidebar is visible
    sidebar_visible: bool,
    /// Whether this viewer is embedded into the main app content area.
    embedded: bool,
    /// Host callback fired when the embedded viewer is clicked (the root
    /// occludes lower hitboxes, so the host pane can't observe clicks itself).
    pub(super) on_click_embedded: Option<Box<dyn Fn(&mut Window, &mut App)>>,
    monochrome_icons: bool,
    /// Open tabs
    pub(super) tabs: Vec<FileViewerTab>,
    /// Index of the active tab
    pub(super) active_tab: usize,
    /// Navigation history
    pub(super) history: NavigationHistory,
    /// Last time we checked files for external modifications
    last_change_check: std::time::Instant,
    /// Whether to include gitignored files in the file tree
    pub(super) show_ignored: bool,
    /// Whether to include hidden (dot) files in the file tree
    pub(super) show_hidden: bool,
    /// Whether the filter popover is open
    pub(super) filter_popover_open: bool,
    /// Bounds of the filter button for popover positioning
    pub(super) filter_button_bounds: Option<Bounds<Pixels>>,
    /// Bounds of the source content area for selection autoscroll.
    pub(super) source_content_bounds: Option<Bounds<Pixels>>,
    /// Context menu state for file tree right-click
    pub(super) context_menu: Option<FileTreeContextMenu>,
    /// Context menu state for tab right-click
    pub(super) tab_context_menu: Option<TabContextMenu>,
    /// Inline rename state
    pub(super) rename_state: Option<FileRenameState>,
    /// Delete confirmation dialog state
    pub(super) delete_confirm: Option<DeleteConfirmState>,
    /// In-file search state (Ctrl+F)
    pub(super) search_state: Option<search::FileSearchState>,
    /// When true this pane is an editable diff editor: the active tab is
    /// decorated with additions (green) / deletions (red) vs `HEAD`.
    pub(super) diff_mode: bool,
}

impl FileViewer {
    /// Create a new file viewer for the given file path.
    pub fn new(
        file_path: PathBuf,
        project_fs: std::sync::Arc<dyn crate::project_fs::ProjectFs>,
        font_size: f32,
        is_dark: bool,
        theme_colors: ThemeColors,
        monochrome_icons: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();

        // Compute relative path and expanded folders from the file_path itself
        // (the file list may not be loaded yet for remote projects).
        let rel_str = file_path.to_string_lossy();
        let expanded_folders = Self::compute_expanded_for_relative(&rel_str);

        let syntax_set = load_syntax_set();

        // Create tab in loading state; content will be loaded in background.
        let relative_hint = rel_str.to_string();
        let tab = FileViewerTab::new_loading(file_path.clone(), None);

        // Load file list asynchronously to avoid blocking the UI thread
        let fs_clone = project_fs.clone();
        let file_path_clone = file_path.clone();
        cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let files = cx
                .background_executor()
                .spawn(async move { fs_clone.list_files(false, false) })
                .await;
            let _ = entity.update(cx, |this, cx| {
                let file_index = files.iter().position(|f| f.path == file_path_clone);
                if let Some(idx) = file_index
                    && let Some(tab) = this.tabs.first_mut()
                {
                    tab.selected_file_index = Some(idx);
                }
                // Recompute expanded folders using the actual relative path from the file list
                if let Some(entry) = files.iter().find(|f| f.path == file_path_clone) {
                    let expanded = Self::compute_expanded_for_relative(&entry.relative_path);
                    this.expanded_folders.extend(expanded);
                }
                this.file_tree = build_file_tree(
                    files
                        .iter()
                        .enumerate()
                        .map(|(i, f)| (i, f.relative_path.as_str())),
                );
                this.files = files;
                this.loading = false;
                cx.notify();
            });
        })
        .detach();

        // Load the initial tab content in the background
        {
            let fs = project_fs.clone();
            let rel = relative_hint;
            let target = file_path;
            cx.spawn(async move |entity: WeakEntity<Self>, cx| {
                // An empty path is an untitled scratch buffer — start empty
                // instead of failing to read a nonexistent file.
                let result: Result<String, String> = if rel.is_empty() {
                    Ok(String::new())
                } else {
                    cx.background_executor()
                        .spawn(async move {
                            let size = fs.file_size(&rel)?;
                            if size > MAX_FILE_SIZE {
                                return Err(format!(
                                    "File too large ({:.1} MB). Maximum size is 5 MB.",
                                    size as f64 / 1024.0 / 1024.0
                                ));
                            }
                            fs.read_file(&rel)
                        })
                        .await
                };
                let _ = entity.update(cx, |this, cx| {
                    if let Some(tab) = this.tabs.iter_mut().find(|t| t.file_path == target) {
                        tab.apply_loaded_content(result, &this.syntax_set, &this.theme_colors);
                        cx.notify();
                    }
                });
            })
            .detach();
        }

        Self {
            focus_handle,
            project_fs,
            syntax_set,
            file_font_size: font_size,
            theme_colors,
            measured_char_width: font_size * 0.6,
            is_dark,
            loading: true,
            files: Vec::new(),
            file_tree: FileTreeNode::default(),
            expanded_folders,
            tree_scroll_handle: ScrollHandle::new(),
            sidebar_visible: true,
            embedded: false,
            on_click_embedded: None,
            monochrome_icons,
            tabs: vec![tab],
            active_tab: 0,
            history: NavigationHistory::new(),
            last_change_check: std::time::Instant::now(),
            show_ignored: false,
            show_hidden: false,
            filter_popover_open: false,
            filter_button_bounds: None,
            source_content_bounds: None,
            context_menu: None,
            tab_context_menu: None,
            rename_state: None,
            delete_confirm: None,
            search_state: None,
            diff_mode: false,
        }
    }

    /// Create a file viewer for browsing a project without a pre-selected file.
    ///
    /// Opens the sidebar file tree with no file loaded.
    pub fn new_browse(
        project_fs: std::sync::Arc<dyn crate::project_fs::ProjectFs>,
        font_size: f32,
        is_dark: bool,
        theme_colors: ThemeColors,
        monochrome_icons: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();

        // Load file list asynchronously to avoid blocking the UI thread
        let fs_clone = project_fs.clone();
        cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let files = cx
                .background_executor()
                .spawn(async move { fs_clone.list_files(false, false) })
                .await;
            let _ = entity.update(cx, |this, cx| {
                this.file_tree = build_file_tree(
                    files
                        .iter()
                        .enumerate()
                        .map(|(i, f)| (i, f.relative_path.as_str())),
                );
                this.files = files;
                this.loading = false;
                cx.notify();
            });
        })
        .detach();

        Self {
            focus_handle,
            project_fs,
            syntax_set: load_syntax_set(),
            file_font_size: font_size,
            theme_colors,
            measured_char_width: font_size * 0.6,
            is_dark,
            loading: true,
            files: Vec::new(),
            file_tree: FileTreeNode::default(),
            expanded_folders: HashSet::new(),
            tree_scroll_handle: ScrollHandle::new(),
            sidebar_visible: true,
            embedded: false,
            on_click_embedded: None,
            monochrome_icons,
            tabs: vec![FileViewerTab::new_empty()],
            active_tab: 0,
            history: NavigationHistory::new(),
            last_change_check: std::time::Instant::now(),
            show_ignored: false,
            show_hidden: false,
            filter_popover_open: false,
            filter_button_bounds: None,
            source_content_bounds: None,
            context_menu: None,
            tab_context_menu: None,
            rename_state: None,
            delete_confirm: None,
            search_state: None,
            diff_mode: false,
        }
    }

    /// Create a file viewer for the main content area.
    ///
    /// This reuses the normal file loading, highlighting, tabs, search, and
    /// selection behavior, but omits the file-tree sidebar.
    pub fn new_embedded(
        file_path: PathBuf,
        project_fs: std::sync::Arc<dyn crate::project_fs::ProjectFs>,
        font_size: f32,
        is_dark: bool,
        theme_colors: ThemeColors,
        monochrome_icons: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut viewer = Self::new(
            file_path,
            project_fs,
            font_size,
            is_dark,
            theme_colors,
            monochrome_icons,
            cx,
        );
        viewer.sidebar_visible = false;
        viewer.embedded = true;
        viewer
    }

    /// Set the host callback fired on any click into the embedded viewer.
    /// The embedded root occludes lower hitboxes, so the host pane cannot
    /// observe clicks itself.
    pub fn set_on_click_embedded(
        &mut self,
        callback: impl Fn(&mut Window, &mut App) + 'static,
    ) {
        self.on_click_embedded = Some(Box::new(callback));
    }

    /// Update configuration (font size and dark mode) from the host app.
    /// Also refreshes the file tree and all tabs that were modified externally.
    pub fn update_config(
        &mut self,
        font_size: f32,
        is_dark: bool,
        theme_colors: ThemeColors,
        monochrome_icons: bool,
        cx: &mut Context<Self>,
    ) {
        let rehighlight = is_dark != self.is_dark || theme_colors != self.theme_colors;
        self.file_font_size = font_size;
        self.is_dark = is_dark;
        self.theme_colors = theme_colors;
        self.monochrome_icons = monochrome_icons;

        // Rescan project files so the sidebar reflects added/removed files
        self.refresh_file_tree_async(cx);

        for tab in &mut self.tabs {
            if tab.is_empty() {
                continue;
            }
            // Reload externally modified files (also re-highlights)
            if tab.reload_if_changed(&self.syntax_set, &self.theme_colors) {
                continue;
            }
            // Theme changed — re-highlight without reloading
            if rehighlight {
                tab.do_highlight_content(
                    &tab.file_path.clone(),
                    &self.syntax_set,
                    &self.theme_colors,
                );
            }
        }
    }

    /// Rescan the project directory and rebuild the file tree asynchronously.
    /// Preserves expanded folders and updates file indices on open tabs.
    fn refresh_file_tree_async(&mut self, cx: &mut Context<Self>) {
        let fs = self.project_fs.clone();
        let show_ignored = self.show_ignored;
        let show_hidden = self.show_hidden;
        cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let files = cx
                .background_executor()
                .spawn(async move { fs.list_files(show_ignored, show_hidden) })
                .await;
            let _ = entity.update(cx, |this, cx| {
                this.file_tree = build_file_tree(
                    files
                        .iter()
                        .enumerate()
                        .map(|(i, f)| (i, f.relative_path.as_str())),
                );
                for tab in &mut this.tabs {
                    if !tab.is_empty() {
                        tab.selected_file_index =
                            files.iter().position(|f| f.path == tab.file_path);
                    }
                }
                this.files = files;
                cx.notify();
            });
        })
        .detach();
    }

    /// Check if the active tab's file was modified externally and reload if so.
    /// Throttled to at most once per second.
    pub(super) fn check_active_tab_freshness(&mut self) {
        if self.last_change_check.elapsed() < std::time::Duration::from_secs(1) {
            return;
        }
        self.last_change_check = std::time::Instant::now();

        let tab = &mut self.tabs[self.active_tab];
        if !tab.is_empty() {
            tab.reload_if_changed(&self.syntax_set, &self.theme_colors);
        }
    }

    /// Get the active tab.
    pub(super) fn active_tab(&self) -> &FileViewerTab {
        &self.tabs[self.active_tab]
    }

    /// Get the active tab mutably.
    pub(super) fn active_tab_mut(&mut self) -> &mut FileViewerTab {
        &mut self.tabs[self.active_tab]
    }

    /// Open a file in a tab (VS Code style).
    /// - If already open in a tab, switches to it.
    /// - If current tab is empty, replaces it.
    /// - Otherwise creates a new tab after the active one.
    pub fn open_file_in_tab(&mut self, file_path: PathBuf, cx: &mut Context<Self>) {
        // Already open? Switch to it.
        if let Some(idx) = self.tabs.iter().position(|t| t.file_path == file_path) {
            if idx != self.active_tab {
                let current_file = self.active_tab().file_path.clone();
                self.history.push(&current_file);
                self.active_tab = idx;
            }
            // Expand ancestors so sidebar highlights this file
            let expanded = Self::compute_expanded_for_relative(
                &self
                    .relative_path_for(&file_path)
                    .unwrap_or_else(|| file_path.to_string_lossy().to_string()),
            );
            self.expanded_folders.extend(expanded);
            cx.notify();
            return;
        }

        let file_index = self.files.iter().position(|f| f.path == file_path);
        let relative = self.relative_path_for(&file_path);
        let expanded = Self::compute_expanded_for_relative(
            relative.as_deref().unwrap_or(&file_path.to_string_lossy()),
        );
        self.expanded_folders.extend(expanded);

        let new_tab = FileViewerTab::new_loading(file_path.clone(), file_index);

        // If current tab is empty (no file loaded), replace it
        if self.active_tab().is_empty() {
            self.tabs[self.active_tab] = new_tab;
            self.spawn_tab_load(file_path, relative, cx);
            cx.notify();
            return;
        }

        // Push history for the current file
        let current_file = self.active_tab().file_path.clone();
        self.history.push(&current_file);

        if self.tabs.len() >= MAX_TABS {
            // At limit: replace the active tab
            self.tabs[self.active_tab] = new_tab;
        } else {
            // Insert new tab after active
            let insert_at = self.active_tab + 1;
            self.tabs.insert(insert_at, new_tab);
            self.active_tab = insert_at;
        }

        self.spawn_tab_load(file_path, relative, cx);
        cx.notify();
    }

    /// Close a tab by index.
    pub(super) fn close_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.tabs.len() <= 1 {
            cx.emit(FileViewerEvent::Close);
            return;
        }

        self.tabs.remove(index);

        if index == self.active_tab {
            // Closed the active tab: prefer the tab to the right (same index),
            // or the last tab if we were at the end
            self.active_tab = index.min(self.tabs.len() - 1);
        } else if self.active_tab > index {
            // Closed a tab before the active one: shift index left
            self.active_tab -= 1;
        }
        // If closed tab was after active tab, active_tab stays the same

        cx.notify();
    }

    /// Close all tabs except the one at `index`.
    pub(super) fn close_other_tabs(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.tabs.len() {
            let kept = self.tabs.remove(index);
            self.tabs.clear();
            self.tabs.push(kept);
            self.active_tab = 0;
            cx.notify();
        }
    }

    /// Close all tabs, leaving an empty viewer state.
    pub(super) fn close_all_tabs(&mut self, cx: &mut Context<Self>) {
        self.tabs.clear();
        self.tabs.push(FileViewerTab::new_empty());
        self.active_tab = 0;
        cx.notify();
    }

    /// Switch to a tab by index.
    pub(super) fn set_active_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.tabs.len() && index != self.active_tab {
            let current_file = self.active_tab().file_path.clone();
            self.history.push(&current_file);
            self.active_tab = index;
            // Update expanded folders to reveal active tab's file
            let tab_path = self.tabs[self.active_tab].file_path.clone();
            let expanded = Self::compute_expanded_for_relative(
                &self
                    .relative_path_for(&tab_path)
                    .unwrap_or_else(|| tab_path.to_string_lossy().to_string()),
            );
            self.expanded_folders.extend(expanded);
            // Re-run search for the new tab's content
            if self.search_state.is_some() {
                self.perform_file_search(cx);
            }
            cx.notify();
        }
    }

    /// Navigate back in history.
    pub(super) fn go_back(&mut self, cx: &mut Context<Self>) {
        let current_file = self.active_tab().file_path.clone();
        if let Some(target) = self.history.go_back(&current_file) {
            self.navigate_to_file_no_history(target, cx);
        }
    }

    /// Navigate forward in history.
    pub(super) fn go_forward(&mut self, cx: &mut Context<Self>) {
        let current_file = self.active_tab().file_path.clone();
        if let Some(target) = self.history.go_forward(&current_file) {
            self.navigate_to_file_no_history(target, cx);
        }
    }

    /// Navigate to a file without pushing history (used by back/forward).
    fn navigate_to_file_no_history(&mut self, file_path: PathBuf, cx: &mut Context<Self>) {
        // If file is open in a tab, switch to it
        if let Some(idx) = self.tabs.iter().position(|t| t.file_path == file_path) {
            self.active_tab = idx;
            cx.notify();
            return;
        }

        // Replace the current tab with a new one for the target file
        let file_index = self.files.iter().position(|f| f.path == file_path);
        let relative = self.relative_path_for(&file_path);
        let expanded = Self::compute_expanded_for_relative(
            relative.as_deref().unwrap_or(&file_path.to_string_lossy()),
        );
        self.expanded_folders.extend(expanded);

        let new_tab = FileViewerTab::new_loading(file_path.clone(), file_index);
        self.tabs[self.active_tab] = new_tab;
        self.spawn_tab_load(file_path, relative, cx);
        cx.notify();
    }

    /// Spawn a background task to load file content for a tab.
    /// The tab is identified by `file_path` to be resilient to index changes.
    fn spawn_tab_load(
        &self,
        file_path: PathBuf,
        relative_path: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let fs = self.project_fs.clone();
        let rel = relative_path.unwrap_or_else(|| file_path.to_string_lossy().to_string());
        let target = file_path;
        cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let result: Result<String, String> = cx
                .background_executor()
                .spawn(async move {
                    let size = fs.file_size(&rel)?;
                    if size > MAX_FILE_SIZE {
                        return Err(format!(
                            "File too large ({:.1} MB). Maximum size is 5 MB.",
                            size as f64 / 1024.0 / 1024.0
                        ));
                    }
                    fs.read_file(&rel)
                })
                .await;
            let _ = entity.update(cx, |this, cx| {
                if let Some(tab) = this.tabs.iter_mut().find(|t| t.file_path == target) {
                    tab.apply_loaded_content(result, &this.syntax_set, &this.theme_colors);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Look up the relative path for a file by its absolute path.
    fn relative_path_for(&self, file_path: &Path) -> Option<String> {
        self.files
            .iter()
            .find(|f| f.path == *file_path)
            .map(|f| f.relative_path.clone())
    }

    /// Compute which folder paths should be expanded to reveal a file.
    fn compute_expanded_for_relative(relative_path: &str) -> HashSet<String> {
        let mut expanded = HashSet::new();
        let parts: Vec<&str> = relative_path.split('/').collect();
        // Expand all ancestor directories (not the file itself)
        let mut path_so_far = String::new();
        for part in &parts[..parts.len().saturating_sub(1)] {
            if !path_so_far.is_empty() {
                path_so_far.push('/');
            }
            path_so_far.push_str(part);
            expanded.insert(path_so_far.clone());
        }
        expanded
    }
}

/// Events emitted by the file viewer.
#[derive(Clone, Debug)]
pub enum FileViewerEvent {
    /// Viewer was closed.
    Close,
}

impl EventEmitter<FileViewerEvent> for FileViewer {}

impl notmux_ui::overlay::CloseEvent for FileViewerEvent {
    fn is_close(&self) -> bool {
        matches!(self, Self::Close)
    }
}

impl Focusable for FileViewer {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{FileViewer, NavigationHistory};

    #[::core::prelude::v1::test]
    fn test_compute_expanded_root_file() {
        let expanded = FileViewer::compute_expanded_for_relative("README.md");
        assert!(expanded.is_empty());
    }

    #[::core::prelude::v1::test]
    fn test_compute_expanded_nested_file() {
        let expanded = FileViewer::compute_expanded_for_relative("src/views/mod.rs");
        assert_eq!(expanded.len(), 2);
        assert!(expanded.contains("src"));
        assert!(expanded.contains("src/views"));
    }

    #[::core::prelude::v1::test]
    fn test_compute_expanded_empty_string() {
        let expanded = FileViewer::compute_expanded_for_relative("");
        assert!(expanded.is_empty());
    }

    #[::core::prelude::v1::test]
    fn test_compute_expanded_no_slash() {
        let expanded = FileViewer::compute_expanded_for_relative("Cargo.toml");
        assert!(expanded.is_empty());
    }

    #[::core::prelude::v1::test]
    fn test_history_back_forward() {
        let mut history = NavigationHistory::new();
        let a = PathBuf::from("/a.rs");
        let b = PathBuf::from("/b.rs");
        let c = PathBuf::from("/c.rs");

        // Navigate a -> b -> c
        history.push(&a);
        history.push(&b);

        assert!(history.can_go_back());
        assert!(!history.can_go_forward());

        // Go back from c
        let target = history.go_back(&c).unwrap();
        assert_eq!(target, b);
        assert!(history.can_go_forward());

        // Go back again
        let target = history.go_back(&b).unwrap();
        assert_eq!(target, a);

        // Go forward
        let target = history.go_forward(&a).unwrap();
        assert_eq!(target, b);

        let target = history.go_forward(&b).unwrap();
        assert_eq!(target, c);

        assert!(!history.can_go_forward());
    }

    #[::core::prelude::v1::test]
    fn test_history_new_navigation_clears_forward() {
        let mut history = NavigationHistory::new();
        let a = PathBuf::from("/a.rs");
        let b = PathBuf::from("/b.rs");
        let c = PathBuf::from("/c.rs");
        let d = PathBuf::from("/d.rs");

        history.push(&a);
        history.push(&b);

        // Go back from c to b
        history.go_back(&c);

        // New navigation from b
        history.push(&b);

        // Forward should be empty
        assert!(!history.can_go_forward());

        // Back should give b then a
        let target = history.go_back(&d).unwrap();
        assert_eq!(target, b);
        let target = history.go_back(&b).unwrap();
        assert_eq!(target, a);
    }

    #[::core::prelude::v1::test]
    fn test_history_limit() {
        let mut history = NavigationHistory::new();
        let current = PathBuf::from("/current.rs");

        for i in 0..60 {
            history.push(&PathBuf::from(format!("/file_{}.rs", i)));
        }

        assert_eq!(history.back_stack.len(), 50);

        // First entry should be file_10 (0-9 were trimmed)
        let mut target = history.go_back(&current).unwrap();
        assert_eq!(target, PathBuf::from("/file_59.rs"));

        // Drain remaining
        let mut count = 1;
        while let Some(t) = history.go_back(&target) {
            target = t;
            count += 1;
        }
        assert_eq!(count, 50);
    }
}
