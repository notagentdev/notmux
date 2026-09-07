//! Selection, clipboard, scrollbar, and navigation for the file viewer.

use crate::code_view::{
    apply_vertical_selection_autoscroll, start_scrollbar_drag,
    update_scrollbar_drag, vertical_selection_autoscroll_delta,
};
use crate::selection::{Selection1DExtension, Selection2DNonEmpty, copy_to_clipboard};
use gpui::*;

use super::{DisplayMode, FileViewer, FileViewerEvent};

impl FileViewer {
    /// Toggle between source and preview display modes.
    pub(super) fn toggle_display_mode(&mut self, cx: &mut Context<Self>) {
        let tab = self.active_tab_mut();
        if !tab.is_markdown {
            return;
        }
        tab.display_mode = match tab.display_mode {
            DisplayMode::Source => DisplayMode::Preview,
            DisplayMode::Preview => DisplayMode::Source,
        };
        cx.notify();
    }

    /// Close the viewer.
    pub(super) fn close(&self, cx: &mut Context<Self>) {
        cx.emit(FileViewerEvent::Close);
    }

    /// Get selected text using the shared utility.
    pub(super) fn get_selected_text(&self) -> Option<String> {
        let tab = self.active_tab();
        tab.selection.normalized_non_empty().map(|((line, column), (end_line, end_column))| tab.buffer.text_in_range(super::Cursor { line, column }, super::Cursor { line: end_line, column: end_column }).to_string())
    }

    /// Copy selected text to clipboard.
    pub(super) fn copy_selection(&self, cx: &mut Context<Self>) {
        copy_to_clipboard(cx, self.get_selected_text());
    }

    /// Select all text.
    pub(super) fn select_all(&mut self, cx: &mut Context<Self>) {
        let tab = self.active_tab_mut();
        if tab.highlighted_lines.is_empty() {
            return;
        }
        let last_line = tab.buffer.line_count() - 1;
        let last_col = tab.buffer.line_char_len(last_line);
        tab.selection.start = Some((0, 0));
        tab.selection.end = Some((last_line, last_col));
        cx.notify();
    }

    /// Get selected text from markdown preview (using character indices).
    pub(super) fn get_selected_markdown_text(&self) -> Option<String> {
        let tab = self.active_tab();
        let doc = tab.markdown_doc.as_ref()?;
        let (start, end) = tab.markdown_selection.normalized_non_empty()?;

        let chars: Vec<char> = doc.plain_text.chars().collect();
        let char_count = chars.len();
        let start = start.min(char_count);
        let end = end.min(char_count);

        Some(chars[start..end].iter().collect())
    }

    /// Copy selected markdown text to clipboard.
    pub(super) fn copy_markdown_selection(&self, cx: &mut Context<Self>) {
        copy_to_clipboard(cx, self.get_selected_markdown_text());
    }

    /// Select all markdown text (using character count).
    pub(super) fn select_all_markdown(&mut self, cx: &mut Context<Self>) {
        let tab = self.active_tab_mut();
        if let Some(doc) = &tab.markdown_doc {
            let count = doc.plain_text.chars().count();
            tab.markdown_selection.start = Some(0);
            tab.markdown_selection.end = Some(count);
            cx.notify();
        }
    }

    /// Select a file from the tree — opens in a new tab (like VS Code).
    /// If the file is already open, switches to that tab.
    /// If the current tab is empty (no file), replaces it instead of creating a new one.
    pub(super) fn select_file(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(file) = self.files.get(index) {
            let path = file.path.clone();
            self.open_file_in_tab(path, cx);
        }
    }

    /// Toggle a folder's expanded/collapsed state.
    pub(super) fn toggle_folder(&mut self, folder_path: &str, cx: &mut Context<Self>) {
        if !self.expanded_folders.remove(folder_path) {
            self.expanded_folders.insert(folder_path.to_string());
        }
        cx.notify();
    }

