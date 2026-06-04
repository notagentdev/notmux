//! Editing commands for the file viewer/editor.

use gpui::*;
use std::path::PathBuf;
use notmux_markdown::MarkdownDocument;

use super::{DisplayMode, FileViewer};

impl FileViewer {
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

    pub(super) fn insert_text_at_cursor(&mut self, text: &str, cx: &mut Context<Self>) {
        if text.is_empty() || !self.can_edit_source() {
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
        if !self.can_edit_source() {
            return;
        }
        {
            let tab = self.active_tab_mut();
            tab.cursor = tab.buffer.delete_backward(tab.cursor);
            tab.selection.clear();
        }
        self.refresh_active_tab_after_edit(cx);
    }

    pub(super) fn delete_forward_at_cursor(&mut self, cx: &mut Context<Self>) {
        if !self.can_edit_source() {
            return;
        }
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

    fn can_edit_source(&self) -> bool {
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
        cx.notify();
    }
}
