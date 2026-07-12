use crate::keybindings::{
    Quit, ShowCommandPalette, ShowKeybindings, ShowSettings, ShowThemeSelector, ToggleGitPanel,
    ToggleSidebar,
};
use crate::theme::theme;
use crate::ui::tokens::{ui_text, ui_text_sm};
use crate::views::components::menu_item;
use crate::workspace::state::Workspace;
use gpui::prelude::*;
use gpui::*;
use gpui_component::h_flex;
use notmux_terminal::TerminalsRegistry;
use std::collections::HashMap;
use std::time::Duration;

/// Maintain the sticky waiting-for-input entries from a poll snapshot of
/// `(terminal_id, is_waiting, has_bell, input_generation)` tuples.
///
/// A terminal is added when it is seen waiting and only removed when it
/// actually receives input (its generation changes) or it disappears from the
/// snapshot — the live waiting flag dropping (e.g. because the pane was merely
/// focused) does NOT remove it.
fn update_waiting_entries(
    snapshot: &[(String, bool, bool, u64)],
    entries: &mut HashMap<String, u64>,
) {
    for (tid, waiting, _, generation) in snapshot {
        if *waiting {
            entries.entry(tid.clone()).or_insert(*generation);
        }
    }
    let live: HashMap<&str, u64> = snapshot
        .iter()
        .map(|(tid, _, _, generation)| (tid.as_str(), *generation))
        .collect();
    entries.retain(|tid, generation| live.get(tid.as_str()) == Some(generation));
}

/// Whether the Pair button is shown in the left cluster (remote server
/// running). Also drives the top-left tab reserve in RootView.
pub fn pair_button_active(cx: &App) -> bool {
    cx.try_global::<crate::remote::GlobalRemoteInfo>()
        .is_some_and(|ri| ri.0.port().is_some())
}

/// Char-safe truncation with an ellipsis for dropdown detail lines.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", cut)
    }
}

/// Window control button types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowControlType {
    Minimize,
    Maximize,
    Restore,
    Close,
}

/// Whether the app must draw its own window controls: frameless Windows
/// always; Linux only with client-side decorations. macOS traffic lights are
/// native and float above the content on their own.
pub fn needs_client_window_controls(window: &Window) -> bool {
    if cfg!(target_os = "windows") {
        true
    } else if cfg!(target_os = "macos") {
        false
    } else {
        matches!(window.window_decorations(), Decorations::Client { .. })
    }
}

/// The minimize / maximize-or-restore / close cluster. Shared by the title
/// bar and by full-window overlays (settings) that visually replace it —
/// anything covering the title bar must re-render these or the window
/// buttons become unreachable on Windows/Linux.
pub fn window_controls_cluster(window: &Window, cx: &App) -> impl IntoElement {
    let is_maximized = window.is_maximized();
    h_flex()
        .gap(px(2.0))
        .child(window_control_button(WindowControlType::Minimize, cx))
        .child(window_control_button(
            if is_maximized {
                WindowControlType::Restore
            } else {
                WindowControlType::Maximize
            },
            cx,
        ))
        .child(window_control_button(WindowControlType::Close, cx))
}

/// One window-control caption button.
fn window_control_button(control_type: WindowControlType, cx: &App) -> impl IntoElement {
    let t = theme(cx);
    let icon = match control_type {
        WindowControlType::Minimize => "─",
        WindowControlType::Maximize => "□",
        WindowControlType::Restore => "❐",
        WindowControlType::Close => "✕",
    };

    let is_close = control_type == WindowControlType::Close;

    // On Windows, use WindowControlArea to let the OS handle button clicks natively.
    // This ensures proper maximize/restore toggle via WM_NCHITTEST.
    // On other platforms, use on_click handlers.
    let control_area = if cfg!(target_os = "windows") {
        Some(match control_type {
            WindowControlType::Minimize => WindowControlArea::Min,
            WindowControlType::Maximize | WindowControlType::Restore => WindowControlArea::Max,
            WindowControlType::Close => WindowControlArea::Close,
        })
    } else {
        None
    };

    div()
        .id(ElementId::Name(
            format!("window-control-{:?}", control_type).into(),
        ))
        .cursor_pointer()
        .w(px(46.0)) // Windows standard caption button width
        .h(px(notmux_ui::tokens::TITLE_BAR_STRIP_H)) // Match titlebar height
        .flex()
        .items_center()
        .justify_center()
        .text_size(ui_text_sm(cx))
        .text_color(rgb(t.text_secondary))
        .when(is_close, |d| {
            d.hover(|s| s.bg(rgb(0xE81123)).text_color(rgb(0xffffff)))
        })
        .when(!is_close, |d| d.hover(|s| s.opacity(0.85)))
        .child(icon)
        .when_some(control_area, |d, area| {
            // occlude() prevents parent Drag hitbox from shadowing button hit tests
            d.occlude().window_control_area(area)
        })
        .when(control_area.is_none(), |d| {
            d.on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_click({
                move |_, window, cx| {
                    cx.stop_propagation();
                    match control_type {
                        WindowControlType::Minimize => window.minimize_window(),
                        WindowControlType::Maximize | WindowControlType::Restore => {
                            window.zoom_window();
                        }
                        WindowControlType::Close => {
                            cx.quit();
                        }
                    }
                }
            })
        })
}

