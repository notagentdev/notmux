//! Theme helpers — re-exported from vryn-theme.
pub use vryn_theme::{
    DARK_THEME,
    // Core types (via vryn-theme which re-exports from vryn-core)
    FolderColor,
    GlobalThemeProvider,
    HIGH_CONTRAST_THEME,
    LIGHT_THEME,
    PASTEL_DARK_THEME,
    ThemeColors,
    ThemeInfo,
    ThemeMode,
    ansi_to_hsla,
    theme,
    // GPUI helpers
    with_alpha,
};
