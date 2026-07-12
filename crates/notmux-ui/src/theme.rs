//! Theme helpers — re-exported from notmux-theme.
pub use notmux_theme::{
    DARK_THEME,
    // Core types (via notmux-theme which re-exports from notmux-core)
    FolderColor,
    GlobalThemeProvider,
    LIGHT_THEME,
    ThemeColors,
    ansi_to_hsla,
    git_theme,
    sidebar_theme,
    theme,
    // GPUI helpers
    with_alpha,
};