/// One entry in the attention (bell) dropdown: a terminal that rang a bell
/// or is waiting for user input.
struct AttentionItem {
    project_id: String,
    terminal_id: String,
    name: String,
    project_name: String,
    detail: String,
    has_bell: bool,
}

/// Title bar with window controls and sidebar toggle
pub struct TitleBar {
    title: SharedString,
    menu_open: bool,
    sidebar_open: bool,
    git_panel_open: bool,
    action_focus_handle: Option<FocusHandle>,
    workspace: Entity<Workspace>,
    terminals: TerminalsRegistry,
    /// Whether the bell dropdown is currently shown (hover-driven).
    bell_menu_open: bool,
    bell_button_hovered: bool,
    bell_menu_hovered: bool,
    /// Terminals that were seen waiting for input, keyed by terminal id, with
    /// the input-generation snapshot taken when first seen. An entry stays in
    /// the attention list until the terminal actually receives input (the
    /// generation changes) or the terminal disappears — merely focusing it is
    /// not enough.
    waiting_entries: HashMap<String, u64>,
    /// Change-detection key of the attention list, refreshed by the poll loop.
    attention_key: Vec<(String, bool)>,
    _workspace_subscription: Subscription,
    /// Flag for Linux compositor-driven window move (set on mouse-down, consumed on mouse-move)
    #[cfg(target_os = "linux")]
    should_move: bool,
}

