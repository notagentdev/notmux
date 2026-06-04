//! Theme module — re-exports from notmux-theme crate.

// Re-export everything from notmux-theme
#[allow(unused_imports)]
pub use notmux_theme::{
    AppTheme, CustomThemeColors, CustomThemeConfig, DARK_THEME, FolderColor, GlobalTheme,
    HIGH_CONTRAST_THEME, LIGHT_THEME, PASTEL_DARK_THEME, ThemeColors, ThemeInfo, ThemeMode,
    ansi_to_hsla, get_themes_dir, load_custom_themes, theme_entity, with_alpha,
};

use gpui::*;

/// Get the current theme colors from the global theme entity (uses preview if active).
/// This is the desktop app's theme() — reads from GlobalTheme entity directly.
/// Different from notmux_theme::theme() which uses GlobalThemeProvider function pointer.
pub fn theme(cx: &App) -> ThemeColors {
    cx.global::<GlobalTheme>().0.read(cx).display_colors()
}
