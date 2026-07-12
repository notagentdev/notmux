//! Theme module — the notagent theme system (via notmux-theme).
//!
//! The active theme lives in the `GpuiTheme` global. The notmux views read
//! their `ThemeColors` palette through the bridge functions, exactly like the
//! notagent GUI themes these same views when it embeds them.

// Re-export everything from notmux-theme
#[allow(unused_imports)]
pub use notmux_theme::{
    BUILTIN_THEMES, DARK_THEME, DEFAULT_THEME_NAME, FolderColor, GpuiTheme, LIGHT_THEME, Theme,
    ThemeBg, ThemeColor, ThemeColors, ansi_to_hsla, get_custom_themes_dir, git_theme,
    list_available_themes, load_gpui_theme, terminal_theme, with_alpha,
};

use std::sync::{Arc, OnceLock};

use gpui::*;
use parking_lot::Mutex;

/// Snapshot of the bridged `ThemeColors` for off-main-thread readers (the OSC
/// color-query resolver on the PTY path). Refreshed on every theme swap.
static COLOR_SNAPSHOT: OnceLock<Arc<Mutex<ThemeColors>>> = OnceLock::new();

/// Shared snapshot of the current bridged theme colors.
pub fn color_snapshot() -> &'static Arc<Mutex<ThemeColors>> {
    COLOR_SNAPSHOT.get_or_init(|| Arc::new(Mutex::new(DARK_THEME)))
}

/// Get the current theme colors: the `ThemeColors` bridge over the active
/// `GpuiTheme` global (the resident bridge is `terminal_theme`, matching the
/// notagent GUI where the terminal panel installs it app-wide).
pub fn theme(cx: &App) -> ThemeColors {
    notmux_theme::terminal_theme(cx)
}

/// Theme for the right panel (Git/Files tabs): the terminal bridge with the
/// sidebar's hover surface, so row hover marking looks identical to the left
/// panel (`surface_3` instead of the terminal chrome's `surface_2`).
pub fn right_panel_theme(cx: &App) -> ThemeColors {
    let mut t = notmux_theme::terminal_theme(cx);
    t.bg_hover = notmux_theme::sidebar_theme(cx).bg_hover;
    t
}

/// Swaps the `GpuiTheme` global and repaints everything (all windows plus the
/// cached terminal content panes, which a window redraw alone does not reach).
/// Does not persist — used for live preview; `set_theme` persists.
pub fn apply_gpui_theme(theme: GpuiTheme, cx: &mut App) {
    cx.set_global(theme);
    *color_snapshot().lock() = notmux_theme::terminal_theme(cx);
    // Keep gpui-component widgets (inputs etc.) on the matching light/dark base.
    let is_dark = cx.global::<GpuiTheme>().bg_base().l <= 0.5;
    let gpui_mode = if is_dark {
        gpui_component::theme::ThemeMode::Dark
    } else {
        gpui_component::theme::ThemeMode::Light
    };
    gpui_component::theme::Theme::change(gpui_mode, None, cx);
    // The terminal reads its palette from the app theme; repaint its panes.
    let panes: Vec<_> = crate::views::root::content_pane_registry()
        .lock()
        .values()
        .cloned()
        .collect();
    for weak in panes {
        let _ = weak.update(cx, |_, cx| cx.notify());
    }
    cx.refresh_windows();
}

/// Applies a theme by name (rebuilds the theme global live) and persists the
/// choice so it is restored on the next launch. Returns false if the theme
/// failed to load (the current theme stays active).
pub fn set_theme(name: &str, cx: &mut App) -> bool {
    match notmux_theme::load_gpui_theme(name) {
        Ok(theme) => {
            apply_gpui_theme(theme, cx);
            crate::settings::settings_entity(cx).update(cx, |s, cx| {
                s.set_theme(name.to_string(), cx);
            });
            true
        }
        Err(e) => {
            log::warn!("Failed to load theme '{}': {}", name, e);
            false
        }
    }
}