impl TitleBar {
    pub fn new(
        title: impl Into<SharedString>,
        workspace: Entity<Workspace>,
        terminals: TerminalsRegistry,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.observe(&workspace, |_, _, cx| cx.notify());
        // Bell state changes on PTY threads without notifying this entity —
        // poll once a second and re-render only when the list changes.
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            loop {
                smol::Timer::after(Duration::from_secs(1)).await;
                if this
                    .update(cx, |tb, cx| tb.refresh_attention(cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        Self {
            title: title.into(),
            menu_open: false,
            sidebar_open: true,
            git_panel_open: false,
            action_focus_handle: None,
            workspace,
            terminals,
            bell_menu_open: false,
            bell_button_hovered: false,
            bell_menu_hovered: false,
            waiting_entries: HashMap::new(),
            attention_key: Vec::new(),
            _workspace_subscription: subscription,
            #[cfg(target_os = "linux")]
            should_move: false,
        }
    }

    pub fn set_action_focus_handle(&mut self, focus_handle: FocusHandle) {
        self.action_focus_handle = Some(focus_handle);
    }

    pub fn set_sidebar_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.sidebar_open != open {
            self.sidebar_open = open;
            cx.notify();
        }
    }

    pub fn set_git_panel_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.git_panel_open != open {
            self.git_panel_open = open;
            cx.notify();
        }
    }

    pub fn is_menu_open(&self) -> bool {
        self.menu_open
    }

    /// Poll tick: maintain the sticky waiting entries and re-render when the
    /// attention list changed (or while the dropdown is open, so idle
    /// durations stay fresh).
    fn refresh_attention(&mut self, cx: &mut Context<Self>) {
        // (terminal_id, waiting, bell, input_generation) for every terminal
        // currently present in any project layout.
        let snapshot: Vec<(String, bool, bool, u64)> = {
            let ws = self.workspace.read(cx);
            let terminals = self.terminals.lock();
            ws.data
                .projects
                .iter()
                .filter_map(|p| p.layout.as_ref())
                .flat_map(|l| l.collect_terminal_ids())
                .filter_map(|tid| {
                    terminals.get(&tid).map(|t| {
                        (
                            tid.clone(),
                            t.is_waiting_for_input(),
                            t.has_bell(),
                            t.input_generation(),
                        )
                    })
                })
                .collect()
        };

        update_waiting_entries(&snapshot, &mut self.waiting_entries);

        let key: Vec<(String, bool)> = snapshot
            .iter()
            .filter(|(tid, _, bell, _)| *bell || self.waiting_entries.contains_key(tid))
            .map(|(tid, _, bell, _)| (tid.clone(), *bell))
            .collect();
        if key != self.attention_key {
            self.attention_key = key;
            cx.notify();
        } else if self.bell_menu_open {
            cx.notify();
        }
    }

    /// Build the current attention list in project order.
    fn attention_items(&self, cx: &App) -> Vec<AttentionItem> {
        let ws = self.workspace.read(cx);
        let terminals = self.terminals.lock();
        let mut items = Vec::new();
        for p in &ws.data.projects {
            let Some(ref layout) = p.layout else { continue };
            for tid in layout.collect_terminal_ids() {
                let Some(t) = terminals.get(&tid) else { continue };
                let has_bell = t.has_bell();
                if !has_bell && !self.waiting_entries.contains_key(&tid) {
                    continue;
                }
                let name = if let Some(custom) = p.terminal_names.get(&tid) {
                    custom.clone()
                } else {
                    p.terminal_display_name(&tid, t.title())
                };
                let detail = if has_bell {
                    let body = t
                        .last_notification()
                        .map(|n| n.body.trim().to_string())
                        .unwrap_or_default();
                    if body.is_empty() {
                        "bell".to_string()
                    } else {
                        truncate_chars(&body, 48)
                    }
                } else {
                    format!("needs input · {}", t.idle_duration_display())
                };
                items.push(AttentionItem {
                    project_id: p.id.clone(),
                    terminal_id: tid,
                    name,
                    project_name: p.name.clone(),
                    detail,
                    has_bell,
                });
            }
        }
        items
    }

    fn schedule_bell_menu_close(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            smol::Timer::after(Duration::from_millis(250)).await;
            let _ = this.update(cx, |tb, cx| {
                if tb.bell_menu_open && !tb.bell_button_hovered && !tb.bell_menu_hovered {
                    tb.bell_menu_open = false;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn toggle_menu(&mut self, cx: &mut Context<Self>) {
        self.menu_open = !self.menu_open;
        cx.notify();
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        self.menu_open = false;
        cx.notify();
    }

    /// Render the app dropdown menu overlay (must be called from a parent with full window coverage).
    pub fn render_menu(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = theme(cx);

        let traffic_light_padding = if cfg!(target_os = "macos") {
            px(80.0)
        } else {
            px(8.0)
        };

        div()
            .id("app-menu-backdrop")
            .absolute()
            .inset_0()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _window, cx| {
                    cx.stop_propagation();
                    this.close_menu(cx);
                }),
            )
            .on_mouse_move(|_, _, cx| {
                cx.stop_propagation();
            })
            .child(
                // Menu panel
                div()
                    .absolute()
                    .top(px(42.0))
                    .left(traffic_light_padding + px(40.0))
                    .bg(rgb(t.bg_primary))
                    .border_1()
                    .border_color(rgb(t.border))
                    .rounded(px(4.0))
                    .shadow_xl()
                    .min_w(px(200.0))
                    .py(px(4.0))
                    .id("app-menu-panel")
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    // Settings
                    .child(
                        menu_item("app-menu-settings", "icons/edit.svg", "Open Settings", &t)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.close_menu(cx);
                                window.dispatch_action(Box::new(ShowSettings), cx);
                            })),
                    )
                    // Theme
                    .child(
                        menu_item("app-menu-theme", "icons/eye.svg", "Select Theme", &t).on_click(
                            cx.listener(|this, _, window, cx| {
                                this.close_menu(cx);
                                window.dispatch_action(Box::new(ShowThemeSelector), cx);
                            }),
                        ),
                    )
                    // Command Palette
                    .child(
                        menu_item(
                            "app-menu-command-palette",
                            "icons/search.svg",
                            "Command Palette",
                            &t,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.close_menu(cx);
                            window.dispatch_action(Box::new(ShowCommandPalette), cx);
                        })),
                    )
                    // Keyboard Shortcuts
                    .child(
                        menu_item(
                            "app-menu-keybindings",
                            "icons/keyboard.svg",
                            "Keyboard Shortcuts",
                            &t,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.close_menu(cx);
                            window.dispatch_action(Box::new(ShowKeybindings), cx);
                        })),
                    )
                    // Separator
                    .child(div().h(px(1.0)).mx(px(8.0)).my(px(4.0)).bg(rgb(t.border)))
                    // Exit
                    .child(
                        menu_item("app-menu-exit", "icons/close.svg", "Exit", &t).on_click(
                            cx.listener(|this, _, window, cx| {
                                this.close_menu(cx);
                                window.dispatch_action(Box::new(Quit), cx);
                            }),
                        ),
                    ),
            )
    }

    /// Render a titlebar icon button that dispatches an action on click.
    fn render_action_button(
        &self,
        id: &'static str,
        icon_path: &'static str,
        active: bool,
        action: Box<dyn gpui::Action>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx);
        let action_focus_handle = self.action_focus_handle.clone();
        let icon_color = if active {
            t.text_primary
        } else {
            t.text_muted
        };
        div()
            .id(id)
            .cursor_pointer()
            .w(px(28.0))
            .h(px(28.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.0))
            .hover(|s| s.opacity(0.85))
            .child(
                svg()
                    .path(icon_path)
                    .size(px(16.0))
                    .text_color(rgb(icon_color)),
            )
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                if let Some(focus_handle) = action_focus_handle.as_ref() {
                    window.focus(focus_handle, cx);
                }
                window.dispatch_action(action.boxed_clone(), cx);
            })
    }

    /// Bell button with a badge showing how many terminals need attention.
    /// Hovering opens the dropdown listing them.
    fn render_bell_button(&self, count: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        div()
            .id("tb-bell")
            .relative()
            .cursor_pointer()
            .w(px(28.0))
            .h(px(28.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.0))
            .hover(|s| s.opacity(0.85))
            .child(
                svg()
                    .path("icons/bell.svg")
                    .size(px(16.0))
                    .text_color(rgb(if count > 0 { t.border_bell } else { t.text_muted })),
            )
            .when(count > 0, |d| {
                d.child(
                    div()
                        .absolute()
                        .top(px(-1.0))
                        .right(px(-3.0))
                        .min_w(px(14.0))
                        .h(px(14.0))
                        .px(px(3.0))
                        .rounded_full()
                        .bg(rgb(t.border_bell))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(ui_text(9.0, cx))
                        .font_weight(FontWeight::BOLD)
                        .text_color(rgb(t.bg_primary))
                        .child(count.to_string()),
                )
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_hover(cx.listener(|this, hovered: &bool, _window, cx| {
                this.bell_button_hovered = *hovered;
                if *hovered {
                    if !this.bell_menu_open {
                        this.bell_menu_open = true;
                        cx.notify();
                    }
                } else {
                    this.schedule_bell_menu_close(cx);
                }
            }))
    }

    /// The dropdown under the bell: one row per terminal needing attention.
    fn render_bell_menu(
        &self,
        items: Vec<AttentionItem>,
        left_offset: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx);
        div()
            .id("tb-bell-menu")
            .absolute()
            .top(px(notmux_ui::tokens::TITLE_BAR_STRIP_H - 4.0))
            .left(left_offset)
            .occlude()
            .bg(rgb(t.bg_primary))
            .border_1()
            .border_color(rgb(t.border))
            .rounded(px(4.0))
            .shadow_xl()
            .min_w(px(260.0))
            .max_w(px(380.0))
            .py(px(4.0))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_hover(cx.listener(|this, hovered: &bool, _window, cx| {
                this.bell_menu_hovered = *hovered;
                if !*hovered {
                    this.schedule_bell_menu_close(cx);
                }
            }))
            .children(items.into_iter().map(|item| {
                let AttentionItem {
                    project_id,
                    terminal_id,
                    name,
                    project_name,
                    detail,
                    has_bell,
                } = item;
                let dot_color = if has_bell { t.border_bell } else { t.border_focused };
                div()
                    .id(ElementId::Name(
                        format!("tb-bell-item-{}", terminal_id).into(),
                    ))
                    .cursor_pointer()
                    .mx(px(4.0))
                    .px(px(8.0))
                    .py(px(5.0))
                    .rounded(px(4.0))
                    .hover(|s| s.bg(rgb(t.bg_hover)))
                    .flex()
                    .flex_col()
                    .gap(px(1.0))
                    .child(
                        h_flex()
                            .gap(px(6.0))
                            .items_center()
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .w(px(6.0))
                                    .h(px(6.0))
                                    .rounded_full()
                                    .bg(rgb(dot_color)),
                            )
                            .child(
                                div()
                                    .flex_grow(1.0)
                                    .overflow_hidden()
                                    .text_size(ui_text_sm(cx))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(rgb(t.text_primary))
                                    .whitespace_nowrap()
                                    .child(name),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_size(ui_text(10.0, cx))
                                    .text_color(rgb(t.text_muted))
                                    .whitespace_nowrap()
                                    .child(project_name),
                            ),
                    )
                    .child(
                        div()
                            .pl(px(12.0))
                            .text_size(ui_text(10.0, cx))
                            .text_color(rgb(t.text_muted))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .child(detail),
                    )
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        cx.stop_propagation();
                        // A bell is acknowledged by the click itself; a
                        // waiting-for-input entry stays listed until the
                        // terminal actually receives input (see
                        // `waiting_entries`).
                        if let Some(t) = this.terminals.lock().get(&terminal_id)
                            && t.has_bell()
                        {
                            t.clear_notification();
                        }
                        this.bell_menu_open = false;
                        this.workspace.update(cx, |ws, cx| {
                            ws.focus_terminal_by_id(&project_id, &terminal_id, cx);
                        });
                        cx.notify();
                    }))
            }))
    }

    /// Render a side-panel toggle with separate VS Code-style glyph states.
    fn render_panel_toggle_button(
        &self,
        id: &'static str,
        on_icon_path: &'static str,
        off_icon_path: &'static str,
        active: bool,
        action: Box<dyn gpui::Action>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let icon_path = if active {
            on_icon_path
        } else {
            off_icon_path
        };
        self.render_action_button(id, icon_path, active, action, cx)
    }
}

