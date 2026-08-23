//! Restorable agent session store + resume command builder.
//!
//! Lifecycle hooks installed into each agent's config call
//! `notmux agent-session record|confirm|end`, which persist session state
//! keyed by the terminal's layout slot (`NOTMUX_SLOT_ID`). On workspace
//! restore, a freshly respawned terminal whose slot has a restorable record
//! gets the agent's resume command typed into its shell.
//!
//! Evidence model — a session *earns* restorability. A
//! session start writes a `pending` record; the first completed turn
//! promotes it to `confirmed`. Restorability is recomputed from evidence at
//! restore time (claude: the transcript file must exist) — never from a
//! persisted quit-time activity bit. A missed auto-resume therefore leaves
//! the record restorable (carry-forward); only an agent that verifiably ran
//! and exited during the current app run (`exited_this_run`, stamped at
//! quit from the agent process's own pid) is excluded, so intentionally
//! closed agents don't resurrect.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Records older than this are pruned on every store write.
const RECORD_TTL_SECS: u64 = 30 * 24 * 60 * 60;

/// One agent session, stored in a slot's `pending` or `confirmed` place.
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
    /// True once the agent reported an intentional session end (user exited);
    /// ended sessions are not auto-resumed.
    #[serde(default)]
    pub ended: bool,
    /// Stamped at app quit when this agent ran during that app run and its
    /// process is gone (the user exited it before quitting). Cleared by the
    /// next record/confirm for the slot. Unlike the evidence checks this is
    /// per-run state, not a permanent kill bit: records untouched during a
    /// run keep their previous value (carry-forward).
    #[serde(default)]
    pub exited_this_run: bool,
    /// Sanitized launch arguments captured from the agent process's argv
    /// (see `agent_launch`); appended to the resume command so model/profile
    /// flags survive the resume.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub launch_args: Vec<String>,
    /// Unix seconds of the last update. Also serves as "ran during this app
    /// run" evidence for the quit-time exited check.
    pub updated_at: u64,
}

/// A layout slot's session state: the last session with turn evidence
/// (`confirmed`) and a freshly started one that has not completed a turn yet
/// (`pending`). Keeping both is what makes claude's resume chain safe: a
/// `claude --resume` mints a new session id at launch, and if that process
/// dies before its first turn the new id has no transcript — the previous
/// confirmed session must survive it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct AgentSlotRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmed: Option<AgentSessionRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<AgentSessionRecord>,
}

