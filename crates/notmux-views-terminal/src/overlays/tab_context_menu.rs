//! Context menu for tab bar (right-click on a tab).

use crate::actions::Cancel;
use gpui::prelude::*;
use gpui::*;
use notmux_ui::menu::{context_menu_panel, menu_item, menu_item_disabled, menu_separator};
use notmux_ui::theme::theme;

/// Event emitted by TabContextMenu
pub enum TabContextMenuEvent {
    Close,
    Rename {
        project_id: String,
        terminal_id: String,
    },
    CloseTab {
        project_id: String,
        layout_path: Vec<usize>,
        tab_index: usize,
    },
    CloseOtherTabs {
        project_id: String,
        layout_path: Vec<usize>,
        tab_index: usize,
    },
    CloseTabsToRight {
        project_id: String,
        layout_path: Vec<usize>,
        tab_index: usize,
    },
    TogglePin {
        project_id: String,
        layout_path: Vec<usize>,
        tab_index: usize,
    },
}

/// Context menu for tab bar
pub struct TabContextMenu {
    tab_index: usize,
    num_tabs: usize,
    project_id: String,
    layout_path: Vec<usize>,
    position: Point<Pixels>,
    /// Some(is_pinned) when the pane can be pinned (local project), None otherwise.
    pin_state: Option<bool>,
    /// Some(terminal_id) when the tab is a terminal (renameable), None otherwise.
    rename_terminal_id: Option<String>,
    focus_handle: FocusHandle,
}

impl TabContextMenu {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tab_index: usize,
        num_tabs: usize,
        project_id: String,
        layout_path: Vec<usize>,
        position: Point<Pixels>,
        pin_state: Option<bool>,
        rename_terminal_id: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        Self {
            tab_index,
            num_tabs,
            project_id,
            layout_path,
            position,
            pin_state,
            rename_terminal_id,
            focus_handle,
        }
    }

    fn close(&self, cx: &mut Context<Self>) {
        cx.emit(TabContextMenuEvent::Close);
    }
}

impl EventEmitter<TabContextMenuEvent> for TabContextMenu {}

impl Render for TabContextMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);

        // Focus on first render
        if !self.focus_handle.is_focused(window) {
            window.focus(&self.focus_handle, cx);
        }

        let position = self.position;
        let has_other_tabs = self.num_tabs > 1;
        let has_tabs_to_right = self.tab_index < self.num_tabs.saturating_sub(1);

        div()
            .track_focus(&self.focus_handle)
            .key_context("TabContextMenu")
            .on_action(cx.listener(|this, _: &Cancel, _window, cx| {
                this.close(cx);
            }))
            .absolute()
            .inset_0()
            .id("tab-context-menu-backdrop")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _window, cx| {
                    this.close(cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, _window, cx| {
                    this.close(cx);
                }),
            )
            .child(deferred(
                anchored().position(position).snap_to_window().child(
                    context_menu_panel("tab-context-menu", &t)
                        // Rename (terminals only)
                        .children(self.rename_terminal_id.clone().map(|terminal_id| {
                            menu_item("tab-ctx-rename", "icons/edit.svg", "Rename", &t).on_click(
                                cx.listener(move |this, _, _window, cx| {
                                    cx.emit(TabContextMenuEvent::Rename {
                                        project_id: this.project_id.clone(),
                                        terminal_id: terminal_id.clone(),
                                    });
                                }),
                            )
                        }))
                        .children(self.rename_terminal_id.as_ref().map(|_| menu_separator(&t)))
                        // Pin / Unpin (local projects only)
                        .children(self.pin_state.map(|is_pinned| {
                            menu_item(
                                "tab-ctx-toggle-pin",
                                if is_pinned {
                                    "icons/unpin.svg"
                                } else {
                                    "icons/pinned.svg"
                                },
                                if is_pinned { "Unpin" } else { "Pin" },
                                &t,
                            )
                            .on_click(cx.listener(|this, _, _window, cx| {
                                cx.emit(TabContextMenuEvent::TogglePin {
                                    project_id: this.project_id.clone(),
                                    layout_path: this.layout_path.clone(),
                                    tab_index: this.tab_index,
                                });
                            }))
                        }))
                        .children(self.pin_state.map(|_| menu_separator(&t)))
                        // Close tab
                        .child(
                            menu_item("tab-ctx-close", "icons/close.svg", "Close", &t).on_click(
                                cx.listener(|this, _, _window, cx| {
                                    cx.emit(TabContextMenuEvent::CloseTab {
                                        project_id: this.project_id.clone(),
                                        layout_path: this.layout_path.clone(),
                                        tab_index: this.tab_index,
                                    });
                                }),
                            ),
                        )
                        .child(menu_separator(&t))
                        // Close Others
                        .child(if has_other_tabs {
                            menu_item(
                                "tab-ctx-close-others",
                                "icons/close.svg",
                                "Close Others",
                                &t,
                            )
                            .on_click(cx.listener(
                                |this, _, _window, cx| {
                                    cx.emit(TabContextMenuEvent::CloseOtherTabs {
                                        project_id: this.project_id.clone(),
                                        layout_path: this.layout_path.clone(),
                                        tab_index: this.tab_index,
                                    });
                                },
                            ))
                        } else {
                            menu_item_disabled(
                                "tab-ctx-close-others",
                                "icons/close.svg",
                                "Close Others",
                                &t,
                            )
                        })
                        // Close to Right
                        .child(if has_tabs_to_right {
                            menu_item(
                                "tab-ctx-close-to-right",
                                "icons/chevron-right.svg",
                                "Close to Right",
                                &t,
                            )
                            .on_click(cx.listener(
                                |this, _, _window, cx| {
                                    cx.emit(TabContextMenuEvent::CloseTabsToRight {
                                        project_id: this.project_id.clone(),
                                        layout_path: this.layout_path.clone(),
                                        tab_index: this.tab_index,
                                    });
                                },
                            ))
                        } else {
                            menu_item_disabled(
                                "tab-ctx-close-to-right",
                                "icons/chevron-right.svg",
                                "Close to Right",
                                &t,
                            )
                        }),
                ),
            ))
    }
}

impl gpui::Focusable for TabContextMenu {
    fn focus_handle(&self, _cx: &gpui::App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}