impl TitleBar {
    /// Left cluster overlay (top-left): sidebar toggle (+ app menu on non-macOS),
    /// after the traffic-light padding. Rendered by RootView as its own corner
    /// overlay so nothing covers the tab region in the middle.
    pub fn render_left_cluster(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let t = theme(cx);
        // In macOS fullscreen the traffic lights auto-hide — collapse their
        // padding so the sidebar toggle doesn't float 80px from the edge.
        let traffic_light_padding = if cfg!(target_os = "macos") && !window.is_fullscreen() {
            px(80.0)
        } else {
            px(8.0)
        };
        let attention_items = self.attention_items(cx);
        let attention_count = attention_items.len();
        let pair_active = pair_button_active(cx);
        let cluster = h_flex()
            .h(px(notmux_ui::tokens::TITLE_BAR_STRIP_H))
            .items_center()
            .gap(px(4.0))
            .pl(traffic_light_padding)
            .window_control_area(WindowControlArea::Drag)
            .child(self.render_panel_toggle_button(
                "tb-toggle-sidebar",
                "icons/layout-sidebar-left.svg",
                "icons/layout-sidebar-left-off.svg",
                self.sidebar_open,
                Box::new(ToggleSidebar),
                cx,
            ))
            .child(self.render_bell_button(attention_count, cx))
            // Pair button (shown while the remote server is running), next to
            // the bell so the top-left tab reserve covers both.
            .when(pair_active, |d| {
                d.child(
                    div()
                        .id("tb-pair-btn")
                        .cursor_pointer()
                        .w(px(28.0))
                        .h(px(28.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.0))
                        .hover(|s| s.opacity(0.85))
                        .child(
                            svg()
                                .path("icons/link.svg")
                                .size(px(14.0))
                                .text_color(rgb(t.term_yellow)),
                        )
                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_click(|_, window, cx| {
                            cx.stop_propagation();
                            window.dispatch_action(
                                Box::new(crate::keybindings::ShowPairingDialog),
                                cx,
                            );
                        }),
                )
            })
            .when(!cfg!(target_os = "macos"), |d| {
                d.child({
                    let menu_open = self.menu_open;
                    let chevron = if menu_open { "▲" } else { "▼" };
                    div()
                        .id("app-menu-trigger")
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .px(px(8.0))
                        .py(px(4.0))
                        .rounded(px(4.0))
                        .hover(|s| s.opacity(0.85))
                        .child(
                            div()
                                .text_size(ui_text(13.0, cx))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(rgb(t.text_primary))
                                .child(self.title.clone()),
                        )
                        .child(
                            div()
                                .text_size(ui_text(8.0, cx))
                                .text_color(rgb(t.text_muted))
                                .child(chevron),
                        )
                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_click(cx.listener(|this, _, _window, cx| {
                            cx.stop_propagation();
                            this.toggle_menu(cx);
                        }))
                })
            });
        // Wrapper so the bell dropdown can hang below the strip without
        // being part of the drag area.
        div()
            .relative()
            .child(cluster)
            .when(self.bell_menu_open && attention_count > 0, |d| {
                // Align the dropdown with the bell button (toggle 28px + gap 4px).
                d.child(self.render_bell_menu(
                    attention_items,
                    traffic_light_padding + px(32.0),
                    cx,
                ))
            })
    }