#[derive(Serialize, Deserialize)]
struct StoreFile {
    version: u32,
    slots: HashMap<String, AgentSlotRecord>,
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

/// Load the full store, migrating a v1 (flat map) file in place. Missing or
/// corrupt files yield an empty map.
pub fn load_store() -> HashMap<String, AgentSlotRecord> {
    let Some(path) = store_path() else {
        return HashMap::new();
    };
    let Ok(content) = std::fs::read_to_string(&path) else {
        return HashMap::new();
    };
    if let Ok(file) = serde_json::from_str::<StoreFile>(&content)
        && file.version >= 2
    {
        return file.slots;
    }
    // v1: flat map of slot id → record. `was_running_at_quit` (the removed
    // kill bit) is dropped by deserialization; the record becomes the slot's
    // confirmed session. Claude records whose recorded transcript no longer
    // exists are repaired to their sibling transcript when possible and
    // dropped otherwise — they point at a session that was never written.
    let Ok(v1) = serde_json::from_str::<HashMap<String, AgentSessionRecord>>(&content) else {
        return HashMap::new();
    };
    v1.into_iter()
        .filter_map(|(slot, rec)| {
            let rec = if rec.kind == "claude" {
                resolve_claude_record(rec)?
            } else {
                rec
            };
            Some((
                slot,
                AgentSlotRecord {
                    confirmed: Some(rec),
                    pending: None,
                },
            ))
        })
        .collect()
}

/// Atomically persist the store (temp file + rename), pruning stale records.
fn save_store(mut store: HashMap<String, AgentSlotRecord>) -> Result<(), String> {
    let Some(path) = store_path() else {
        return Err("no config dir (HOME not set)".to_string());
    };
    let cutoff = now_secs().saturating_sub(RECORD_TTL_SECS);
    for slot in store.values_mut() {
        slot.confirmed.take_if(|rec| rec.updated_at < cutoff);
        slot.pending.take_if(|rec| rec.updated_at < cutoff);
    }
    store.retain(|_, slot| slot.confirmed.is_some() || slot.pending.is_some());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let file = StoreFile { version: 2, slots: store };
    let json = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    std::fs::write(&tmp, json).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename {}: {e}", path.display()))
}

/// Record a session start: writes the slot's `pending` place. The previous
/// confirmed session is never overwritten by a start — only a completed turn
/// (`confirm`) replaces it. A start for the already-confirmed session id
/// (reattach) refreshes the confirmed record in place.
pub fn record(surface_id: &str, mut rec: AgentSessionRecord) -> Result<(), String> {
    let mut store = load_store();
    rec.exited_this_run = false;
    let slot = store.entry(surface_id.to_string()).or_default();
    match &mut slot.confirmed {
        Some(confirmed)
            if confirmed.kind == rec.kind && confirmed.session_id == rec.session_id =>
        {
            *confirmed = rec;
            slot.pending = None;
        }
        _ => slot.pending = Some(rec),
    }
    save_store(store)
}

/// Record turn evidence: promotes the slot's `pending` session to
/// `confirmed` (replacing the previous confirmed session). Without a pending
/// record, a matching confirmed record is refreshed; agents that only report
/// on turn completion (cursor, opencode, pi) create the confirmed record
/// directly.
pub fn confirm(surface_id: &str, mut rec: AgentSessionRecord) -> Result<(), String> {
    let mut store = load_store();
    rec.exited_this_run = false;
    let slot = store.entry(surface_id.to_string()).or_default();

    let id_matches = |other: &AgentSessionRecord| {
        other.kind == rec.kind
            && (rec.session_id.trim().is_empty() || other.session_id == rec.session_id)
    };

    if slot.pending.as_ref().is_some_and(id_matches) {
        let mut promoted = slot.pending.take().unwrap();
        merge_confirm_fields(&mut promoted, rec);
        slot.confirmed = Some(promoted);
    } else if slot.confirmed.as_ref().is_some_and(id_matches) {
        let mut confirmed = slot.confirmed.take().unwrap();
        merge_confirm_fields(&mut confirmed, rec);
        slot.confirmed = Some(confirmed);
    } else if !rec.session_id.trim().is_empty() {
        slot.confirmed = Some(rec);
    } else {
        return Ok(());
    }
    save_store(store)
}

/// Fold the fields of a confirm payload into an existing record without
/// erasing richer earlier data with an emptier late payload.
fn merge_confirm_fields(target: &mut AgentSessionRecord, incoming: AgentSessionRecord) {
    if !incoming.session_id.trim().is_empty() {
        target.session_id = incoming.session_id;
    }
    if incoming.cwd.is_some() {
        target.cwd = incoming.cwd;
    }
    if incoming.transcript_path.is_some() {
        target.transcript_path = incoming.transcript_path;
    }
    if incoming.pid.is_some() {
        target.pid = incoming.pid;
    }
    if !incoming.launch_args.is_empty() {
        target.launch_args = incoming.launch_args;
    }
    target.ended = false;
    target.exited_this_run = false;
    target.updated_at = incoming.updated_at.max(now_secs());
}

/// Mark the sessions on a surface as intentionally ended (user exited the
/// agent). A kind mismatch is ignored so a stale hook from another agent
/// can't clear a newer record.
pub fn mark_ended(surface_id: &str, kind: &str) -> Result<(), String> {
    let mut store = load_store();
    let Some(slot) = store.get_mut(surface_id) else {
        return Ok(());
    };
    let mut changed = false;
    for rec in [slot.pending.as_mut(), slot.confirmed.as_mut()].into_iter().flatten() {
        if rec.kind == kind {
            rec.ended = true;
            rec.updated_at = now_secs();
            changed = true;
        }
    }
    if changed { save_store(store) } else { Ok(()) }
}

static APP_START_EPOCH: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

/// Record the app process's start time (call once early in `main`). The
/// quit-time exited check compares record freshness against it.
pub fn init_app_start_epoch() {
    let _ = APP_START_EPOCH.set(now_secs());
}

/// The recorded app start time; falls back to "now" when init was skipped
/// (which disables exited-marking rather than poisoning carried records).
pub fn app_start_epoch() -> u64 {
    *APP_START_EPOCH.get_or_init(now_secs)
}

/// Quit-time pass over the whole store (replaces the removed
/// `was_running_at_quit` shell-child heuristic): a record whose agent ran
/// during this app run (`updated_at >= app_start_epoch`) and whose process is
/// gone was exited by the user — it must not resurrect on the next start.
/// Records untouched during this run keep their previous state
/// (carry-forward), so a missed auto-resume never poisons a session.
pub fn mark_quit_states(app_start_epoch: u64) -> Result<(), String> {
    let mut store = load_store();
    let mut changed = false;
    for slot in store.values_mut() {
        for rec in [slot.pending.as_mut(), slot.confirmed.as_mut()].into_iter().flatten() {
            if rec.updated_at < app_start_epoch {
                continue;
            }
            let exited = !rec.pid.is_some_and(pid_alive);
            if rec.exited_this_run != exited {
                rec.exited_this_run = exited;
                changed = true;
            }
        }
    }
    if changed { save_store(store) } else { Ok(()) }
}

/// Look up a slot's stored state.
pub fn get_slot(surface_id: &str) -> Option<AgentSlotRecord> {
    load_store().remove(surface_id)
}

/// Single-quote a value as one POSIX shell word.
fn shell_single_quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// The bare resume argv for an agent kind (executable + args), without cwd
/// handling or preserved launch args. Executables stay bare names on purpose:
/// `claude`/`codex` must resolve through the notmux PATH shims so hooks are
/// re-injected on the resumed session — an absolute path would bypass them.
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

/// The full resume argv for a record: base command, codex's update-check
/// suppression, then the preserved launch args.
fn resume_argv_for(rec: &AgentSessionRecord) -> Option<Vec<String>> {
    let mut argv = resume_argv(&rec.kind, &rec.session_id)?;
    if rec.kind == "codex" {
        argv.extend(crate::agent_launch::codex_resume_config_overrides(&rec.launch_args));
    }
    argv.extend(rec.launch_args.iter().cloned());
    Some(argv)
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
    let argv = resume_argv_for(rec)?;
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

/// Whether a record may be auto-resumed, from persisted state and file
/// evidence. Recomputed at restore time; deliberately excludes the pid
/// liveness gate (a live agent defers the resume but must not consume or
/// invalidate it — see `take_resume_input`).
pub fn is_restorable(rec: &AgentSessionRecord) -> bool {
    if rec.ended || rec.exited_this_run {
        return false;
    }
    if rec.session_id.trim().is_empty() {
        return false;
    }
    if rec.kind == "claude" && !claude_transcript_exists(rec) {
        return false;
    }
    true
}

fn claude_transcript_exists(rec: &AgentSessionRecord) -> bool {
    match rec.transcript_path.as_deref().map(str::trim) {
        Some(transcript) if !transcript.is_empty() => {
            non_empty_file(Path::new(&expand_tilde(transcript)))
        }
        // Without transcript info, keep the legacy benefit of the doubt.
        _ => true,
    }
}

fn non_empty_file(path: &Path) -> bool {
    std::fs::metadata(path).map(|m| m.is_file() && m.len() > 0).unwrap_or(false)
}

fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest).to_string_lossy().to_string();
    }
    path.to_string()
}

