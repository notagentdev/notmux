// Re-export core theme types (source of truth is vryn-core)
pub use vryn_core::theme::{
    DARK_THEME, FolderColor, HIGH_CONTRAST_THEME, LIGHT_THEME, PASTEL_DARK_THEME, ThemeColors,
    ThemeInfo, ThemeMode,
};

mod app_theme;
pub mod builtin_vscode;
pub mod custom;
mod gpui_helpers;
pub mod vscode;

pub use app_theme::{AppTheme, GlobalTheme, theme_entity};
pub use builtin_vscode::DEFAULT_THEME_ID;
pub use custom::{CustomThemeColors, CustomThemeConfig, get_themes_dir, load_custom_themes};
pub use gpui_helpers::{GlobalThemeProvider, ansi_to_hsla, theme, with_alpha};