    /// Right cluster overlay (top-right): git-panel toggle + settings + native
    /// window controls.
    pub fn render_right_cluster(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let needs_controls = needs_client_window_controls(window);
        h_flex()
            .h(px(notmux_ui::tokens::TITLE_BAR_STRIP_H))
            .gap(px(4.0))
            .pr(px(4.0))
            .items_center()
            .window_control_area(WindowControlArea::Drag)
            .child(self.render_panel_toggle_button(
                "tb-toggle-git-panel",
                "icons/layout-sidebar-right.svg",
                "icons/layout-sidebar-right-off.svg",
                self.git_panel_open,
                Box::new(ToggleGitPanel),
                cx,
            ))
            .when(needs_controls, |d| {
                d.child(div().ml(px(4.0)).child(window_controls_cluster(window, cx)))
            })
    }
}

impl Render for TitleBar {
    #[allow(unused_variables)]
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

#[cfg(test)]
mod tests {
    use super::update_waiting_entries;
    use std::collections::HashMap;

    fn snap(rows: &[(&str, bool, bool, u64)]) -> Vec<(String, bool, bool, u64)> {
        rows.iter()
            .map(|(tid, w, b, g)| (tid.to_string(), *w, *b, *g))
            .collect()
    }

    #[test]
    fn waiting_terminal_is_added() {
        let mut entries = HashMap::new();
        update_waiting_entries(&snap(&[("t1", true, false, 3)]), &mut entries);
        assert_eq!(entries.get("t1"), Some(&3));
    }

