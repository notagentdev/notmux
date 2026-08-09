//! `notmux agent-session` — invoked by agent lifecycle hooks to persist
//! restorable session records (see `notmux_terminal::agent_sessions`).
//!
//! `record` reads the agent's hook payload from stdin (JSON) and/or explicit
//! flags and stores `{kind, session_id, cwd, …}` keyed by the surrounding
//! terminal's `NOTMUX_SURFACE_ID`. `end` marks the session ended so it is not
//! auto-resumed. Both are silent no-ops outside a notmux terminal.

use notmux_terminal::agent_sessions::{self, AgentSessionRecord};
use std::io::Read;

/// Payload keys that may carry the session/conversation id.
const SESSION_ID_KEYS: &[&str] = &["session_id", "sessionId", "conversation_id", "conversationId"];
const TRANSCRIPT_KEYS: &[&str] = &["transcript_path", "transcriptPath", "rollout_path"];
const CWD_KEYS: &[&str] = &["cwd", "workspace_root", "working_directory"];

pub fn cli_agent_session(args: &[String]) -> i32 {
    let action = args.first().map(|s| s.as_str()).unwrap_or("");
    if !matches!(action, "record" | "end") {
        eprintln!("Usage: notmux agent-session <record|end> --kind <agent> [--session-id <id>] [--cwd <dir>] [--transcript-path <p>] [--pid <pid>]");
        return 1;
    }

    let mut kind = String::new();
    let mut session_id: Option<String> = None;
    let mut cwd: Option<String> = None;
    let mut transcript_path: Option<String> = None;
    let mut pid: Option<u32> = None;
    let mut i = 1;
    while i < args.len() {
        let take = |i: &mut usize| -> Option<String> {
            *i += 1;
            args.get(*i).cloned()
        };
        match args[i].as_str() {
            "--kind" => kind = take(&mut i).unwrap_or_default(),
            "--session-id" => session_id = take(&mut i).filter(|s| !s.trim().is_empty()),
            "--cwd" => cwd = take(&mut i).filter(|s| !s.trim().is_empty()),
            "--transcript-path" => {
                transcript_path = take(&mut i).filter(|s| !s.trim().is_empty())
            }
            "--pid" => pid = take(&mut i).and_then(|s| s.trim().parse().ok()),
            _ => {}
        }
        i += 1;
    }

    // Only meaningful inside a notmux terminal; silent no-op elsewhere so
    // agents run outside notmux never error in their hook chain. Records key
    // on the pane's layout slot id (`NOTMUX_SLOT_ID`): terminal ids are
    // cleared on load and regenerated every app start, so only the slot id
    // matches across restarts. The surface-id fallback covers terminals
    // spawned outside a layout slot.
    let non_empty = |var: &str| std::env::var(var).ok().filter(|s| !s.is_empty());
    let Some(surface_id) = non_empty("NOTMUX_SLOT_ID")
        .or_else(|| non_empty("NOTMUX_SURFACE_ID"))
        .or_else(|| non_empty("NOTMUX_TERMINAL_ID"))
    else {
        return 0;
    };
    if kind.trim().is_empty() {
        return 0;
    }

    if action == "end" {
        // Claude Code (≥2.1.x) fires SessionEnd on terminal disconnect too:
        // quitting the app tears down the PTY, claude gets SIGHUP and reports
        // reason "other". Only an intentional user exit may mark the record
        // ended — a disconnected session must stay restorable for auto-resume.
        if let Some(reason) = read_stdin_json().as_ref().and_then(end_reason)
            && !is_intentional_end_reason(&reason)
        {
            return 0;
        }
        if let Err(e) = agent_sessions::mark_ended(&surface_id, &kind) {
            log::warn!("agent-session end: {e}");
        }
        return 0;
    }

    // Merge stdin payload (if any) under explicit flags.
    if (session_id.is_none() || cwd.is_none() || transcript_path.is_none())
        && let Some(payload) = read_stdin_json()
    {
        session_id = session_id.or_else(|| first_string(&payload, SESSION_ID_KEYS));
        cwd = cwd.or_else(|| first_string(&payload, CWD_KEYS));
        transcript_path = transcript_path.or_else(|| first_string(&payload, TRANSCRIPT_KEYS));
    }

    let Some(session_id) = session_id.filter(|s| !s.trim().is_empty()) else {
        return 0;
    };

    let record = AgentSessionRecord {
        kind: kind.trim().to_string(),
        session_id: session_id.trim().to_string(),
        cwd,
        transcript_path,
        pid,
        ended: false,
        updated_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    };
    if let Err(e) = agent_sessions::record(&surface_id, record) {
        eprintln!("agent-session record: {e}");
        return 1;
    }
    0
}

