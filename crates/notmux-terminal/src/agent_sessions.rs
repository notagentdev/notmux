//! Restorable agent session store + resume command builder.
//!
//! Lifecycle hooks installed into each
//! agent's config call `notmux agent-session record`, which persists
//! `{kind, session_id, cwd, …}` keyed by the terminal's surface id
//! (`NOTMUX_SURFACE_ID`). On workspace restore, a freshly respawned terminal
//! whose surface id has a restorable record gets the agent's resume command
//! typed into its shell (`claude --resume <id>`, `codex resume <id>`, …).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Records older than this are pruned on every store write.
const RECORD_TTL_SECS: u64 = 30 * 24 * 60 * 60;

/// One restorable agent session, keyed in the store by surface (terminal) id.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AgentSessionRecord {
    /// Agent kind: "claude", "codex", "notagent", "opencode", "pi",
    /// "cursor", "antigravity".
    pub kind: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    /// PID of the agent process that recorded the session (liveness evidence:
    /// if it is still alive on restore — e.g. a dtach/tmux reattach — we must
    /// not type a resume command into its terminal).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// True once the agent reported a real session end (user exited normally);
    /// ended sessions are not auto-resumed.
    #[serde(default)]
    pub ended: bool,
    /// Unix seconds of the last update.
    pub updated_at: u64,
}

/// `NOTMUX_CONFIG_DIR` override, else `$HOME/.config/notmux` — same resolution
/// as the shim/shell-init base dir in `pty_manager`.
fn config_dir() -> Option<PathBuf> {
    if let Ok(config) = std::env::var("NOTMUX_CONFIG_DIR")
        && !config.is_empty()
    {
        return Some(PathBuf::from(config));
    }
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE").filter(|h| !h.is_empty());
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty());
    home.map(|h| PathBuf::from(h).join(".config").join("notmux"))
}

pub fn store_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("agent-sessions.json"))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Load the full store. Missing or corrupt files yield an empty map.
pub fn load_store() -> HashMap<String, AgentSessionRecord> {
    let Some(path) = store_path() else {
        return HashMap::new();
    };
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Atomically persist the store (temp file + rename), pruning stale records.
fn save_store(mut store: HashMap<String, AgentSessionRecord>) -> Result<(), String> {
    let Some(path) = store_path() else {
        return Err("no config dir (HOME not set)".to_string());
    };
    let cutoff = now_secs().saturating_sub(RECORD_TTL_SECS);
    store.retain(|_, rec| rec.updated_at >= cutoff);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let json = serde_json::to_string_pretty(&store).map_err(|e| e.to_string())?;
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    std::fs::write(&tmp, json).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename {}: {e}", path.display()))
}

/// Insert/update the record for a surface id.
pub fn record(surface_id: &str, rec: AgentSessionRecord) -> Result<(), String> {
    let mut store = load_store();
    store.insert(surface_id.to_string(), rec);
    save_store(store)
}

/// Mark the session on a surface as ended (agent exited normally). A kind
/// mismatch is ignored so a stale hook from another agent can't clear a newer
/// record.
pub fn mark_ended(surface_id: &str, kind: &str) -> Result<(), String> {
    let mut store = load_store();
    if let Some(rec) = store.get_mut(surface_id) {
        if rec.kind != kind {
            return Ok(());
        }
        rec.ended = true;
        rec.updated_at = now_secs();
        return save_store(store);
    }
    Ok(())
}

/// Look up the record for a surface id.
pub fn get(surface_id: &str) -> Option<AgentSessionRecord> {
    load_store().remove(surface_id)
}

/// Single-quote a value as one POSIX shell word.
fn shell_single_quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// The bare resume argv for an agent kind (executable + args), without cwd
/// handling. Returns None for unknown kinds.
pub fn resume_argv(kind: &str, session_id: &str) -> Option<Vec<String>> {
    let id = session_id.trim();
    if id.is_empty() {
        return None;
    }
    let argv: Vec<&str> = match kind {
        "claude" => vec!["claude", "--resume", id],
        "codex" => vec!["codex", "resume", id],
        "notagent" => vec!["notagent", "--cid", id],
        "opencode" => vec!["opencode", "--session", id],
        "pi" => vec!["pi", "--session", id],
        "cursor" => vec!["cursor-agent", "--resume", id],
        "antigravity" => vec!["agy", "--conversation", id],
        _ => return None,
    };
    Some(argv.into_iter().map(str::to_string).collect())
}

/// Build the full shell command to resume a recorded session, including the
/// cd-guard prefix when the record carries a cwd. The prefix is a plain
/// AND-OR list (no `{ …; }` grouping) so it parses in sh/bash/zsh/fish alike
/// (fish has no brace grouping):
/// `cd -- '<dir>' 2>/dev/null || [ ! -d '<dir>' ] && <agent cmd>`
/// runs the agent after a successful cd, still runs it (in the current dir)
/// when the dir is gone, and does nothing when the dir exists but cd failed.
pub fn resume_shell_command(rec: &AgentSessionRecord) -> Option<String> {
    if cfg!(windows) {
        // The command below is POSIX; auto-resume is Unix-only for now.
        return None;
    }
    let argv = resume_argv(&rec.kind, &rec.session_id)?;
    let cmd = argv
        .iter()
        .map(|part| shell_single_quoted(part))
        .collect::<Vec<_>>()
        .join(" ");
    match rec.cwd.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
        Some(cwd) => {
            let quoted = shell_single_quoted(cwd);
            Some(format!(
                "cd -- {quoted} 2>/dev/null || [ ! -d {quoted} ] && {cmd}"
            ))
        }
        None => Some(cmd),
    }
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    // kill(pid, 0): 0 or EPERM ⇒ the process exists.
    let rc = unsafe { libc::kill(pid as i32, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    false
}

/// Whether a record is worth resuming: not ended, agent process no longer
/// alive (a live PID means the session survived — e.g. dtach/tmux reattach),
/// and for claude the transcript file must still exist (claude sessions
/// without their transcript JSONL cannot be resumed).
pub fn is_restorable(rec: &AgentSessionRecord) -> bool {
    if rec.ended {
        return false;
    }
    if rec.session_id.trim().is_empty() {
        return false;
    }
    if let Some(pid) = rec.pid
        && pid_alive(pid)
    {
        return false;
    }
    if rec.kind == "claude"
        && let Some(transcript) = rec.transcript_path.as_deref().map(str::trim)
        && !transcript.is_empty()
    {
        let expanded = expand_tilde(transcript);
        if !std::path::Path::new(&expanded).is_file() {
            return false;
        }
    }
    true
}

fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest).to_string_lossy().to_string();
    }
    path.to_string()
}

