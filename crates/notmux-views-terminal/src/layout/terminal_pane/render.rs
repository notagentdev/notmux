//! Render implementation for TerminalPane.

use crate::ActionDispatch;
use crate::actions::{
    AddTab, CloseSearch, CloseTerminal, Copy, FocusDown, FocusLeft, FocusNextTerminal,
    FocusPrevTerminal, FocusRight, FocusUp, FullscreenNextTerminal, FullscreenPrevTerminal,
    MinimizeTerminal, Paste, ResetZoom, ResumeAgentSession, ScrollDown, ScrollUp, Search,
    SearchNext, SearchPrev, SendBacktab, SendEscape, SendTab, SplitHorizontal, SplitVertical,
    ToggleFullscreen, ZoomIn, ZoomOut,
};
use crate::layout::navigation::NavigationDirection;
use crate::terminal_view_settings;
use gpui::prelude::FluentBuilder;
use gpui::*;
use notmux_core::api::ActionRequest;
use notmux_files::theme::theme;
use notmux_ui::theme::with_alpha;
use notmux_ui::tokens::ui_text_sm;
use notmux_workspace::state::SplitDirection;

use super::TerminalPane;

/// Duration of the bell double-blink ring, in seconds (two pulses, then off).
const BELL_FLASH_DURATION: f32 = 0.9;

/// Opacity of the bell ring `elapsed` seconds into the flash: keyframes
/// [0, 1, 0, 1, 0] at quarter intervals with alternating ease-out/ease-in
/// segments. 0 outside the window,
/// so the ring ends invisible and stays gone.
fn bell_flash_opacity(elapsed: f32) -> f32 {
    if !(0.0..BELL_FLASH_DURATION).contains(&elapsed) {
        return 0.0;
    }
    let phase = elapsed / BELL_FLASH_DURATION * 4.0;
    let segment = (phase as usize).min(3);
    let p = phase - segment as f32;
    match segment {
        // 0 → 1, ease-out
        0 | 2 => 1.0 - (1.0 - p) * (1.0 - p),
        // 1 → 0, ease-in
        _ => 1.0 - p * p,
    }
}

