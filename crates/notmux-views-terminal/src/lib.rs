//! NotMux terminal views crate.
//!
//! Contains custom GPUI elements for terminal rendering and the layout system
//! (split panes, tabs, terminal panes) used by the main application.

pub mod actions;
pub mod elements;
pub mod layout;
pub mod overlays;
pub mod shell_selector_overlay;

mod simple_input;

use notmux_core::api::ActionRequest;
use notmux_workspace::state::SplitDirection;

/// Trait for dispatching terminal actions (local or remote).
///
/// This abstracts the `ActionDispatcher` enum from the main application,
/// allowing the layout views to dispatch actions without knowing whether
/// the project is local or remote.
pub trait ActionDispatch: Clone + 'static {
    /// Dispatch a standard action.
    fn dispatch(&self, action: ActionRequest, cx: &mut gpui::App);

    /// Whether this dispatcher targets a remote project.
    fn is_remote(&self) -> bool;

    /// Split a terminal.
    fn split_terminal(
        &self,
        project_id: &str,
        layout_path: &[usize],
        direction: SplitDirection,
        cx: &mut gpui::App,
    );

    /// Add a tab.
    fn add_tab(&self, project_id: &str, layout_path: &[usize], in_group: bool, cx: &mut gpui::App);
}

/// Settings namespace used in ExtensionSettingsStore.
const SETTINGS_ID: &str = "terminal";

/// Settings needed by terminal views.
///
/// Read/written through `ExtensionSettingsStore` so that changes flow through
/// the host app's persistence system automatically.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TerminalViewSettings {
    pub font_size: f32,
    pub line_height: f32,
    pub font_family: String,
    pub cursor_style: notmux_workspace::settings::CursorShape,
    pub cursor_blink: bool,
    pub show_focused_border: bool,
    #[serde(default = "default_show_notification_label")]
    pub show_notification_label: bool,
    pub show_shell_selector: bool,
    pub idle_timeout_secs: u32,
    pub color_tinted_background: bool,
    #[serde(default)]
    pub monochrome_icons: bool,
    pub file_opener: String,
    pub default_shell: notmux_terminal::shell_config::ShellType,
    pub hooks: notmux_workspace::settings::HooksConfig,
    #[serde(default = "default_persist_scrollback")]
    pub persist_scrollback: bool,
    #[serde(default = "default_persist_scrollback_lines")]
    pub persist_scrollback_lines: u32,
    #[serde(default)]
    pub terminal_env: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub terminal_working_directory: notmux_workspace::settings::TerminalWorkingDirectory,
    #[serde(default)]
    pub option_as_meta: bool,
}

fn default_show_notification_label() -> bool {
    true
}
fn default_persist_scrollback() -> bool {
    true
}
fn default_persist_scrollback_lines() -> u32 {
    100
}

/// Read current terminal view settings from ExtensionSettingsStore.
pub fn terminal_view_settings(cx: &gpui::App) -> TerminalViewSettings {
    let store = cx.global::<notmux_extensions::ExtensionSettingsStore>();
    store
        .get(SETTINGS_ID, cx)
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_else(|| TerminalViewSettings {
            font_size: 13.0,
            line_height: 1.3,
            font_family: "JetBrains Mono".to_string(),
            cursor_style: Default::default(),
            cursor_blink: false,
            show_focused_border: false,
            show_notification_label: true,
            show_shell_selector: false,
            idle_timeout_secs: 0,
            color_tinted_background: false,
            monochrome_icons: false,
            file_opener: String::new(),
            default_shell: notmux_terminal::shell_config::ShellType::Default,
            hooks: Default::default(),
            persist_scrollback: default_persist_scrollback(),
            persist_scrollback_lines: default_persist_scrollback_lines(),
            terminal_env: Default::default(),
            terminal_working_directory: Default::default(),
            option_as_meta: false,
        })
}

/// Write terminal view settings to ExtensionSettingsStore.
pub fn set_terminal_view_settings(settings: &TerminalViewSettings, cx: &mut gpui::App) {
    if let Ok(value) = serde_json::to_value(settings) {
        notmux_extensions::ExtensionSettingsStore::update(SETTINGS_ID, value, cx);
    }
}

/// Callback type for registering content panes for dirty notification.
pub type RegisterContentPaneFn =
    Box<dyn Fn(String, gpui::WeakEntity<layout::terminal_pane::TerminalContent>) + Send + Sync>;

/// Global content pane registration function.
static REGISTER_CONTENT_PANE_FN: std::sync::OnceLock<RegisterContentPaneFn> =
    std::sync::OnceLock::new();

