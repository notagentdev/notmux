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

/// Title bar with window controls and sidebar toggle
pub struct TitleBar {
    title: SharedString,
    menu_open: bool,
    sidebar_open: bool,
    git_panel_open: bool,
    action_focus_handle: Option<FocusHandle>,
    _workspace_subscription: Subscription,
    /// Flag for Linux compositor-driven window move (set on mouse-down, consumed on mouse-move)
    #[cfg(target_os = "linux")]
    should_move: bool,
}

impl TitleBar {
    pub fn new(
        title: impl Into<SharedString>,
        workspace: Entity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.observe(&workspace, |_, _, cx| cx.notify());
        Self {
            title: title.into(),
            menu_open: false,
            sidebar_open: true,
            git_panel_open: false,
            action_focus_handle: None,
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
        h_flex()
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
