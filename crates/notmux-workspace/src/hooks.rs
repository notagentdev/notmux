//! Re-exports the hook execution surface from `notmux-hooks` so that existing
//! `crate::hooks::*` callers (and `notmux_workspace::hooks::*` from outside)
//! keep working after the split.
pub use notmux_hooks::hooks::*;