impl<D: ActionDispatch + Send + Sync> Render for TerminalPane<D> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);

        // Refresh search results if terminal content changed (scroll, new output)
        self.search_bar
            .update(cx, |bar, cx| bar.refresh_if_needed(cx));

        let focus_handle = self.focus_handle.clone();
        let id_suffix = self.id_suffix();

        let is_modal = {
            let ws = self.workspace.read(cx);
            let is_modal = ws.focus_manager.is_modal();
            let search_active = self.search_bar.read(cx).is_active();

            if !search_active && !is_modal {
                if let Some(focused) = ws.focus_manager.focused_terminal_state()
                    && focused.project_id == self.project_id
                    && focused.layout_path == self.layout_path
                    && !focus_handle.is_focused(window)
                {
                    self.pending_focus = true;
                }

                if let Some(ref tid) = self.terminal_id
                    && ws
                        .focus_manager
                        .is_terminal_fullscreened(&self.project_id, tid)
                    && !focus_handle.is_focused(window)
                {
                    self.pending_focus = true;
                }
            }
            is_modal
        };

        let search_active = self.search_bar.read(cx).is_active();
        if self.pending_focus && self.terminal.is_some() && !search_active && !is_modal {
            self.pending_focus = false;
            window.focus(&self.focus_handle, cx);
        }

        let is_focused = focus_handle.is_focused(window);

        let has_bell = self.terminal.as_ref().is_some_and(|t| t.has_bell());
        if is_focused
            && has_bell
            && let Some(ref terminal) = self.terminal
            // Only auto-clear a plain terminal bell when the pane is focused.
            // Agent completion notifications (which carry a body) must persist
            // while focused — otherwise the ring/badge is cleared in the same
            // frame it is set (the user is watching the agent finish and never
            // sees it). They clear on the agent's next prompt via the
            // UserPromptSubmit hook (`notmux clear-notification`).
            && terminal.last_notification().is_none()
        {
            terminal.clear_bell();
        }

        if is_focused
            && let Some(ref terminal) = self.terminal
            && terminal.is_waiting_for_input()
        {
            terminal.clear_waiting();
        }

        if self.was_focused
            && !is_focused
            && let Some(ref terminal) = self.terminal
        {
            terminal.mark_as_viewed();
        }
        self.was_focused = is_focused;

        // The bell ring is a transient double-blink
        // instead of a permanent border — the notification label below carries
        // the persistent state. Start the flash on the has_bell rising edge and
        // drive ~30fps repaints until it finishes.
        if has_bell && !self.had_bell {
            self.bell_flash_start = Some(std::time::Instant::now());
            cx.spawn(async move |this: WeakEntity<Self>, cx| {
                loop {
                    smol::Timer::after(std::time::Duration::from_millis(33)).await;
                    let done = this.update(cx, |pane, cx| {
                        cx.notify();
                        pane.bell_flash_start
                            .is_none_or(|s| s.elapsed().as_secs_f32() >= BELL_FLASH_DURATION)
                    });
                    match done {
                        Ok(false) => {}
                        _ => break,
                    }
                }
            })
            .detach();
        }
        if !has_bell {
            self.bell_flash_start = None;
        }
        self.had_bell = has_bell;
        let bell_flash_alpha = self
            .bell_flash_start
            .map(|s| bell_flash_opacity(s.elapsed().as_secs_f32()))
            .filter(|a| *a > 0.001);

        let view_settings = terminal_view_settings(cx);
        let show_focused_border = view_settings.show_focused_border;
        let show_notification_label = view_settings.show_notification_label;
        let is_waiting = !is_focused
            && self
                .terminal
                .as_ref()
                .is_some_and(|t| t.is_waiting_for_input());
        let show_border =
            (is_focused && show_focused_border) || bell_flash_alpha.is_some() || is_waiting;
        // The blinking bell ring wins over the focused-border color so it is
        // visible on the focused pane where the agent finished.
        let border_color = if let Some(alpha) = bell_flash_alpha {
            with_alpha(t.border_bell, alpha)
        } else if is_focused && show_focused_border {
            with_alpha(t.border_focused, 1.0)
        } else {
            with_alpha(t.border_idle, 1.0)
        };

        let is_zoomed = self.is_zoomed(cx);
        let zoom_header = if is_zoomed {
            Some(self.render_zoom_header(cx))
        } else {
            None
        };

        div()
            .id(format!("terminal-pane-main-{}", id_suffix))
            .track_focus(&focus_handle)
            // Add a fullscreen-only context so arrow keys can cycle terminals
            // while maximized without swallowing the shell's arrows otherwise.
            .key_context(if is_zoomed {
                "TerminalPane TerminalPaneFullscreen"
            } else {
                "TerminalPane"
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus_handle, cx);
                    this.workspace.update(cx, |ws, cx| {
                        ws.set_focused_terminal(
                            this.project_id.clone(),
                            this.layout_path.clone(),
                            cx,
                        );
                    });
                }),
            )
            .on_scroll_wheel(cx.listener(|this, _event: &ScrollWheelEvent, window, cx| {
                if !this.focus_handle.is_focused(window) {
                    window.focus(&this.focus_handle, cx);
                    this.workspace.update(cx, |ws, cx| {
                        ws.set_focused_terminal(
                            this.project_id.clone(),
                            this.layout_path.clone(),
                            cx,
                        );
                    });
                }
            }))
            .on_action(cx.listener(|this, _: &SplitVertical, _window, cx| {
                this.handle_split(SplitDirection::Vertical, cx);
            }))
            .on_action(cx.listener(|this, _: &SplitHorizontal, _window, cx| {
                this.handle_split(SplitDirection::Horizontal, cx);
            }))
            .on_action(cx.listener(|this, _: &AddTab, _window, cx| {
                this.handle_add_tab(cx);
            }))
            .on_action(cx.listener(|this, _: &CloseTerminal, _window, cx| {
                this.handle_close(cx);
            }))
            .on_action(cx.listener(|this, _: &MinimizeTerminal, _window, cx| {
                this.handle_minimize(cx);
            }))
            .on_action(cx.listener(|this, _: &ResumeAgentSession, _window, cx| {
                this.handle_resume_agent_session(cx);
            }))
            .on_action(cx.listener(|this, _: &Copy, _window, cx| {
                this.handle_copy(cx);
            }))
            .on_action(cx.listener(|this, _: &Paste, _window, cx| {
                this.handle_paste(cx);
            }))
            .on_action(cx.listener(|this, _: &Search, window, cx| {
                if !this.search_bar.read(cx).is_active() {
                    this.start_search(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &CloseSearch, _window, cx| {
                if this.search_bar.read(cx).is_active() {
                    this.close_search(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &SearchNext, _window, cx| {
                this.next_match(cx);
            }))
            .on_action(cx.listener(|this, _: &SearchPrev, _window, cx| {
                this.prev_match(cx);
            }))
            .on_action(cx.listener(|this, _: &FocusLeft, window, cx| {
                this.handle_navigation(NavigationDirection::Left, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusRight, window, cx| {
                this.handle_navigation(NavigationDirection::Right, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusUp, window, cx| {
                this.handle_navigation(NavigationDirection::Up, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusDown, window, cx| {
                this.handle_navigation(NavigationDirection::Down, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusNextTerminal, window, cx| {
                this.handle_sequential_navigation(true, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusPrevTerminal, window, cx| {
                this.handle_sequential_navigation(false, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ScrollUp, _window, cx| {
                if let Some(ref terminal) = this.terminal {
                    terminal.scroll_up(5);
                }
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ScrollDown, _window, cx| {
                if let Some(ref terminal) = this.terminal {
                    terminal.scroll_down(5);
                }
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &SendTab, _window, _cx| {
                if let Some(ref terminal) = this.terminal {
                    terminal.send_bytes(b"\t");
                }
            }))
            .on_action(cx.listener(|this, _: &SendBacktab, _window, _cx| {
                if let Some(ref terminal) = this.terminal {
                    terminal.send_bytes(b"\x1b[Z");
                }
            }))
            .on_action(cx.listener(|this, _: &SendEscape, _window, cx| {
                if let Some(ref terminal) = this.terminal {
                    terminal.send_bytes(b"\x1b");
                }
                // Esc never reaches `handle_key`: GPUI matches keybindings
                // before running `on_key_down` listeners, and an action stops
                // propagation in the bubble phase. So the interrupt clear for
                // a working agent has to live here, not in the key handler.
                this.clear_agent_working_on_interrupt(cx);
            }))
            .on_action(cx.listener(|this, _: &ZoomIn, _window, cx| {
                let current = this
                    .workspace
                    .read(cx)
                    .get_terminal_zoom(&this.project_id, &this.layout_path);
                let new_zoom = (current + 0.1).clamp(0.5, 3.0);
                let project_id = this.project_id.clone();
                let layout_path = this.layout_path.clone();
                this.workspace.update(cx, |ws, cx| {
                    ws.set_terminal_zoom(&project_id, &layout_path, new_zoom, cx);
                });
            }))
            .on_action(cx.listener(|this, _: &ZoomOut, _window, cx| {
                let current = this
                    .workspace
                    .read(cx)
                    .get_terminal_zoom(&this.project_id, &this.layout_path);
                let new_zoom = (current - 0.1).clamp(0.5, 3.0);
                let project_id = this.project_id.clone();
                let layout_path = this.layout_path.clone();
                this.workspace.update(cx, |ws, cx| {
                    ws.set_terminal_zoom(&project_id, &layout_path, new_zoom, cx);
                });
            }))
            .on_action(cx.listener(|this, _: &ResetZoom, _window, cx| {
                let project_id = this.project_id.clone();
                let layout_path = this.layout_path.clone();
                this.workspace.update(cx, |ws, cx| {
                    ws.set_terminal_zoom(&project_id, &layout_path, 1.0, cx);
                });
            }))
            .on_action(cx.listener(|this, _: &ToggleFullscreen, _window, cx| {
                let is_fullscreen = this.workspace.read(cx).focus_manager.has_fullscreen();
                if is_fullscreen {
                    let action = ActionRequest::SetFullscreen {
                        project_id: this.project_id.clone(),
                        terminal_id: None,
                    };
                    if let Some(ref dispatcher) = this.action_dispatcher {
                        dispatcher.dispatch(action, cx);
                    }
                } else {
                    this.handle_fullscreen(cx);
                }
            }))
            .on_action(
                cx.listener(|this, _: &FullscreenNextTerminal, _window, cx| {
                    this.handle_zoom_next_terminal(cx);
                }),
            )
            .on_action(
                cx.listener(|this, _: &FullscreenPrevTerminal, _window, cx| {
                    this.handle_zoom_prev_terminal(cx);
                }),
            )
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                this.handle_key(event, cx);
            }))
            .on_click(cx.listener(|this, _, window, cx| {
                window.focus(&this.focus_handle, cx);
                this.workspace.update(cx, |ws, cx| {
                    ws.set_focused_terminal(this.project_id.clone(), this.layout_path.clone(), cx);
                });
            }))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _window, cx| {
                this.handle_file_drop(paths, cx);
            }))
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .min_w_0()
            .bg(rgb(t.bg_primary))
            .group("terminal-pane")
            .relative()
            .children(zoom_header)
            .when(!self.minimized && !self.detached, |el| {
                el.child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .min_w_0()
                        .overflow_hidden()
                        .relative()
                        .child(
                            AnyView::from(self.content.clone())
                                .cached(StyleRefinement::default().size_full()),
                        )
                        .when(show_border, |el| {
                            el.child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .border_1()
                                    .border_color(border_color),
                            )
                        })
                        .when(has_bell && !is_focused && show_notification_label, |el| {
                            let notif_body = self
                                .terminal
                                .as_ref()
                                .and_then(|t| t.last_notification())
                                .map(|n| n.body)
                                .unwrap_or_default();
                            if notif_body.is_empty() {
                                el
                            } else {
                                el.child(
                                    div()
                                        .absolute()
                                        .top(px(6.0))
                                        .right(px(6.0))
                                        .max_w(px(280.0))
                                        .py(px(4.0))
                                        .px(px(8.0))
                                        .rounded(px(4.0))
                                        .bg(rgb(t.border_bell))
                                        .text_color(rgb(t.bg_primary))
                                        .text_size(ui_text_sm(cx))
                                        .child(SharedString::from(notif_body)),
                                )
                            }
                        }),
                )
            })
            .when(search_active, |el: Stateful<Div>| {
                el.child(self.search_bar.clone())
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{BELL_FLASH_DURATION, bell_flash_opacity};

    #[test]
    fn bell_flash_double_blinks_and_ends_dark() {
        // Starts and ends at 0 — the ring never sticks around.
        assert_eq!(bell_flash_opacity(0.0), 0.0);
        assert_eq!(bell_flash_opacity(BELL_FLASH_DURATION), 0.0);
        assert_eq!(bell_flash_opacity(10.0), 0.0);
        assert_eq!(bell_flash_opacity(-1.0), 0.0);

        // Peaks at the quarter marks (two pulses).
        let q = BELL_FLASH_DURATION / 4.0;
        assert!(bell_flash_opacity(q - 0.001) > 0.99);
        assert!(bell_flash_opacity(3.0 * q - 0.001) > 0.99);
        // Dark in the middle between the pulses.
        assert!(bell_flash_opacity(2.0 * q) < 0.01);

        // Monotone rise within the first segment.
        assert!(bell_flash_opacity(0.05) < bell_flash_opacity(0.1));
        assert!(bell_flash_opacity(0.1) < bell_flash_opacity(0.2));
    }
}
