//! Terminal pane action handlers.

use crate::ActionDispatch;
use gpui::*;
use notmux_core::api::ActionRequest;
use notmux_workspace::state::SplitDirection;

use super::TerminalPane;

impl<D: ActionDispatch + Send + Sync> TerminalPane<D> {
    pub(super) fn handle_split(&mut self, direction: SplitDirection, cx: &mut Context<Self>) {
        if let Some(ref dispatcher) = self.action_dispatcher {
            dispatcher.split_terminal(&self.project_id, &self.layout_path, direction, cx);
        }
    }

    pub(super) fn handle_add_tab(&mut self, cx: &mut Context<Self>) {
        if let Some(ref dispatcher) = self.action_dispatcher {
            dispatcher.add_tab(&self.project_id, &self.layout_path, false, cx);
        }
    }

    pub(super) fn handle_close(&mut self, cx: &mut Context<Self>) {
        if let Some(terminal_id) = self.terminal_id.clone() {
            let action = ActionRequest::CloseTerminal {
                project_id: self.project_id.clone(),
                terminal_id,
            };
            if let Some(ref dispatcher) = self.action_dispatcher {
                dispatcher.dispatch(action, cx);
            }
        }
    }

    pub(super) fn handle_minimize(&mut self, cx: &mut Context<Self>) {
        if let Some(ref terminal_id) = self.terminal_id {
            let action = ActionRequest::ToggleMinimized {
                project_id: self.project_id.clone(),
                terminal_id: terminal_id.clone(),
            };
            if let Some(ref dispatcher) = self.action_dispatcher {
                dispatcher.dispatch(action, cx);
            }
        }
    }

    pub(super) fn handle_fullscreen(&mut self, cx: &mut Context<Self>) {
        if let Some(ref id) = self.terminal_id {
            let action = ActionRequest::SetFullscreen {
                project_id: self.project_id.clone(),
                terminal_id: Some(id.clone()),
            };
            if let Some(ref dispatcher) = self.action_dispatcher {
                dispatcher.dispatch(action, cx);
            }
        }
    }

    /// Manual agent resume (command palette / context menu): types the
    /// recorded resume command for this pane's slot, if a restorable session
    /// exists and its agent process is not currently running.
    pub(super) fn handle_resume_agent_session(&mut self, cx: &mut Context<Self>) {
        if self.backend.is_remote() {
            return;
        }
        let Some(terminal_id) = self.terminal_id.clone() else {
            return;
        };
        match notmux_terminal::agent_sessions::manual_resume_input(&self.slot_id) {
            Some(input) => {
                self.backend
                    .transport()
                    .send_input(&terminal_id, input.as_bytes());
            }
            None => crate::toast_error(
                "No resumable agent session in this terminal".to_string(),
                cx,
            ),
        }
    }

    pub(super) fn handle_copy(&mut self, cx: &mut Context<Self>) {
        if let Some(ref terminal) = self.terminal
            && let Some(text) = terminal.get_selected_text()
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    pub(super) fn handle_paste(&mut self, cx: &mut Context<Self>) {
        let Some(ref terminal) = self.terminal else {
            return;
        };
        paste_clipboard_into_terminal(terminal, cx.read_from_clipboard());
    }

    pub(super) fn handle_file_drop(&mut self, paths: &ExternalPaths, _cx: &mut Context<Self>) {
        let Some(ref terminal) = self.terminal else {
            return;
        };

        for path in paths.paths() {
            let escaped_path = Self::shell_escape_path(path);
            terminal.send_input(&format!("{} ", escaped_path));
        }
    }

    pub(super) fn shell_escape_path(path: &std::path::Path) -> String {
        shell_escape_path(path)
    }
}

pub(crate) fn paste_clipboard_into_terminal(
    terminal: &notmux_terminal::terminal::Terminal,
    clipboard_item: Option<ClipboardItem>,
) -> bool {
    let Some(clipboard_item) = clipboard_item else {
        return false;
    };

    match clipboard_item.entries().first() {
        Some(ClipboardEntry::Image(image)) if !image.bytes.is_empty() => {
            terminal.send_bytes(b"\x16");
            true
        }
        _ => {
            let text = terminal_clipboard_text(&clipboard_item);
            if text.is_empty() {
                false
            } else {
                terminal.send_paste(&text);
                true
            }
        }
    }
}

fn terminal_clipboard_text(clipboard_item: &ClipboardItem) -> String {
    let mut text = String::new();

    for entry in clipboard_item.entries() {
        match entry {
            ClipboardEntry::String(clipboard_string) => text.push_str(&clipboard_string.text),
            ClipboardEntry::ExternalPaths(paths) => {
                for path in paths.paths() {
                    if !text.is_empty() && !text.ends_with(' ') {
                        text.push(' ');
                    }
                    text.push_str(&shell_escape_path(path));
                    text.push(' ');
                }
            }
            ClipboardEntry::Image(_) => {}
        }
    }

    text
}

fn shell_escape_path(path: &std::path::Path) -> String {
    let path_str = path.to_string_lossy();
    let mut escaped = String::with_capacity(path_str.len() * 2);

    for c in path_str.chars() {
        match c {
            ' ' | '(' | ')' | '[' | ']' | '{' | '}' | '\'' | '"' | '`' | '$' | '&' | '|' | ';'
            | '<' | '>' | '*' | '?' | '!' | '#' | '~' | '\\' => {
                escaped.push('\\');
                escaped.push(c);
            }
            _ => escaped.push(c),
        }
    }

    escaped
}
