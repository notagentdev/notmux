// Re-export core theme types (source of truth is vryn-core)
pub use vryn_core::theme::{
    ThemeColors, ThemeInfo, ThemeMode, FolderColor,
    DARK_THEME, LIGHT_THEME, PASTEL_DARK_THEME, HIGH_CONTRAST_THEME,
};

pub mod custom;
pub mod vscode;
pub mod builtin_vscode;
mod gpui_helpers;
mod app_theme;

pub use gpui_helpers::{with_alpha, ansi_to_hsla, GlobalThemeProvider, theme};
pub use app_theme::{AppTheme, GlobalTheme, theme_entity};
pub use custom::{CustomThemeConfig, CustomThemeColors, get_themes_dir, load_custom_themes};
pub use builtin_vscode::DEFAULT_THEME_ID;