/// Set the global content pane registration function.
/// Called once by the main app at startup.
pub fn set_register_content_pane_fn(f: RegisterContentPaneFn) {
    let _ = REGISTER_CONTENT_PANE_FN.set(f);
}

/// Register a terminal content pane for direct dirty notification.
pub fn register_content_pane(
    terminal_id: String,
    content: gpui::WeakEntity<layout::terminal_pane::TerminalContent>,
) {
    if let Some(f) = REGISTER_CONTENT_PANE_FN.get() {
        f(terminal_id, content);
    }
}

/// Callback type for showing toast notifications.
pub type ToastErrorFn = Box<dyn Fn(String, &mut gpui::App) + Send + Sync>;

/// Global toast error function.
static TOAST_ERROR_FN: std::sync::OnceLock<ToastErrorFn> = std::sync::OnceLock::new();

/// Set the global toast error function.
pub fn set_toast_error_fn(f: ToastErrorFn) {
    let _ = TOAST_ERROR_FN.set(f);
}

/// Show an error toast notification.
pub fn toast_error(msg: String, cx: &mut gpui::App) {
    if let Some(f) = TOAST_ERROR_FN.get() {
        f(msg, cx);
    } else {
        log::error!("{}", msg);
    }
}

// --- Title-bar overlay reserves -------------------------------------------
// When the host renders a transparent title-bar overlay on top of the tab bars
// (macOS traffic lights + toggles on the left, window controls on the right),
// the pane touching that edge must inset its tabs/buttons so they aren't
// covered. These globals let the host set that inset per edge; the tab bar
// applies them only to the edge pane. All default to 0 (no change).

/// Right space (px) reserved at the end of the top-right pane's action buttons.
#[derive(Clone, Copy, Default)]
pub struct TabActionRightReserve(pub f32);
impl gpui::Global for TabActionRightReserve {}

pub fn tab_action_right_reserve(cx: &gpui::App) -> f32 {
    cx.try_global::<TabActionRightReserve>()
        .map(|r| r.0)
        .unwrap_or(0.0)
}

pub fn set_tab_action_right_reserve(reserve: f32, cx: &mut gpui::App) {
    cx.set_global(TabActionRightReserve(reserve));
}

/// Layout path of the pane whose tab bar carries the right reserve (the pane at
/// the top-right corner). `None` applies it to every pane's bar.
#[derive(Clone, Default)]
pub struct TabActionRightReservePath(pub Option<Vec<usize>>);
impl gpui::Global for TabActionRightReservePath {}

/// The right reserve (px) that applies to the tab bar at `layout_path`.
pub fn tab_action_right_reserve_for(layout_path: &[usize], cx: &gpui::App) -> f32 {
    let applies = cx
        .try_global::<TabActionRightReservePath>()
        .and_then(|p| p.0.as_deref())
        .is_none_or(|path| path == layout_path);
    if applies {
        tab_action_right_reserve(cx)
    } else {
        0.0
    }
}

pub fn set_tab_action_right_reserve_path(path: Option<Vec<usize>>, cx: &mut gpui::App) {
    cx.set_global(TabActionRightReservePath(path));
}

/// Left space (px) reserved at the start of the top-left pane's tab bar.
#[derive(Clone, Copy, Default)]
pub struct TabActionLeftReserve(pub f32);
impl gpui::Global for TabActionLeftReserve {}

pub fn tab_action_left_reserve(cx: &gpui::App) -> f32 {
    cx.try_global::<TabActionLeftReserve>()
        .map(|r| r.0)
        .unwrap_or(0.0)
}

pub fn set_tab_action_left_reserve(reserve: f32, cx: &mut gpui::App) {
    cx.set_global(TabActionLeftReserve(reserve));
}

/// Height (px) of the terminal tab bar. The host raises it so the tab strip
/// vertically centers with a taller title-bar overlay floating over it.
/// Defaults to 32 (notmux standalone).
#[derive(Clone, Copy)]
pub struct TabBarHeight(pub f32);
impl gpui::Global for TabBarHeight {}
impl Default for TabBarHeight {
    fn default() -> Self {
        Self(32.0)
    }
}

pub fn tab_bar_height(cx: &gpui::App) -> f32 {
    cx.try_global::<TabBarHeight>().map(|h| h.0).unwrap_or(32.0)
}

pub fn set_tab_bar_height(height: f32, cx: &mut gpui::App) {
    cx.set_global(TabBarHeight(height));
}

