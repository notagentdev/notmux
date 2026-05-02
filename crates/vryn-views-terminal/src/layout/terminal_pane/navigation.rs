//! Terminal pane navigation, search, and key handling.

use crate::ActionDispatch;
use crate::layout::navigation::{NavigationDirection, PaneBounds, get_pane_map};
use crate::layout::terminal_pane::actions::paste_clipboard_into_terminal;
use gpui::*;
use vryn_terminal::input::{KeyEvent, KeyModifiers, key_to_bytes_with_options};

use super::TerminalPane;

impl<D: ActionDispatch + Send + Sync> TerminalPane<D> {
    pub(super) fn handle_navigation(
        &mut self,
        direction: NavigationDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pane_map = get_pane_map();

        let source = match pane_map.find_pane(&self.project_id, &self.layout_path) {
            Some(pane) => pane.clone(),
            None => return,
        };

        if let Some(target) = pane_map.find_nearest_in_direction(&source, direction) {
            self.focus_target(target, window, cx);
        }
    }

    pub(super) fn handle_sequential_navigation(
        &mut self,
        next: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pane_map = get_pane_map();

        let source = match pane_map.find_pane(&self.project_id, &self.layout_path) {
            Some(pane) => pane.clone(),
            None => return,
        };

        let target = if next {
            pane_map.find_next_pane(&source)
        } else {
            pane_map.find_prev_pane(&source)
        };

        if let Some(ref target) = target {
            self.focus_target(target, window, cx);
        }
    }

    fn focus_target(&self, target: &PaneBounds, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ref fh) = target.focus_handle {
            window.focus(fh, cx);
        }
        self.workspace.update(cx, |ws, cx| {
            ws.set_focused_terminal(target.project_id.clone(), target.layout_path.clone(), cx);
        });
    }

    pub(super) fn start_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_bar.update(cx, |search_bar, cx| {
            search_bar.open(window, cx);
        });
        cx.notify();
    }

    pub(super) fn close_search(&mut self, cx: &mut Context<Self>) {
        self.search_bar.update(cx, |search_bar, cx| {
            search_bar.close(cx);
        });
        cx.notify();
    }

    pub(super) fn next_match(&mut self, cx: &mut Context<Self>) {
        self.search_bar.update(cx, |search_bar, cx| {
            search_bar.next_match(cx);
        });
    }

    pub(super) fn prev_match(&mut self, cx: &mut Context<Self>) {
        self.search_bar.update(cx, |search_bar, cx| {
            search_bar.prev_match(cx);
        });
    }

    pub(super) fn handle_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        if let Some(ref terminal) = self.terminal {
            if is_paste_shortcut(event) {
                terminal.claim_resize_local();
                paste_clipboard_into_terminal(terminal, cx.read_from_clipboard());
                cx.stop_propagation();
                return;
            }

            terminal.claim_resize_local();

            // Backspace with selection: delete selected text (only in plain shell)
            if event.keystroke.key == "backspace"
                && !event.keystroke.modifiers.control
                && !event.keystroke.modifiers.alt
                && !event.keystroke.modifiers.platform
                && terminal.has_selection()
                && !terminal.is_mouse_mode()
                && !terminal.is_alt_screen()
                && !terminal.has_running_child()
                && terminal.delete_selection()
            {
                return;
            }

            let app_cursor_mode = terminal.is_app_cursor_mode();
            let key_event = KeyEvent {
                key: event.keystroke.key.clone(),
                key_char: event.keystroke.key_char.clone(),
                modifiers: KeyModifiers {
                    control: event.keystroke.modifiers.control,
                    shift: event.keystroke.modifiers.shift,
                    alt: event.keystroke.modifiers.alt,
                    platform: event.keystroke.modifiers.platform,
                },
            };
            let option_as_meta =
                cfg!(target_os = "macos") && crate::terminal_view_settings(cx).option_as_meta;
            if let Some(input) =
                key_to_bytes_with_options(&key_event, app_cursor_mode, option_as_meta)
            {
                terminal.send_bytes(&input);
            }
        }
    }
}

fn is_paste_shortcut(event: &KeyDownEvent) -> bool {
    let modifiers = &event.keystroke.modifiers;
    let key = event.keystroke.key.as_str();

    key.eq_ignore_ascii_case("v")
        && !modifiers.alt
        && ((modifiers.platform && !modifiers.control) || (modifiers.control && modifiers.shift))
}
