// Re-export everything from the notmux-git crate.
// This allows existing `use crate::git::*` imports to keep working.
pub use notmux_git::*;

// Watcher re-exported from notmux-views-git crate
pub use notmux_views_git::watcher;
