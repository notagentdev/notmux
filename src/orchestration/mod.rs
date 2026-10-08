//! Multi-agent orchestration: one root pane directs up to four worker panes.
//!
//! * [`model`] is the pure state machine. It enforces every rule (root-only
//!   spawn, worker leaf rule, capacity, budget, one active assignment per
//!   worker, immutable results, group-scoped messaging) and never touches
//!   disk, processes, or GPUI.
//! * [`store`] serializes mutations through one writer thread, persists the
//!   document atomically under `<config dir>/orchestration/`, and updates
//!   memory only after the write succeeded.
//! * [`migrate`] lifts the version-2 document of the earlier prototype into
//!   the current layout.
//!
//! The wire contract lives in `notmux_core::orchestration`.

pub mod app_host;
pub mod bootstrap;
pub mod harness;
pub mod migrate;
pub mod model;
pub mod runtime;
pub mod store;

use notmux_core::orchestration::AgentRequest;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

static RUNTIME: OnceLock<runtime::Runtime> = OnceLock::new();

/// Open the process-wide runtime once, at application start. Returns the
/// reason when it could not be opened; the app then runs without agent
/// orchestration and the API answers `storage_failed`.
pub fn install() -> Result<runtime::Runtime, String> {
    if let Some(existing) = RUNTIME.get() {
        return Ok(existing.clone());
    }
    let vendor = harness::VendorPaths::from_env().map_err(|e| e.to_string())?;
    let (rt, opened) = runtime::Runtime::open(&orchestration_dir(), vendor, harness::default_search_path())
        .map_err(|e| e.to_string())?;
    log::info!("orchestration: store opened ({opened:?})");
    let _ = RUNTIME.set(rt.clone());
    Ok(rt)
}

/// The installed runtime, if any.
pub fn runtime() -> Option<runtime::Runtime> {
    RUNTIME.get().cloned()
}

/// What a request handler needs to answer store-only agent commands off
/// the UI thread: the process backend (kill) and the terminal registry
/// (publish state, forget killed terminals). Installed by the app once.
#[derive(Clone)]
pub struct BackgroundParts {
    pub backend: std::sync::Arc<dyn crate::terminal::backend::TerminalBackend>,
    pub terminals: crate::views::root::TerminalsRegistry,
}

static BACKGROUND: OnceLock<BackgroundParts> = OnceLock::new();

pub fn install_background_parts(parts: BackgroundParts) {
    let _ = BACKGROUND.set(parts);
}

pub fn background_parts() -> Option<BackgroundParts> {
    BACKGROUND.get().cloned()
}

type ExitBatch = Vec<(String, Option<u32>)>;
static EXIT_RECORDER: OnceLock<std::sync::Mutex<std::sync::mpsc::Sender<ExitBatch>>> = OnceLock::new();

/// Record PTY exits of managed runs on one long-lived background thread,
/// in the order they happened. The UI thread only hands the batch over.
pub fn record_exits_in_background(exits: ExitBatch) {
    let (Some(runtime), Some(parts)) = (runtime(), background_parts()) else {
        return;
    };
    let sender = EXIT_RECORDER.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<ExitBatch>();
        let spawned = std::thread::Builder::new()
            .name("orchestration-exits".into())
            .spawn(move || {
                while let Ok(batch) = rx.recv() {
                    record_exits(&runtime, parts.clone(), &batch);
                }
            });
        if let Err(e) = spawned {
            log::error!("orchestration: exit recorder not started: {e}");
        }
        std::sync::Mutex::new(tx)
    });
    let _ = sender.lock().unwrap_or_else(|p| p.into_inner()).send(exits);
}

fn record_exits(runtime: &runtime::Runtime, parts: BackgroundParts, exits: &[(String, Option<u32>)]) {
    let mut host = app_host::BackgroundHost {
        backend: parts.backend,
        terminals: parts.terminals,
        project: None,
    };
    for (terminal_id, exit_code) in exits {
        runtime.on_terminal_exit(terminal_id, *exit_code, &mut host);
    }
}

/// `<config dir>/orchestration`: the store, task documents, and per-run
/// files.
pub fn orchestration_dir() -> PathBuf {
    crate::workspace::persistence::config_dir().join("orchestration")
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A fresh lowercase UUID, the only ID form the contract accepts.
pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Key under which a mutating request is remembered: `<caller>/<op>/<id>`.
/// `None` for requests without a request ID.
pub fn request_key(caller_run_id: &str, request: &AgentRequest) -> Option<String> {
    request
        .request_id()
        .map(|id| format!("{caller_run_id}/{}/{id}", request.op()))
}

/// SHA-256 over the serialized request. Two retries with the same ID must
/// carry the same payload; anything else is a conflict, not a retry.
pub fn request_digest(request: &AgentRequest) -> String {
    let bytes = serde_json::to_vec(request).unwrap_or_default();
    let digest = Sha256::digest(&bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}
