// Re-export everything from the vryn-terminal crate.
// This allows existing `use crate::terminal::*` imports to keep working.
pub use vryn_terminal::backend;
pub use vryn_terminal::pty_manager;
pub use vryn_terminal::session_backend;
pub use vryn_terminal::shell_config;
pub use vryn_terminal::terminal;

pub mod snapshot_persist;