/// The normalized `reason` of a SessionEnd hook payload, if present.
fn end_reason(payload: &serde_json::Value) -> Option<String> {
    payload
        .get("reason")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
}
/// SessionEnd reasons that represent an intentional user exit (claude's
/// documented reasons minus the "other" catch-all, which covers SIGHUP /
/// terminal disconnect). Payloads without a reason always mark ended, so
/// agents that don't send one keep their previous behavior.
fn is_intentional_end_reason(reason: &str) -> bool {
    matches!(reason, "clear" | "logout" | "prompt_input_exit" | "exit")
}
/// Read stdin to EOF and parse as JSON. Hooks pipe the payload; callers that
/// pass flags instead close stdin immediately, so this returns quickly. A
/// TTY stdin (manual CLI invocation) is skipped so the call never blocks.
fn read_stdin_json() -> Option<serde_json::Value> {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        return None;
    }
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).ok()?;
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed).ok()
}

/// First non-empty string among `keys`, checked at the top level and one
/// level deep in common nesting containers.
fn first_string(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    let object = value.as_object()?;
    for key in keys {
        if let Some(s) = object.get(*key).and_then(|v| v.as_str())
            && !s.trim().is_empty()
        {
            return Some(s.trim().to_string());
        }
    }
    for container in ["context", "payload", "session", "info", "properties"] {
        if let Some(nested) = object.get(container)
            && nested.is_object()
            && let Some(found) = first_string_flat(nested, keys)
        {
            return Some(found);
        }
    }
    None
}

fn first_string_flat(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    let object = value.as_object()?;
    for key in keys {
        if let Some(s) = object.get(*key).and_then(|v| v.as_str())
            && !s.trim().is_empty()
        {
            return Some(s.trim().to_string());
        }
    }
    // `info.id` (OpenCode session objects) — a bare `id` is only trusted
    // inside a nested session container, never at the payload top level.
    if keys == SESSION_ID_KEYS
        && let Some(s) = object.get("id").and_then(|v| v.as_str())
        && !s.trim().is_empty()
    {
        return Some(s.trim().to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_top_level_session_id() {
        let payload = serde_json::json!({
            "session_id": "abc", "transcript_path": "/t/p.jsonl", "cwd": "/w"
        });
        assert_eq!(first_string(&payload, SESSION_ID_KEYS).unwrap(), "abc");
        assert_eq!(first_string(&payload, TRANSCRIPT_KEYS).unwrap(), "/t/p.jsonl");
        assert_eq!(first_string(&payload, CWD_KEYS).unwrap(), "/w");
    }

    #[test]
    fn extracts_camel_case_and_conversation_id() {
        let payload = serde_json::json!({ "conversationId": "conv-1" });
        assert_eq!(first_string(&payload, SESSION_ID_KEYS).unwrap(), "conv-1");
    }

    #[test]
    fn extracts_nested_info_id() {
        let payload = serde_json::json!({ "info": { "id": "ses_123" } });
        assert_eq!(first_string(&payload, SESSION_ID_KEYS).unwrap(), "ses_123");
        // But a top-level bare `id` is not trusted.
        let payload = serde_json::json!({ "id": "nope" });
        assert!(first_string(&payload, SESSION_ID_KEYS).is_none());
    }

    #[test]
    fn empty_strings_are_skipped() {
        let payload = serde_json::json!({ "session_id": "  ", "conversation_id": "real" });
        assert_eq!(first_string(&payload, SESSION_ID_KEYS).unwrap(), "real");
    }
    #[test]
    fn end_reason_normalizes_payload() {
        let payload = serde_json::json!({ "reason": "  Other " });
        assert_eq!(end_reason(&payload).unwrap(), "other");
        assert!(end_reason(&serde_json::json!({ "reason": "" })).is_none());
        assert!(end_reason(&serde_json::json!({})).is_none());
    }
    #[test]
    fn disconnect_reasons_are_not_intentional_exits() {
        // "other" is what claude sends on SIGHUP when the app quits — the
        // session must stay restorable.
        assert!(!is_intentional_end_reason("other"));
        assert!(is_intentional_end_reason("clear"));
        assert!(is_intentional_end_reason("logout"));
        assert!(is_intentional_end_reason("prompt_input_exit"));
        assert!(is_intentional_end_reason("exit"));
    }
}
