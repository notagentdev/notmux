//! Theme colors provider for vryn-files.
//!
//! Re-exports `GlobalThemeProvider` and `theme()` from vryn-ui so that
//! existing imports (`vryn_files::theme::theme`) keep working.

pub use vryn_ui::theme::{GlobalThemeProvider, theme};