/// Surface ids that already received a resume injection this app run —
/// a pane re-created later in the same run must not re-type the command.
static RESUMED_SURFACES: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    std::sync::OnceLock::new();

/// One-shot: the resume input (command + carriage return) to type into a
/// freshly respawned terminal, or None when there is nothing restorable.
/// Marks the surface as handled so a second call returns None.
pub fn take_resume_input(surface_id: &str) -> Option<String> {
    let resumed = RESUMED_SURFACES.get_or_init(Default::default);
    {
        let mut seen = resumed.lock().unwrap_or_else(|e| e.into_inner());
        if !seen.insert(surface_id.to_string()) {
            return None;
        }
    }
    let rec = get(surface_id)?;
    if !is_restorable(&rec) {
        return None;
    }
    let cmd = resume_shell_command(&rec)?;
    log::info!(
        "Auto-resuming {} session {} in terminal {}",
        rec.kind,
        rec.session_id,
        surface_id
    );
    Some(format!("{cmd}\r"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(kind: &str, session_id: &str, cwd: Option<&str>) -> AgentSessionRecord {
        AgentSessionRecord {
            kind: kind.to_string(),
            session_id: session_id.to_string(),
            cwd: cwd.map(str::to_string),
            transcript_path: None,
            pid: None,
            ended: false,
            updated_at: now_secs(),
        }
    }

    #[test]
    fn resume_argv_per_kind() {
        assert_eq!(
            resume_argv("claude", "abc").unwrap(),
            vec!["claude", "--resume", "abc"]
        );
        assert_eq!(
            resume_argv("codex", "abc").unwrap(),
            vec!["codex", "resume", "abc"]
        );
        assert_eq!(
            resume_argv("notagent", "abc").unwrap(),
            vec!["notagent", "--cid", "abc"]
        );
        assert_eq!(
            resume_argv("opencode", "abc").unwrap(),
            vec!["opencode", "--session", "abc"]
        );
        assert_eq!(resume_argv("pi", "abc").unwrap(), vec!["pi", "--session", "abc"]);
        assert_eq!(
            resume_argv("cursor", "abc").unwrap(),
            vec!["cursor-agent", "--resume", "abc"]
        );
        assert_eq!(
            resume_argv("antigravity", "abc").unwrap(),
            vec!["agy", "--conversation", "abc"]
        );
        assert!(resume_argv("unknown", "abc").is_none());
        assert!(resume_argv("claude", "  ").is_none());
    }

    #[test]
    #[cfg(unix)]
    fn resume_shell_command_with_cd_guard() {
        let record = rec("claude", "abc-123", Some("/tmp/my project"));
        let cmd = resume_shell_command(&record).unwrap();
        assert_eq!(
            cmd,
            "cd -- '/tmp/my project' 2>/dev/null || [ ! -d '/tmp/my project' ] && 'claude' '--resume' 'abc-123'"
        );
    }

    #[test]
    #[cfg(unix)]
    fn resume_shell_command_without_cwd() {
        let record = rec("codex", "abc", None);
        assert_eq!(resume_shell_command(&record).unwrap(), "'codex' 'resume' 'abc'");
        // Whitespace-only cwd is treated as absent.
        let record = rec("codex", "abc", Some("  "));
        assert_eq!(resume_shell_command(&record).unwrap(), "'codex' 'resume' 'abc'");
    }

    #[test]
    fn shell_quoting_escapes_single_quotes() {
        assert_eq!(shell_single_quoted("it's"), "'it'\\''s'");
        assert_eq!(shell_single_quoted("plain"), "'plain'");
    }

    #[test]
    fn restorable_rules() {
        // Basic record is restorable.
        assert!(is_restorable(&rec("codex", "abc", None)));
        // Ended is not.
        let mut r = rec("codex", "abc", None);
        r.ended = true;
        assert!(!is_restorable(&r));
        // Empty session id is not.
        assert!(!is_restorable(&rec("codex", " ", None)));
        // Live pid (our own) is not.
        let mut r = rec("codex", "abc", None);
        r.pid = Some(std::process::id());
        assert!(!is_restorable(&r));
        // Dead pid is fine — pid 1 exists but is not ours… use an unlikely-alive pid.
        let mut r = rec("codex", "abc", None);
        r.pid = Some(0);
        assert!(is_restorable(&r));
        // Claude with a missing transcript is not restorable.
        let mut r = rec("claude", "abc", None);
        r.transcript_path = Some("/nonexistent/path/session.jsonl".to_string());
        assert!(!is_restorable(&r));
        // Claude without transcript info stays restorable.
        assert!(is_restorable(&rec("claude", "abc", None)));
    }

    #[test]
    fn store_round_trip_and_prune() {
        let dir = std::env::temp_dir().join(format!(
            "notmux-agent-sessions-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        // Scope the env override; tests in this binary run in threads, so use
        // a unique dir and restore afterwards.
        let prev = std::env::var("NOTMUX_CONFIG_DIR").ok();
        unsafe { std::env::set_var("NOTMUX_CONFIG_DIR", &dir) };

        record("t1", rec("claude", "s1", Some("/tmp"))).unwrap();
        let mut stale = rec("codex", "s2", None);
        stale.updated_at = 1; // far in the past → pruned on next save
        record("t2", stale).unwrap();
        // The save that wrote t2 prunes on write, but t2 itself was just
        // inserted with updated_at=1, so the *next* write drops it.
        record("t3", rec("pi", "s3", None)).unwrap();

        let store = load_store();
        assert_eq!(store.get("t1").unwrap().session_id, "s1");
        assert!(store.get("t2").is_none(), "stale record should be pruned");
        assert!(store.get("t3").is_some());

        mark_ended("t1", "claude").unwrap();
        assert!(load_store().get("t1").unwrap().ended);
        // Kind mismatch leaves the record alone.
        mark_ended("t3", "claude").unwrap();
        assert!(!load_store().get("t3").unwrap().ended);

        match prev {
            Some(v) => unsafe { std::env::set_var("NOTMUX_CONFIG_DIR", v) },
            None => unsafe { std::env::remove_var("NOTMUX_CONFIG_DIR") },
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
