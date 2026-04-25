// Re-export everything from the vryn-git crate.
// This allows existing `use crate::git::*` imports to keep working.
pub use vryn_git::*;

// Watcher re-exported from vryn-views-git crate
pub use vryn_views_git::watcher;

