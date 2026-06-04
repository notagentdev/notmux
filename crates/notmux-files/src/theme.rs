//! Theme colors provider for notmux-files.
//!
//! Re-exports `GlobalThemeProvider` and `theme()` from notmux-ui so that
//! existing imports (`notmux_files::theme::theme`) keep working.

pub use notmux_ui::theme::{GlobalThemeProvider, theme};
