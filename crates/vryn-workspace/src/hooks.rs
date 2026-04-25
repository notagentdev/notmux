//! Re-exports the hook execution surface from `vryn-hooks` so that existing
//! `crate::hooks::*` callers (and `vryn_workspace::hooks::*` from outside)
//! keep working after the split.
pub use vryn_hooks::hooks::*;