    #[test]
    fn entry_survives_focus_without_input() {
        // Focusing the pane clears the live waiting flag but does not bump
        // the input generation — the entry must stay.
        let mut entries = HashMap::new();
        update_waiting_entries(&snap(&[("t1", true, false, 3)]), &mut entries);
        update_waiting_entries(&snap(&[("t1", false, false, 3)]), &mut entries);
        assert!(entries.contains_key("t1"));
    }

    #[test]
    fn entry_removed_after_input() {
        let mut entries = HashMap::new();
        update_waiting_entries(&snap(&[("t1", true, false, 3)]), &mut entries);
        update_waiting_entries(&snap(&[("t1", false, false, 4)]), &mut entries);
        assert!(entries.is_empty());
    }

    #[test]
    fn entry_removed_when_terminal_disappears() {
        let mut entries = HashMap::new();
        update_waiting_entries(&snap(&[("t1", true, false, 3)]), &mut entries);
        update_waiting_entries(&snap(&[]), &mut entries);
        assert!(entries.is_empty());
    }

    #[test]
    fn generation_snapshot_taken_on_first_sighting() {
        // If input happens and the terminal waits again later, the entry is
        // re-added with the new generation.
        let mut entries = HashMap::new();
        update_waiting_entries(&snap(&[("t1", true, false, 3)]), &mut entries);
        update_waiting_entries(&snap(&[("t1", false, false, 4)]), &mut entries);
        assert!(entries.is_empty());
        update_waiting_entries(&snap(&[("t1", true, false, 4)]), &mut entries);
        assert_eq!(entries.get("t1"), Some(&4));
    }
}
