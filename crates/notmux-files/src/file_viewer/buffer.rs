//! Editable text buffer for the file viewer/editor.

use std::time::SystemTime;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Cursor {
    pub line: usize,
    pub column: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LineRange {
    start: usize,
    end: usize,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct EditorBuffer {
    text: String,
    lines: Vec<LineRange>,
    saved_text: String,
    modified_at: Option<SystemTime>,
}

impl EditorBuffer {
    pub fn text_in_range(&self, start: Cursor, end: Cursor) -> &str {
        let start = self.byte_offset(start);
        let end = self.byte_offset(end);
        &self.text[start.min(end)..start.max(end)]
    }
    pub fn replace_range(&mut self, start: Cursor, end: Cursor, text: &str) -> Cursor {
        let start = self.byte_offset(start);
        let end = self.byte_offset(end);
        let offset = start.min(end);
        self.text.replace_range(offset..start.max(end), text);
        self.rebuild_lines();
        self.cursor_at_byte_offset(offset + text.len())
    }

    pub fn new(text: String, modified_at: Option<SystemTime>) -> Self {
        let mut this = Self {
            saved_text: text.clone(),
            text,
            lines: Vec::new(),
            modified_at,
        };
        this.rebuild_lines();
        this
    }

    pub fn empty() -> Self {
        Self::new(String::new(), None)
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn set_text(&mut self, text: String, modified_at: Option<SystemTime>) {
        self.saved_text = text.clone();
        self.text = text;
        self.modified_at = modified_at;
        self.rebuild_lines();
    }

    pub fn mark_saved(&mut self, modified_at: Option<SystemTime>) {
        self.saved_text = self.text.clone();
        self.modified_at = modified_at;
    }

    pub fn is_dirty(&self) -> bool {
        self.text != self.saved_text
    }

    pub fn line_count(&self) -> usize {
        self.lines.len().max(1)
    }

    pub fn line_char_len(&self, line: usize) -> usize {
        self.line_str(line).chars().count()
    }

    pub fn line_str(&self, line: usize) -> &str {
        let Some(range) = self.lines.get(line) else {
            return "";
        };
        &self.text[range.start..range.end]
    }

    pub fn clamp_cursor(&self, cursor: Cursor) -> Cursor {
        let line = cursor.line.min(self.line_count().saturating_sub(1));
        let column = cursor.column.min(self.line_char_len(line));
        Cursor { line, column }
    }

    pub fn insert_text(&mut self, cursor: Cursor, text: &str) -> Cursor {
        let cursor = self.clamp_cursor(cursor);
        let offset = self.byte_offset(cursor);
        self.text.insert_str(offset, text);
        self.rebuild_lines();
        self.cursor_after_insert(cursor, text)
    }

    pub fn delete_backward(&mut self, cursor: Cursor) -> Cursor {
        let cursor = self.clamp_cursor(cursor);
        let offset = self.byte_offset(cursor);
        if offset == 0 {
            return cursor;
        }

        let prev_offset = self.prev_char_boundary(offset);
        self.text.replace_range(prev_offset..offset, "");
        self.rebuild_lines();
        self.cursor_at_byte_offset(prev_offset)
    }

    pub fn delete_forward(&mut self, cursor: Cursor) -> Cursor {
        let cursor = self.clamp_cursor(cursor);
        let offset = self.byte_offset(cursor);
        if offset >= self.text.len() {
            return cursor;
        }

        let next_offset = self.next_char_boundary(offset);
        self.text.replace_range(offset..next_offset, "");
        self.rebuild_lines();
        self.cursor_at_byte_offset(offset)
    }

    pub fn move_left(&self, cursor: Cursor) -> Cursor {
        let cursor = self.clamp_cursor(cursor);
        if cursor.column > 0 {
            Cursor {
                column: cursor.column - 1,
                ..cursor
            }
        } else if cursor.line > 0 {
            let line = cursor.line - 1;
            Cursor {
                line,
                column: self.line_char_len(line),
            }
        } else {
            cursor
        }
    }

    pub fn move_right(&self, cursor: Cursor) -> Cursor {
        let cursor = self.clamp_cursor(cursor);
        let line_len = self.line_char_len(cursor.line);
        if cursor.column < line_len {
            Cursor {
                column: cursor.column + 1,
                ..cursor
            }
        } else if cursor.line + 1 < self.line_count() {
            Cursor {
                line: cursor.line + 1,
                column: 0,
            }
        } else {
            cursor
        }
    }

    pub fn move_up(&self, cursor: Cursor) -> Cursor {
        let cursor = self.clamp_cursor(cursor);
        if cursor.line == 0 {
            return cursor;
        }
        let line = cursor.line - 1;
        Cursor {
            line,
            column: cursor.column.min(self.line_char_len(line)),
        }
    }

    pub fn move_down(&self, cursor: Cursor) -> Cursor {
        let cursor = self.clamp_cursor(cursor);
        if cursor.line + 1 >= self.line_count() {
            return cursor;
        }
        let line = cursor.line + 1;
        Cursor {
            line,
            column: cursor.column.min(self.line_char_len(line)),
        }
    }

    pub fn line_start(&self, cursor: Cursor) -> Cursor {
        Cursor {
            line: self.clamp_cursor(cursor).line,
            column: 0,
        }
    }

    pub fn line_end(&self, cursor: Cursor) -> Cursor {
        let cursor = self.clamp_cursor(cursor);
        Cursor {
            line: cursor.line,
            column: self.line_char_len(cursor.line),
        }
    }

    fn rebuild_lines(&mut self) {
        self.lines.clear();
        let mut start = 0;
        for (idx, ch) in self.text.char_indices() {
            if ch == '\n' {
                self.lines.push(LineRange { start, end: idx });
                start = idx + ch.len_utf8();
            }
        }
        self.lines.push(LineRange {
            start,
            end: self.text.len(),
        });
    }

    fn byte_offset(&self, cursor: Cursor) -> usize {
        let cursor = self.clamp_cursor(cursor);
        let Some(range) = self.lines.get(cursor.line) else {
            return self.text.len();
        };
        if cursor.column == 0 {
            return range.start;
        }
        self.text[range.start..range.end]
            .char_indices()
            .nth(cursor.column)
            .map(|(idx, _)| range.start + idx)
            .unwrap_or(range.end)
    }

    fn cursor_at_byte_offset(&self, offset: usize) -> Cursor {
        let offset = offset.min(self.text.len());
        let line = self
            .lines
            .iter()
            .position(|range| offset <= range.end)
            .unwrap_or_else(|| self.lines.len().saturating_sub(1));
        let column = self
            .lines
            .get(line)
            .map(|range| self.text[range.start..offset.min(range.end)].chars().count())
            .unwrap_or_default();
        Cursor { line, column }
    }

    fn cursor_after_insert(&self, cursor: Cursor, text: &str) -> Cursor {
        let mut line = cursor.line;
        let mut column = cursor.column;
        for ch in text.chars() {
            if ch == '\n' {
                line += 1;
                column = 0;
            } else {
                column += 1;
            }
        }
        self.clamp_cursor(Cursor { line, column })
    }

    fn prev_char_boundary(&self, offset: usize) -> usize {
        self.text[..offset]
            .char_indices()
            .last()
            .map(|(idx, _)| idx)
            .unwrap_or(0)
    }

    fn next_char_boundary(&self, offset: usize) -> usize {
        self.text[offset..]
            .char_indices()
            .nth(1)
            .map(|(idx, _)| offset + idx)
            .unwrap_or(self.text.len())
    }
}

#[cfg(test)]
mod tests {
    use super::{Cursor, EditorBuffer};

    #[test]
    fn insert_text_updates_cursor() {
        let mut buffer = EditorBuffer::new("one".to_string(), None);
        let cursor = buffer.insert_text(Cursor { line: 0, column: 3 }, "\ntwo");
        assert_eq!(buffer.text(), "one\ntwo");
        assert_eq!(cursor, Cursor { line: 1, column: 3 });
    }

    #[test]
    fn delete_backward_joins_lines() {
        let mut buffer = EditorBuffer::new("one\ntwo".to_string(), None);
        let cursor = buffer.delete_backward(Cursor { line: 1, column: 0 });
        assert_eq!(buffer.text(), "onetwo");
        assert_eq!(cursor, Cursor { line: 0, column: 3 });
    }

    #[test]
    fn delete_forward_joins_lines() {
        let mut buffer = EditorBuffer::new("one\ntwo".to_string(), None);
        let cursor = buffer.delete_forward(Cursor { line: 0, column: 3 });
        assert_eq!(buffer.text(), "onetwo");
        assert_eq!(cursor, Cursor { line: 0, column: 3 });
    }

    #[test]
    fn non_ascii_cursor_columns_are_char_based() {
        let mut buffer = EditorBuffer::new("aé🙂".to_string(), None);
        let cursor = buffer.delete_backward(Cursor { line: 0, column: 3 });
        assert_eq!(buffer.text(), "aé");
        assert_eq!(cursor, Cursor { line: 0, column: 2 });
    }

    #[test]
    fn dirty_tracks_saved_text() {
        let mut buffer = EditorBuffer::new("a".to_string(), None);
        assert!(!buffer.is_dirty());
        buffer.insert_text(Cursor { line: 0, column: 1 }, "b");
        assert!(buffer.is_dirty());
        buffer.mark_saved(None);
        assert!(!buffer.is_dirty());
    }
}