    /// Toggle sidebar visibility.
    pub(super) fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_visible = !self.sidebar_visible;
        cx.notify();
    }

    /// Toggle a file filter option and refresh the tree.
    pub(super) fn toggle_filter(&mut self, filter: &str, cx: &mut Context<Self>) {
        match filter {
            "ignored" => self.show_ignored = !self.show_ignored,
            "hidden" => self.show_hidden = !self.show_hidden,
            _ => {}
        }
        self.refresh_file_tree_async(cx);
        cx.notify();
    }

    /// Close the active tab.
    pub(super) fn close_active_tab(&mut self, cx: &mut Context<Self>) {
        let idx = self.active_tab;
        self.close_tab(idx, cx);
    }

    /// Switch to the next tab.
    pub(super) fn next_tab(&mut self, cx: &mut Context<Self>) {
        if self.tabs.len() > 1 {
            let next = (self.active_tab + 1) % self.tabs.len();
            self.set_active_tab(next, cx);
        }
    }

    /// Switch to the previous tab.
    pub(super) fn prev_tab(&mut self, cx: &mut Context<Self>) {
        if self.tabs.len() > 1 {
            let prev = if self.active_tab == 0 {
                self.tabs.len() - 1
            } else {
                self.active_tab - 1
            };
            self.set_active_tab(prev, cx);
        }
    }

    // Scrollbar methods using shared utilities

    pub(super) fn start_scrollbar_drag(&mut self, y: f32, cx: &mut Context<Self>) {
        let tab = self.active_tab_mut();
        let mut drag = start_scrollbar_drag(&tab.source_scroll_handle);
        drag.start_y = y;
        tab.scrollbar_drag = Some(drag);
        cx.notify();
    }

    pub(super) fn update_scrollbar_drag(&mut self, y: f32, cx: &mut Context<Self>) {
        let tab = self.active_tab_mut();
        if let Some(drag) = tab.scrollbar_drag {
            update_scrollbar_drag(&tab.source_scroll_handle, drag, y);
            cx.notify();
        }
    }

    pub(super) fn end_scrollbar_drag(&mut self, cx: &mut Context<Self>) {
        self.active_tab_mut().scrollbar_drag = None;
        cx.notify();
    }

    fn source_selection_target_col(
        &self,
        target_line: usize,
        pointer_y_in_viewport: f32,
        viewport_height: f32,
    ) -> usize {
        let tab = self.active_tab();
        let line_len = tab
            .highlighted_lines
            .get(target_line)
            .map(|line| line.plain_text.len())
            .unwrap_or_default();

        if pointer_y_in_viewport <= 0.0 {
            return 0;
        }
        if pointer_y_in_viewport >= viewport_height {
            return line_len;
        }

        if let Some((end_line, end_col)) = tab.selection.end
            && end_line == target_line
        {
            return end_col.min(line_len);
        }

        if let Some((start_line, _)) = tab.selection.start {
            if target_line < start_line {
                0
            } else {
                line_len
            }
        } else {
            line_len
        }
    }

    pub(super) fn update_source_selection_from_pointer(
        &mut self,
        pointer_y_in_viewport: f32,
        viewport_height: f32,
        line_height: f32,
        cx: &mut Context<Self>,
    ) {
        {
            let tab = self.active_tab_mut();
            if !tab.selection.is_selecting
                || tab.scrollbar_drag.is_some()
                || line_height <= 0.0
                || tab.line_count == 0
            {
                tab.selection_autoscroll = None;
                return;
            }
        }

        let tab = self.active_tab();
        let scroll_y = -f32::from(tab.source_scroll_handle.0.borrow().base_handle.offset().y);
        let target_line =
            ((scroll_y + pointer_y_in_viewport.max(0.0)) / line_height).floor() as usize;
        let target_line = target_line.min(tab.line_count.saturating_sub(1));
        let target_col =
            self.source_selection_target_col(target_line, pointer_y_in_viewport, viewport_height);

        let delta_y = vertical_selection_autoscroll_delta(
            pointer_y_in_viewport,
            viewport_height,
            line_height,
        );

        let mut token_to_schedule = None;
        {
            let tab = self.active_tab_mut();
            tab.selection.end = Some((target_line, target_col));

            if delta_y != 0.0 {
                let token = tab
                    .selection_autoscroll
                    .map(|state| state.token.wrapping_add(1))
                    .unwrap_or(1);
                tab.selection_autoscroll = Some(super::SelectionAutoscrollState {
                    pointer_y_in_viewport,
                    viewport_height,
                    line_height,
                    token,
                });
                token_to_schedule = Some(token);
            } else {
                tab.selection_autoscroll = None;
            }
        }

        if let Some(token) = token_to_schedule {
            self.schedule_source_selection_autoscroll(token, cx);
        }

        cx.notify();
    }

    fn schedule_source_selection_autoscroll(&mut self, token: u64, cx: &mut Context<Self>) {
        cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(16))
                .await;
            let _ = entity.update(cx, |this, cx| {
                this.tick_source_selection_autoscroll(token, cx);
            });
        })
        .detach();
    }

    fn tick_source_selection_autoscroll(&mut self, token: u64, cx: &mut Context<Self>) {
        let Some(state) = self.active_tab().selection_autoscroll else {
            return;
        };
        if state.token != token || !self.active_tab().selection.is_selecting {
            return;
        }

        let delta_y = vertical_selection_autoscroll_delta(
            state.pointer_y_in_viewport,
            state.viewport_height,
            state.line_height,
        );
        if delta_y == 0.0 {
            self.active_tab_mut().selection_autoscroll = None;
            cx.notify();
            return;
        }

        let did_scroll =
            apply_vertical_selection_autoscroll(&self.active_tab().source_scroll_handle, delta_y)
                .is_some();

        if did_scroll {
            let scroll_y = -f32::from(
                self.active_tab()
                    .source_scroll_handle
                    .0
                    .borrow()
                    .base_handle
                    .offset()
                    .y,
            );
            let pointer_y = state.pointer_y_in_viewport;
            let target_line =
                ((scroll_y + pointer_y.max(0.0)) / state.line_height).floor() as usize;
            let target_line = target_line.min(self.active_tab().line_count.saturating_sub(1));
            let target_col =
                self.source_selection_target_col(target_line, pointer_y, state.viewport_height);
            let tab = self.active_tab_mut();
            tab.selection.end = Some((target_line, target_col));

            let next_token = token.wrapping_add(1);
            tab.selection_autoscroll = Some(super::SelectionAutoscrollState {
                token: next_token,
                ..state
            });
            cx.notify();
            self.schedule_source_selection_autoscroll(next_token, cx);
        } else {
            self.active_tab_mut().selection_autoscroll = None;
            cx.notify();
        }
    }
}