// ── Claude sibling-transcript repair ───────────────────────────────────────

/// Claude's project-directory encoding: `/` and `.` both become `-`.
fn encode_claude_project_dir(path: &str) -> String {
    path.replace(['/', '.'], "-")
}

fn claude_config_root() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR")
        && !dir.is_empty()
    {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude"))
}

/// Resolve a claude record whose transcript is missing to the single sibling
/// transcript of its session, or `None` when it cannot be repaired. Anchored
/// on the container directory: a candidate project dir must contain the dead
/// session's workflow container directory (`<project>/<session-id>/`), and the repair
/// only fires when exactly one other non-empty transcript exists there —
/// never guess among many.
pub fn resolve_claude_record(rec: AgentSessionRecord) -> Option<AgentSessionRecord> {
    if claude_transcript_exists(&rec) {
        return Some(rec);
    }
    let session_id = rec.session_id.trim();
    if session_id.is_empty() || !claude_session_id_is_safe(session_id) {
        return None;
    }

    let mut candidate_dirs: Vec<PathBuf> = Vec::new();
    if let Some(transcript) = rec.transcript_path.as_deref().map(str::trim)
        && !transcript.is_empty()
        && let Some(parent) = Path::new(&expand_tilde(transcript)).parent()
    {
        candidate_dirs.push(parent.to_path_buf());
    }
    if let Some(cwd) = rec.cwd.as_deref().map(str::trim).filter(|c| !c.is_empty())
        && let Some(root) = claude_config_root()
    {
        candidate_dirs.push(root.join("projects").join(encode_claude_project_dir(cwd)));
    }
    candidate_dirs.dedup();

    let mut matches: Vec<(String, PathBuf)> = Vec::new();
    for dir in &candidate_dirs {
        if !dir.join(session_id).is_dir() {
            // No workflow container for the dead session — not anchored here.
            continue;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if stem == session_id || !claude_session_id_is_safe(stem) {
                continue;
            }
            if !non_empty_file(&path) {
                continue;
            }
            if !matches.iter().any(|(existing, _)| existing == stem) {
                matches.push((stem.to_string(), path));
            }
            if matches.len() >= 2 {
                return None;
            }
        }
        if !matches.is_empty() {
            break;
        }
    }
    let (sibling_id, sibling_path) = matches.pop()?;
    let mut repaired = rec;
    log::info!(
        "Repaired claude session {} → sibling {} ({})",
        repaired.session_id,
        sibling_id,
        sibling_path.display()
    );
    repaired.session_id = sibling_id;
    repaired.transcript_path = Some(sibling_path.to_string_lossy().to_string());
    Some(repaired)
}

