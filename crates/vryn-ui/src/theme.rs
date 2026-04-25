//! Theme helpers — re-exported from vryn-theme.
pub use vryn_theme::{
    // Core types (via vryn-theme which re-exports from vryn-core)
    FolderColor, ThemeColors, ThemeInfo, ThemeMode,
    DARK_THEME, HIGH_CONTRAST_THEME, LIGHT_THEME, PASTEL_DARK_THEME,
    // GPUI helpers
    with_alpha, ansi_to_hsla, GlobalThemeProvider, theme,
};
