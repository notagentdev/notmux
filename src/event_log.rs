//! Append-only NDJSON event log (`~/.config/notmux/events.jsonl`).
//!
//! Every mutating action, notification, and terminal exit is appended as one
//! JSON object per line so external tools can observe app activity (`tail -f`
//! or `notmux events --follow`). Read-only query actions are not logged —
//! polling clients (web/mobile) would flood the file — and neither is
//! transient per-mouse-move UI state like split resizes (see
//! [`is_unlogged_action`]).
//!
//! The log rotates at 10 MB: the current file is renamed to
//! `events.jsonl.1` (replacing any previous generation) and a fresh file is
//! started, so the log never grows unbounded.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel};
use std::time::{SystemTime, UNIX_EPOCH};

/// Rotate when the log exceeds this size.
const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;

/// Path of the event log in the config dir.
pub fn event_log_path() -> PathBuf {
    crate::workspace::persistence::config_dir().join("events.jsonl")
}

/// Channel to the dedicated writer thread. Emitters only serialize + send;
/// all filesystem work (open, size check, rotation, write) happens on the
/// writer thread so UI and PTY paths never block on disk I/O.
static WRITER_TX: OnceLock<Sender<String>> = OnceLock::new();

fn writer_tx() -> &'static Sender<String> {
    WRITER_TX.get_or_init(|| {
        let (tx, rx) = channel::<String>();
        std::thread::Builder::new()
            .name("event-log-writer".into())
            .spawn(move || {
                let path = event_log_path();
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                // Keep the file open across writes; track its size ourselves
                // so rotation needs no per-line metadata call.
                let mut file: Option<std::fs::File> = None;
                let mut written: u64 = 0;
                while let Ok(line) = rx.recv() {
                    if written >= MAX_LOG_BYTES {
                        file = None;
                        let _ = std::fs::rename(&path, path.with_extension("jsonl.1"));
                        written = 0;
                    }
                    if file.is_none() {
                        match OpenOptions::new().create(true).append(true).open(&path) {
                            Ok(f) => {
                                written = f.metadata().map(|m| m.len()).unwrap_or(0);
                                file = Some(f);
                            }
                            Err(e) => {
                                log::warn!("Failed to open event log: {}", e);
                                continue;
                            }
                        }
                    }
                    if let Some(ref mut f) = file {
                        if let Err(e) = f.write_all(line.as_bytes()) {
                            log::warn!("Failed to write event log: {}", e);
                            file = None;
                        } else {
                            written += line.len() as u64;
                        }
                    }
                }
            })
            .expect("spawn event-log writer thread");
        tx
    })
}

/// Append an event line: `{"ts_ms":…,"event":"<kind>", …payload}`.
/// Fire-and-forget — failures are logged and never propagate.
pub fn emit(kind: &str, payload: serde_json::Value) {
    let ts_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut line = serde_json::json!({ "ts_ms": ts_ms, "event": kind });
    if let (Some(obj), Some(extra)) = (line.as_object_mut(), payload.as_object()) {
        for (k, v) in extra {
            obj.insert(k.clone(), v.clone());
        }
    }
    let Ok(mut text) = serde_json::to_string(&line) else {
        return;
    };
    text.push('\n');

    // Hand off to the writer thread — never touch the filesystem here.
    let _ = writer_tx().send(text);
}

/// Whether an action must not be logged: read-only queries (polling clients
/// would flood the file) and transient high-frequency UI state (split-resize
/// and terminal-resize fire per mouse-move during drags — logging them costs
/// one file write per event and makes dragging laggy).
pub fn is_unlogged_action(action: &notmux_core::api::ActionRequest) -> bool {
    use notmux_core::api::ActionRequest as A;
    matches!(
        action,
        A::UpdateSplitSizes { .. }
            | A::Resize { .. }
            | A::ReadContent { .. }
            | A::AgentExplain { .. }
            | A::GitStatus { .. }
            | A::GitDiffSummary { .. }
            | A::GitDiff { .. }
            | A::GitBranches { .. }
            | A::GitFileContents { .. }
            | A::GitCommitLog { .. }
            | A::GitListBranches { .. }
            | A::GitWorkingTreeStatus { .. }
            | A::ListFiles { .. }
            | A::ReadFile { .. }
            | A::FileSize { .. }
            | A::SearchContent { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use notmux_core::api::ActionRequest;

    #[test]
    fn read_only_and_transient_actions_are_not_logged() {
        assert!(is_unlogged_action(&ActionRequest::ReadContent {
            terminal_id: "t".into()
        }));
        assert!(is_unlogged_action(&ActionRequest::ListFiles {
            project_id: "p".into(),
            show_ignored: false,
            show_hidden: false,
        }));
        // Per-mouse-move drag traffic stays out of the log.
        assert!(is_unlogged_action(&ActionRequest::UpdateSplitSizes {
            project_id: "p".into(),
            path: vec![],
            sizes: vec![50.0, 50.0],
        }));
        assert!(!is_unlogged_action(&ActionRequest::SendText {
            terminal_id: "t".into(),
            text: "x".into()
        }));
        assert!(!is_unlogged_action(&ActionRequest::CreateTerminal {
            project_id: "p".into()
        }));
    }
}