fn claude_session_id_is_safe(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Surface ids that already received a resume injection this app run —
/// a pane re-created later in the same run must not re-type the command.
static RESUMED_SURFACES: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    std::sync::OnceLock::new();

fn resume_input(surface_id: &str, ignore_consumed: bool) -> Option<String> {
    let resumed = RESUMED_SURFACES.get_or_init(Default::default);
    {
        let seen = resumed.lock().unwrap_or_else(|e| e.into_inner());
        if !ignore_consumed && seen.contains(surface_id) {
            return None;
        }
    }

    let mut store = load_store();
    let slot = store.get_mut(surface_id)?;

    // Resolve claude evidence in place: a repair rewrites the record, an
    // unrepairable record (transcript never written) is discarded so the
    // other place can take over. Returns the restorable candidate, if any.
    let mut dirty = false;
    let mut resolve_place =
        |place: &mut Option<AgentSessionRecord>| -> Option<AgentSessionRecord> {
            let original = place.take()?;
            let resolved = if original.kind == "claude" {
                resolve_claude_record(original.clone())
            } else {
                Some(original.clone())
            };
            match resolved {
                Some(rec) => {
                    if rec != original {
                        dirty = true;
                    }
                    *place = Some(rec.clone());
                    is_restorable(&rec).then_some(rec)
                }
                None => {
                    dirty = true;
                    None
                }
            }
        };

    // Prefer the pending session when it has evidence; the dead-on-arrival
    // claude resume (new id, no transcript) falls back to confirmed.
    let candidate = resolve_place(&mut slot.pending)
        .or_else(|| resolve_place(&mut slot.confirmed));
    if dirty {
        let _ = save_store(store);
    }

    let rec = candidate?;
    // A live agent process means the session survived (e.g. a dtach/tmux
    // reattach) — don't type into it, and don't consume the attempt either:
    // the record stays available for a later manual resume.
    if rec.pid.is_some_and(pid_alive) {
        return None;
    }
    let cmd = resume_shell_command(&rec)?;
    {
        let mut seen = resumed.lock().unwrap_or_else(|e| e.into_inner());
        seen.insert(surface_id.to_string());
    }
    log::info!(
        "Auto-resuming {} session {} in terminal {}",
        rec.kind,
        rec.session_id,
        surface_id
    );
    Some(format!("{cmd}\r"))
}

/// One-shot: the resume input (command + carriage return) to type into a
/// freshly respawned terminal, or None when there is nothing restorable.
/// The surface is only marked as handled once a command was actually
/// produced — a resume blocked by a still-live agent process stays available.
pub fn take_resume_input(surface_id: &str) -> Option<String> {
    resume_input(surface_id, false)
}

/// Manual variant (command palette / context menu): ignores the one-shot
/// marker so the user can re-resume after exiting an agent, but still
/// requires evidence and a dead agent process.
pub fn manual_resume_input(surface_id: &str) -> Option<String> {
    resume_input(surface_id, true)
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
            exited_this_run: false,
            launch_args: Vec::new(),
            updated_at: now_secs(),
        }
    }

    /// Serialize store access: tests in this binary run in threads and the
    /// store path comes from the NOTMUX_CONFIG_DIR env var.
    fn with_store(test: impl FnOnce()) {
        let _guard = crate::test_env_lock();
        let dir = std::env::temp_dir().join(format!(
            "notmux-agent-sessions-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let prev = std::env::var("NOTMUX_CONFIG_DIR").ok();
        unsafe { std::env::set_var("NOTMUX_CONFIG_DIR", &dir) };
        test();
        match prev {
            Some(v) => unsafe { std::env::set_var("NOTMUX_CONFIG_DIR", v) },
            None => unsafe { std::env::remove_var("NOTMUX_CONFIG_DIR") },
        }
        let _ = std::fs::remove_dir_all(&dir);
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
    fn resume_shell_command_with_cd_guard_and_launch_args() {
        let mut record = rec("claude", "abc-123", Some("/tmp/my project"));
        record.launch_args = vec!["--model".into(), "opus".into()];
        let cmd = resume_shell_command(&record).unwrap();
        assert_eq!(
            cmd,
            "cd -- '/tmp/my project' 2>/dev/null || [ ! -d '/tmp/my project' ] && 'claude' '--resume' 'abc-123' '--model' 'opus'"
        );
    }

    #[test]
    #[cfg(unix)]
    fn codex_resume_gets_update_check_suppression() {
        let record = rec("codex", "abc", None);
        assert_eq!(
            resume_shell_command(&record).unwrap(),
            "'codex' 'resume' 'abc' '-c' 'check_for_update_on_startup=false'"
        );
        // An explicit user override is left authoritative.
        let mut record = rec("codex", "abc", None);
        record.launch_args =
            vec!["-c".into(), "check_for_update_on_startup=true".into()];
        assert_eq!(
            resume_shell_command(&record).unwrap(),
            "'codex' 'resume' 'abc' '-c' 'check_for_update_on_startup=true'"
        );
    }

    #[test]
    fn restorable_rules() {
        assert!(is_restorable(&rec("codex", "abc", None)));
        let mut r = rec("codex", "abc", None);
        r.ended = true;
        assert!(!is_restorable(&r));
        let mut r = rec("codex", "abc", None);
        r.exited_this_run = true;
        assert!(!is_restorable(&r));
        assert!(!is_restorable(&rec("codex", " ", None)));
        // Claude with a missing transcript is not restorable.
        let mut r = rec("claude", "abc", None);
        r.transcript_path = Some("/nonexistent/path/session.jsonl".to_string());
        assert!(!is_restorable(&r));
        // Claude without transcript info stays restorable.
        assert!(is_restorable(&rec("claude", "abc", None)));
    }

    #[test]
    fn record_keeps_confirmed_and_confirm_promotes() {
        with_store(|| {
            // First session start → pending only.
            record("slot", rec("claude", "s1", Some("/w"))).unwrap();
            let slot = get_slot("slot").unwrap();
            assert_eq!(slot.pending.as_ref().unwrap().session_id, "s1");
            assert!(slot.confirmed.is_none());

            // Turn completed → promoted to confirmed.
            confirm("slot", rec("claude", "s1", None)).unwrap();
            let slot = get_slot("slot").unwrap();
            assert!(slot.pending.is_none());
            let confirmed = slot.confirmed.unwrap();
            assert_eq!(confirmed.session_id, "s1");
            assert_eq!(confirmed.cwd.as_deref(), Some("/w"), "confirm must not erase cwd");

            // A new session start (resume mints s2) must NOT evict s1.
            record("slot", rec("claude", "s2", Some("/w"))).unwrap();
            let slot = get_slot("slot").unwrap();
            assert_eq!(slot.confirmed.as_ref().unwrap().session_id, "s1");
            assert_eq!(slot.pending.as_ref().unwrap().session_id, "s2");

            // s2 completes a turn → replaces s1.
            confirm("slot", rec("claude", "s2", None)).unwrap();
            let slot = get_slot("slot").unwrap();
            assert_eq!(slot.confirmed.as_ref().unwrap().session_id, "s2");
            assert!(slot.pending.is_none());
        });
    }

    #[test]
    fn confirm_without_prior_record_creates_confirmed() {
        with_store(|| {
            // cursor/opencode/pi report only on turn completion.
            confirm("slot", rec("cursor", "conv-1", Some("/w"))).unwrap();
            let slot = get_slot("slot").unwrap();
            assert_eq!(slot.confirmed.as_ref().unwrap().session_id, "conv-1");
            // Kind mismatch with empty id is a no-op.
            confirm("slot", rec("codex", "", None)).unwrap();
            let slot = get_slot("slot").unwrap();
            assert_eq!(slot.confirmed.as_ref().unwrap().kind, "cursor");
        });
    }

    #[test]
    fn mark_ended_hits_both_places_and_respects_kind() {
        with_store(|| {
            record("slot", rec("claude", "s1", None)).unwrap();
            confirm("slot", rec("claude", "s1", None)).unwrap();
            record("slot", rec("claude", "s2", None)).unwrap();
            mark_ended("slot", "codex").unwrap();
            let slot = get_slot("slot").unwrap();
            assert!(!slot.confirmed.as_ref().unwrap().ended);
            assert!(!slot.pending.as_ref().unwrap().ended);
            mark_ended("slot", "claude").unwrap();
            let slot = get_slot("slot").unwrap();
            assert!(slot.confirmed.as_ref().unwrap().ended);
            assert!(slot.pending.as_ref().unwrap().ended);
        });
    }

    #[test]
    fn quit_states_only_touch_records_from_this_run() {
        with_store(|| {
            let app_start = now_secs() - 50;
            // Ran this run (updated after app start), process dead → exited.
            record("ran", rec("codex", "s1", None)).unwrap();
            // Carried from a previous run (updated before app start, within
            // the TTL) → untouched, even with a dead pid.
            let mut carried = rec("codex", "s2", None);
            carried.updated_at = app_start - 100;
            carried.pid = Some(0);
            record("carried", carried).unwrap();
            // Still alive through quit → not exited.
            let mut live = rec("notagent", "s3", None);
            live.pid = Some(std::process::id());
            record("live", live).unwrap();
            let mut store = load_store();
            store.get_mut("carried").unwrap().pending.as_mut().unwrap().updated_at =
                app_start - 100;
            save_store(store).unwrap();

            mark_quit_states(app_start).unwrap();
            assert!(load_store()["ran"].pending.as_ref().unwrap().exited_this_run);
            assert!(!load_store()["carried"].pending.as_ref().unwrap().exited_this_run);
            assert!(!load_store()["live"].pending.as_ref().unwrap().exited_this_run);

            // The next session start clears the flag.
            record("ran", rec("codex", "s1b", None)).unwrap();
            assert!(!load_store()["ran"].pending.as_ref().unwrap().exited_this_run);
        });
    }

    #[test]
    fn take_resume_prefers_confirmed_over_dead_pending_claude() {
        with_store(|| {
            let dir = std::env::temp_dir().join(format!(
                "notmux-claude-evidence-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let transcript = dir.join("s1.jsonl");
            std::fs::write(&transcript, "line\n").unwrap();

            let mut confirmed = rec("claude", "s1", Some("/w"));
            confirmed.transcript_path = Some(transcript.to_string_lossy().to_string());
            record("slot", confirmed.clone()).unwrap();
            confirm("slot", confirmed).unwrap();

            // The dead-on-arrival resume: new session id, transcript never
            // written, no workflow container → unrepairable pending.
            let mut dead = rec("claude", "s2", Some("/w"));
            dead.transcript_path =
                Some(dir.join("s2.jsonl").to_string_lossy().to_string());
            record("slot", dead).unwrap();

            let input = take_resume_input("slot").unwrap();
            assert!(input.contains("'--resume' 's1'"), "got: {input}");
            // The dead pending record was discarded.
            let slot = get_slot("slot").unwrap();
            assert!(slot.pending.is_none());
            // One-shot: second call yields nothing…
            assert!(take_resume_input("slot").is_none());
            // …but the manual variant still works.
            assert!(manual_resume_input("slot").is_some());
            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    #[test]
    fn blocked_resume_is_not_consumed() {
        with_store(|| {
            let mut live = rec("codex", "s1", None);
            live.pid = Some(std::process::id()); // our own pid: alive
            record("slot-live", live).unwrap();
            // The confirm payload carries no pid; the live pid is kept.
            confirm("slot-live", rec("codex", "s1", None)).unwrap();
            assert!(take_resume_input("slot-live").is_none());
            // Not consumed: once the pid dies the resume still fires. Simulate
            // by clearing the pid.
            let mut store = load_store();
            store.get_mut("slot-live").unwrap().confirmed.as_mut().unwrap().pid = Some(0);
            save_store(store).unwrap();
            assert!(take_resume_input("slot-live").is_some());
        });
    }

    #[test]
    fn v1_store_migrates_to_confirmed() {
        with_store(|| {
            let dir = std::env::var("NOTMUX_CONFIG_DIR").unwrap();
            std::fs::create_dir_all(&dir).unwrap();
            let v1 = serde_json::json!({
                "slot-a": {
                    "kind": "codex",
                    "session_id": "old-1",
                    "cwd": "/w",
                    "pid": 4242,
                    "ended": false,
                    "was_running_at_quit": false,
                    "updated_at": now_secs()
                },
                "slot-b": {
                    "kind": "claude",
                    "session_id": "dead",
                    "transcript_path": "/nonexistent/dead.jsonl",
                    "ended": false,
                    "updated_at": now_secs()
                }
            });
            std::fs::write(
                std::path::Path::new(&dir).join("agent-sessions.json"),
                serde_json::to_string(&v1).unwrap(),
            )
            .unwrap();

            let store = load_store();
            // The kill bit is gone: the codex record is restorable again.
            let migrated = store["slot-a"].confirmed.as_ref().unwrap();
            assert_eq!(migrated.session_id, "old-1");
            assert!(is_restorable(migrated));
            // The transcript-less claude record is unrepairable → dropped.
            assert!(!store.contains_key("slot-b"));
        });
    }

    #[test]
    fn claude_sibling_repair_with_container_anchor() {
        with_store(|| {
            let dir = std::env::temp_dir().join(format!(
                "notmux-claude-repair-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            // Workflow container for the dead session + exactly one sibling.
            std::fs::create_dir_all(dir.join("dead-id")).unwrap();
            std::fs::write(dir.join("sibling-id.jsonl"), "data\n").unwrap();

            let mut dead = rec("claude", "dead-id", Some("/w"));
            dead.transcript_path =
                Some(dir.join("dead-id.jsonl").to_string_lossy().to_string());
            let repaired = resolve_claude_record(dead.clone()).unwrap();
            assert_eq!(repaired.session_id, "sibling-id");

            // Two siblings → ambiguous, no repair.
            std::fs::write(dir.join("other-id.jsonl"), "data\n").unwrap();
            assert!(resolve_claude_record(dead.clone()).is_none());

            // No container dir → not anchored, no repair.
            let _ = std::fs::remove_dir_all(dir.join("dead-id"));
            let _ = std::fs::remove_file(dir.join("other-id.jsonl"));
            assert!(resolve_claude_record(dead).is_none());
            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    #[test]
    fn shell_quoting_escapes_single_quotes() {
        assert_eq!(shell_single_quoted("it's"), "'it'\\''s'");
        assert_eq!(shell_single_quoted("plain"), "'plain'");
    }

    #[test]
    fn store_round_trip_and_prune() {
        with_store(|| {
            record("t1", rec("claude", "s1", Some("/tmp"))).unwrap();
            let mut stale = rec("codex", "s2", None);
            stale.updated_at = 1;
            record("t2", stale).unwrap();
            record("t3", rec("pi", "s3", None)).unwrap();
            let store = load_store();
            assert_eq!(store["t1"].pending.as_ref().unwrap().session_id, "s1");
            assert!(!store.contains_key("t2"), "stale record should be pruned");
            assert!(store.contains_key("t3"));
        });
    }
}
