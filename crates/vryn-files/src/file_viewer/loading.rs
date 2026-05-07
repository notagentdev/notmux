//! File loading and syntax highlighting for the file viewer.

use super::{FileViewerTab, MAX_FILE_SIZE, MAX_LINES};
use crate::syntax::highlight_content;
use std::path::Path;
use syntect::parsing::SyntaxSet;
use vryn_core::theme::ThemeColors;
use vryn_markdown::MarkdownDocument;

impl FileViewerTab {
    /// Check if a file is a markdown file based on extension.
    pub(super) fn is_markdown_file(path: &Path) -> bool {
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| {
                let ext_lower = ext.to_lowercase();
                ext_lower == "md" || ext_lower == "markdown"
            })
            .unwrap_or(false)
    }

    /// Load file content and apply syntax highlighting.
    pub(super) fn load_file(&mut self, path: &Path, syntax_set: &SyntaxSet, colors: &ThemeColors) {
        // Check file size first
        match std::fs::metadata(path) {
            Ok(metadata) => {
                if metadata.len() > MAX_FILE_SIZE {
                    self.error_message = Some(format!(
                        "File too large ({:.1} MB). Maximum size is 5 MB.",
                        metadata.len() as f64 / 1024.0 / 1024.0
                    ));
                    return;
                }
                self.modified_at = metadata.modified().ok();
            }
            Err(e) => {
                self.error_message = Some(format!("Cannot read file: {}", e));
                return;
            }
        }

        // Read file content
        match std::fs::read_to_string(path) {
            Ok(content) => {
                self.buffer.set_text(content, self.modified_at);
                self.cursor = self.buffer.clamp_cursor(self.cursor);
                self.do_highlight_content(path, syntax_set, colors);
                // Parse markdown if this is a markdown file
                if self.is_markdown {
                    self.markdown_doc = Some(MarkdownDocument::parse(self.buffer.text()));
                }
            }
            Err(e) => {
                // Try reading as binary and check if it's a binary file
                match std::fs::read(path) {
                    Ok(bytes) => {
                        if bytes.iter().take(1024).any(|&b| b == 0) {
                            self.error_message = Some("Cannot display binary file".to_string());
                        } else {
                            self.error_message = Some(format!("Cannot read file: {}", e));
                        }
                    }
                    Err(_) => {
                        self.error_message = Some(format!("Cannot read file: {}", e));
                    }
                }
            }
        }
    }

    /// Apply content that was loaded asynchronously in the background.
    pub(super) fn apply_loaded_content(
        &mut self,
        result: Result<String, String>,
        syntax_set: &SyntaxSet,
        colors: &ThemeColors,
    ) {
        self.loading = false;
        match result {
            Ok(content) => {
                self.buffer.set_text(content, None);
                self.cursor = self.buffer.clamp_cursor(self.cursor);
                self.do_highlight_content(&self.file_path.clone(), syntax_set, colors);
                if self.is_markdown {
                    self.markdown_doc = Some(MarkdownDocument::parse(self.buffer.text()));
                }
                // Try to get mtime for local files; harmlessly fails for remote files.
                self.modified_at = std::fs::metadata(&self.file_path)
                    .ok()
                    .and_then(|m| m.modified().ok());
                self.buffer.mark_saved(self.modified_at);
            }
            Err(e) => {
                self.error_message = Some(e);
            }
        }
    }

    /// Check if the file was modified externally and reload if so.
    /// Returns true if the file was reloaded.
    pub(super) fn reload_if_changed(
        &mut self,
        syntax_set: &SyntaxSet,
        colors: &ThemeColors,
    ) -> bool {
        if self.buffer.is_dirty() {
            return false;
        }
        let Some(old_mtime) = self.modified_at else {
            return false;
        };
        let Ok(metadata) = std::fs::metadata(&self.file_path) else {
            return false;
        };
        let Ok(new_mtime) = metadata.modified() else {
            return false;
        };
        if new_mtime == old_mtime {
            return false;
        }
        let path = self.file_path.clone();
        self.error_message = None;
        self.load_file(&path, syntax_set, colors);
        true
    }

    /// Apply syntax highlighting to the content using shared utilities.
    pub(super) fn do_highlight_content(
        &mut self,
        path: &Path,
        syntax_set: &SyntaxSet,
        colors: &ThemeColors,
    ) {
        self.highlighted_lines =
            highlight_content(self.buffer.text(), path, syntax_set, MAX_LINES, colors);
        self.line_count = self.highlighted_lines.len();
        self.line_num_width = self.line_count.to_string().len().max(3);
    }
}
