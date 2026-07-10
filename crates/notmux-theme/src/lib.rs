//! Theme system: the notagent JSON theme engine (`notagent_theme`) plus the
//! GPUI adapter (`GpuiTheme`) and the bridges that map it onto the
//! `ThemeColors` palette the notmux views read.

use std::sync::Arc;

// Re-export core theme types (source of truth is notmux-core)
pub use notmux_core::theme::{DARK_THEME, FolderColor, LIGHT_THEME, ThemeColors};

// The notagent theme engine: built-in JSON themes, custom themes directory,
// variable resolution.
pub use notagent_theme::{
    BUILTIN_THEMES, ColorMode, Theme, ThemeBg, ThemeColor, ThemeJson, get_custom_themes_dir,
    list_available_themes, load_theme_json,
};

mod bridge;
mod gpui_helpers;
mod gpui_theme;

pub use bridge::{git_theme, sidebar_theme, terminal_theme};
pub use gpui_helpers::{GlobalThemeProvider, ansi_to_hsla, theme, with_alpha};
pub use gpui_theme::GpuiTheme;

/// The default theme name used on first launch (no prior settings) and as
/// fallback when a persisted theme fails to load — same as notagent.
pub const DEFAULT_THEME_NAME: &str = "dark";

/// Load a theme by name into a [`GpuiTheme`].
///
/// Colors are resolved in truecolor so the GUI always renders the exact hex
/// values from the theme JSON (terminal color-mode detection only matters for
/// ANSI output, not for GPUI rendering).
///
/// # Errors
/// Returns an error string if the theme is not found or fails to resolve.
pub fn load_gpui_theme(name: &str) -> Result<GpuiTheme, String> {
    let json = load_theme_json(name)?;
    let theme = Theme::from_json(json, ColorMode::TrueColor, Some(name.to_string()))?;
    Ok(GpuiTheme::new(Arc::new(theme)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_builtin_themes_load_as_gpui_themes() {
        for name in BUILTIN_THEMES {
            let theme = load_gpui_theme(name)
                .unwrap_or_else(|e| panic!("built-in theme '{name}' failed to load: {e}"));
            assert_ne!(
                theme.fg(ThemeColor::Accent),
                gpui::white(),
                "theme '{name}' accent should differ from fallback white"
            );
        }
    }

    #[test]
    fn default_theme_is_dark() {
        assert_eq!(DEFAULT_THEME_NAME, "dark");
        assert!(load_gpui_theme(DEFAULT_THEME_NAME).is_ok());
    }
}
