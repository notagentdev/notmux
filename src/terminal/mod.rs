// Re-export everything from the notmux-terminal crate.
// This allows existing `use crate::terminal::*` imports to keep working.
pub use notmux_terminal::backend;
pub use notmux_terminal::pty_manager;
pub use notmux_terminal::session_backend;
pub use notmux_terminal::shell_config;
pub use notmux_terminal::terminal;

pub mod snapshot_persist;
