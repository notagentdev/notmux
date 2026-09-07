//! Editing commands for the file viewer/editor.

use gpui::*;
use std::path::PathBuf;
use notmux_markdown::MarkdownDocument;

use super::{Cursor, DisplayMode, FileViewer};
use crate::selection::Selection2DNonEmpty;

impl FileViewer {
    fn replace_active_selection(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        if !self.can_edit_source() { return false; }
        let Some(((start_line, start_column), (end_line, end_column))) = self.active_tab().selection.normalized_non_empty() else { return false; };
        let tab = self.active_tab_mut();
        tab.cursor = tab.buffer.replace_range(
            Cursor { line: start_line, column: start_column },
            Cursor { line: end_line, column: end_column },
            text,
        );
        tab.selection.clear();
        self.refresh_active_tab_after_edit(cx);
        true
    }
    pub(super) fn cut_selection(&mut self, cx: &mut Context<Self>) {
        if self.can_edit_source() {
            self.copy_selection(cx);
            self.replace_active_selection("", cx);
        }
    }
    pub(super) fn paste_clipboard(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.insert_text_at_cursor(&text, cx);
        }
    }

    pub(super) fn active_relative_path(&self) -> String {
        let file_path = self.active_tab().file_path.clone();
        if let Some(relative) = self.relative_path_for(&file_path) {
            return relative;
        }
        if file_path.is_absolute() {
            let project_root = PathBuf::from(self.project_fs.project_id());
            if let Ok(relative) = file_path.strip_prefix(project_root) {
                return relative.to_string_lossy().replace('\\', "/");
            }
        }
        file_path.to_string_lossy().replace('\\', "/")
    }

    /// Enable/disable diff-editor mode for this pane. Clears decorations when
    /// disabling and recomputes them when enabling.
    pub fn set_diff_mode(&mut self, diff: bool, cx: &mut Context<Self>) {
        if self.diff_mode == diff {
            return;
        }
        self.diff_mode = diff;
        // A freshly opened diff editor jumps to its first change once the
        // diff is computed — unless an explicit goto-line was requested
        if diff && !self.had_goto_line {
            self.pending_first_diff_scroll = true;
        }
        if !diff {
            for tab in &mut self.tabs {
                tab.line_diff = None;
                tab.diff_rows = Vec::new();
                tab.diff_baseline = None;
                tab.diff_baseline_loaded = false;
            }
        }
        self.recompute_active_diff();
        cx.notify();
    }

    /// Fetch the `HEAD` baseline once (cached per tab), then recompute the
    /// active tab's line-diff decorations against the current buffer. Cheap
    /// enough to run after every edit; the git lookup runs only once per file.
    pub(super) fn recompute_active_diff(&mut self) {
        if !self.diff_mode {
            return;
        }
        let idx = self.active_tab;
        match self.tabs.get(idx) {
            Some(tab) if !tab.loading && !tab.is_empty() => {}
            _ => return,
        }
        let rel = self.active_relative_path();
        let fs = self.project_fs.clone();
        let tab = &mut self.tabs[idx];
        if !tab.diff_baseline_loaded {
            tab.diff_baseline = fs.file_at_head(&rel);
            tab.diff_baseline_loaded = true;
        }
        let baseline = tab.diff_baseline.as_deref().unwrap_or("");
        let ld = super::diff::compute_line_diff(baseline, tab.buffer.text());
        tab.diff_rows = ld.rows();
        if self.pending_first_diff_scroll {
            if let Some(row) = ld.first_change_row() {
                tab.source_scroll_handle
                    .scroll_to_item(row, ScrollStrategy::Center);
            }
            self.pending_first_diff_scroll = false;
        }
        tab.line_diff = Some(ld);
    }

    pub(super) fn insert_text_at_cursor(&mut self, text: &str, cx: &mut Context<Self>) {
        if text.is_empty() || !self.can_edit_source() || self.replace_active_selection(text, cx) {
            return;
        }
        {
            let tab = self.active_tab_mut();
            tab.cursor = tab.buffer.insert_text(tab.cursor, text);
            tab.selection.clear();
        }
        self.refresh_active_tab_after_edit(cx);
    }

    pub(super) fn delete_backward_at_cursor(&mut self, cx: &mut Context<Self>) {
        if !self.can_edit_source() || self.replace_active_selection("", cx) { return; }
        {
            let tab = self.active_tab_mut();
            tab.cursor = tab.buffer.delete_backward(tab.cursor);
            tab.selection.clear();
        }
        self.refresh_active_tab_after_edit(cx);
    }

    pub(super) fn delete_forward_at_cursor(&mut self, cx: &mut Context<Self>) {
        if !self.can_edit_source() || self.replace_active_selection("", cx) { return; }
        {
            let tab = self.active_tab_mut();
            tab.cursor = tab.buffer.delete_forward(tab.cursor);
            tab.selection.clear();
        }
        self.refresh_active_tab_after_edit(cx);
    }

    pub(super) fn move_cursor_left(&mut self, cx: &mut Context<Self>) {
        let tab = self.active_tab_mut();
        tab.cursor = tab.buffer.move_left(tab.cursor);
        tab.selection.clear();
        cx.notify();
    }

    pub(super) fn move_cursor_right(&mut self, cx: &mut Context<Self>) {
        let tab = self.active_tab_mut();
        tab.cursor = tab.buffer.move_right(tab.cursor);
        tab.selection.clear();
        cx.notify();
    }

    pub(super) fn move_cursor_up(&mut self, cx: &mut Context<Self>) {
        let tab = self.active_tab_mut();
        tab.cursor = tab.buffer.move_up(tab.cursor);
        tab.selection.clear();
        cx.notify();
    }

    pub(super) fn move_cursor_down(&mut self, cx: &mut Context<Self>) {
        let tab = self.active_tab_mut();
        tab.cursor = tab.buffer.move_down(tab.cursor);
        tab.selection.clear();
        cx.notify();
    }

    pub(super) fn move_cursor_line_start(&mut self, cx: &mut Context<Self>) {
        let tab = self.active_tab_mut();
        tab.cursor = tab.buffer.line_start(tab.cursor);
        tab.selection.clear();
        cx.notify();
    }

    pub(super) fn move_cursor_line_end(&mut self, cx: &mut Context<Self>) {
        let tab = self.active_tab_mut();
        tab.cursor = tab.buffer.line_end(tab.cursor);
        tab.selection.clear();
        cx.notify();
    }

    pub(super) fn save_active_tab(&mut self, cx: &mut Context<Self>) {
        let rel = self.active_relative_path();
        let file_path = self.active_tab().file_path.clone();
        let text = self.active_tab().buffer.text().to_string();
        let fs = self.project_fs.clone();
        let project_root = PathBuf::from(fs.project_id());
        let full_path = project_root.join(&rel);

        self.active_tab_mut().save_error = None;
        cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    fs.write_file(&rel, &text)?;
                    let modified_at = std::fs::metadata(&full_path)
                        .ok()
                        .and_then(|m| m.modified().ok());
                    Ok::<_, String>(modified_at)
                })
                .await;

            let _ = entity.update(cx, |this, cx| {
                let tab_index = this.tabs.iter().position(|tab| tab.file_path == file_path);
                let tab = if let Some(tab_index) = tab_index {
                    &mut this.tabs[tab_index]
                } else {
                    this.active_tab_mut()
                };
                match result {
                    Ok(modified_at) => {
                        tab.modified_at = modified_at;
                        tab.buffer.mark_saved(modified_at);
                        tab.save_error = None;
                    }
                    Err(err) => {
                        tab.save_error = Some(err);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn can_edit_source(&self) -> bool {
        let tab = self.active_tab();
        !tab.loading
            && tab.error_message.is_none()
            && (!tab.is_markdown || tab.display_mode == DisplayMode::Source)
    }

    fn refresh_active_tab_after_edit(&mut self, cx: &mut Context<Self>) {
        let syntax_set = self.syntax_set.clone();
        let theme_colors = self.theme_colors;
        let tab = self.active_tab_mut();
        tab.do_highlight_content(&tab.file_path.clone(), &syntax_set, &theme_colors);
        if tab.is_markdown {
            tab.markdown_doc = Some(MarkdownDocument::parse(tab.buffer.text()));
        }
        // Keep the diff decorations live as the buffer changes.
        self.recompute_active_diff();
        cx.notify();
    }
}
