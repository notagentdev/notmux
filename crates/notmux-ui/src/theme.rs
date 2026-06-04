//! Theme helpers — re-exported from notmux-theme.
pub use notmux_theme::{
    DARK_THEME,
    // Core types (via notmux-theme which re-exports from notmux-core)
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
