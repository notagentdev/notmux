pub mod agent_sessions;
pub mod backend;
pub mod input;
pub mod process;
pub mod pty_manager;
pub mod scrollback_snapshot;
pub mod session_backend;
pub mod shell_config;
pub mod terminal;

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

/// Shared terminals registry for PTY event routing.
/// Maps terminal ID → Terminal instance.
pub type TerminalsRegistry = Arc<Mutex<HashMap<String, Arc<terminal::Terminal>>>>;

/// Global handle to the active `TerminalsRegistry`, set once by the host app
/// after construction. Lets non-GPUI code (e.g. shutdown hooks) iterate live
/// terminals without threading the registry through every call site.
static GLOBAL_REGISTRY: OnceLock<TerminalsRegistry> = OnceLock::new();

pub fn set_global_registry(registry: TerminalsRegistry) {
    let _ = GLOBAL_REGISTRY.set(registry);
}

pub fn global_registry() -> Option<&'static TerminalsRegistry> {
    GLOBAL_REGISTRY.get()
}
