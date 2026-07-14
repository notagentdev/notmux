//! Agent notification hook installation (Claude Code, Codex, shell).
//!
//! Installs/uninstalls hooks into agent config files so that agents
//! call `notmux notify` when they finish a turn or need input.

use sha2::{Digest, Sha256};
use std::path::PathBuf;

fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// Resolve the notmux binary path. Uses the current executable so the
/// installed app and dev builds both work.
fn notmux_binary() -> String {
    if let Ok(exe) = std::env::current_exe() {
        return exe.to_string_lossy().to_string();
    }
    "notmux".to_string()
}

const MARKER_BEGIN: &str = "# >>> notmux hooks >>>";
const MARKER_END: &str = "# <<< notmux hooks <<<";
/// Timeout (ms) baked into every codex hook — must match the value used when
/// computing the trust hash, or codex will treat the hook as untrusted.
const CODEX_HOOK_TIMEOUT_MS: u64 = 10_000;

/// Compute codex's per-hook trust hash for a `command`-type handler with no
/// matcher. Verified byte-for-byte against codex 0.142.5: `sha256:` + sha256 of
/// the canonical JSON
/// `{"event_name":<label>,"hooks":[{"async":false,"command":<cmd>,"timeout":<ms>,"type":"command"}]}`
/// (keys in exactly this order, forward slashes unescaped). `serde_json` matches
/// codex's serialization for ASCII commands, which ours always are.
fn codex_hook_trust_hash(event_label: &str, command: &str, timeout_ms: u64) -> String {
    let cmd = serde_json::to_string(command).unwrap_or_else(|_| "\"\"".to_string());
    let label = serde_json::to_string(event_label).unwrap_or_else(|_| "\"\"".to_string());
    let identity = format!(
        "{{\"event_name\":{label},\"hooks\":[{{\"async\":false,\"command\":{cmd},\"timeout\":{timeout_ms},\"type\":\"command\"}}]}}"
    );
    let digest = Sha256::digest(identity.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

/// Escape a string for use inside a TOML basic string (`"..."`).
fn toml_basic_string_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Split the inside of a single-line TOML array on top-level commas (i.e. commas
/// that are not inside a quoted string). Returns trimmed elements.
fn split_toml_array(inner: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_str = false;
    let mut escaped = false;
    for c in inner.chars() {
        if in_str {
            cur.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                cur.push(c);
            }
            ',' => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// Drop any `notify = [...]` array elements that reference the notmux binary
/// (cleanup for the mangled entry an earlier notify-based install could append).
fn strip_notmux_codex_notify(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .filter_map(|line| {
            let t = line.trim_start();
            if !(t.starts_with("notify") && t.contains('=') && line.contains('[')) {
                return Some(line);
            }
            let lb = line.find('[');
            let rb = line.rfind(']');
            let (Some(lb), Some(rb)) = (lb, rb) else {
                return Some(line);
            };
            if rb <= lb {
                return Some(line);
            }
            let prefix = &line[..=lb];
            let suffix = &line[rb..];
            let kept: Vec<String> = split_toml_array(&line[lb + 1..rb])
                .into_iter()
                .filter(|e| !e.contains("notmux"))
                .collect();
            if kept.is_empty() {
                return None; // whole notify entry was ours
            }
            Some(format!("{prefix}{}{suffix}", kept.join(", ")))
        })
        .collect()
}

/// Remove `[hooks.state."<hooks_path>:..."]` trust blocks we previously wrote,
/// so re-installing (e.g. after the binary path changes) never leaves stale
/// hashes behind.
fn strip_codex_hook_trust_blocks(lines: Vec<String>, hooks_path: &str) -> Vec<String> {
    let prefix = format!("[hooks.state.\"{}:", toml_basic_string_escape(hooks_path));
    let mut out = Vec::new();
    let mut skipping = false;
    for line in lines {
        let t = line.trim_start();
        if t.starts_with('[') {
            skipping = t.starts_with(&prefix);
            if skipping {
                continue;
            }
        }
        if skipping {
            continue;
        }
        out.push(line);
    }
    // Collapse a trailing run of blank lines to at most one.
    while out.len() >= 2
        && out.last().map(|l| l.trim().is_empty()).unwrap_or(false)
        && out[out.len() - 2].trim().is_empty()
    {
        out.pop();
    }
    out
}

/// Ensure `hooks = true` exists under a `[features]` table.
fn ensure_features_hooks_true(lines: &mut Vec<String>) {
    if let Some(fi) = lines.iter().position(|l| l.trim() == "[features]") {
        let mut i = fi + 1;
        while i < lines.len() {
            let t = lines[i].trim();
            if t.starts_with('[') {
                break;
            }
            if t.starts_with("hooks") && t.contains('=') {
                lines[i] = "hooks = true".to_string();
                return;
            }
            i += 1;
        }
        lines.insert(fi + 1, "hooks = true".to_string());
        return;
    }
    if lines.last().map(|l| !l.trim().is_empty()).unwrap_or(false) {
        lines.push(String::new());
    }
    lines.push("[features]".to_string());
    lines.push("hooks = true".to_string());
}

/// Produce config.toml content that enables + trusts the notmux codex hooks.
fn codex_config_with_hooks(
    existing: &str,
    hooks_path: &str,
    entries: &[(String, String)],
) -> String {
    let mut lines: Vec<String> = existing.lines().map(|l| l.to_string()).collect();
    lines = strip_notmux_codex_notify(lines);
    lines = strip_codex_hook_trust_blocks(lines, hooks_path);
    ensure_features_hooks_true(&mut lines);
    if lines.last().map(|l| !l.trim().is_empty()).unwrap_or(false) {
        lines.push(String::new());
    }
    for (key, hash) in entries {
        lines.push(format!("[hooks.state.\"{}\"]", toml_basic_string_escape(key)));
        lines.push(format!("trusted_hash = \"{hash}\""));
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Produce config.toml content with the notmux codex hook trust removed.
fn codex_config_without_hooks(existing: &str, hooks_path: &str) -> String {
    let mut lines: Vec<String> = existing.lines().map(|l| l.to_string()).collect();
    lines = strip_notmux_codex_notify(lines);
    lines = strip_codex_hook_trust_blocks(lines, hooks_path);
    let mut out = lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// Install Claude Code hooks: `Stop` + `Notification` events.
pub fn install_claude() -> Result<(), String> {
    let home = home_dir().ok_or("HOME not set")?;
    let claude_dir = std::env::var("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home.join(".claude"));
    std::fs::create_dir_all(&claude_dir)
        .map_err(|e| format!("Failed to create {}: {e}", claude_dir.display()))?;

    let settings_path = claude_dir.join("settings.json");
    let mut settings: serde_json::Value = std::fs::read_to_string(&settings_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));

    let exe = notmux_binary();
    // Static, guarded bodies. Guard on NOTMUX_SURFACE_ID so `claude` run outside
    // notmux never broadcasts to every pane. Use a fixed short "Turn complete"
    // rather than the assistant's full message — the message could be long and,
    // when it contained quotes/newlines, broke the `--body "$(…)"` interpolation
    // and produced an empty label.
    // All hook bodies stay silent (`>/dev/null 2>&1`) and exit 0 (`|| true`):
    // Claude Code surfaces any hook stdout/stderr and non-zero exit in the
    // conversation (e.g. `notmux notify` token errors under "Ran 2 stop
    // hooks"), and silence + success is the supported way to keep hooks
    // invisible there.
    let stop_cmd = format!(
        "[ -n \"$NOTMUX_SURFACE_ID\" ] && \"{exe}\" notify --title \"Claude Code\" --body \"Turn complete\" >/dev/null 2>&1 || true"
    );
    // Submitting a prompt marks the agent as working (drives the sidebar spinner)
    // and clears any stale turn-complete notification.
    let working_cmd = format!(
        "[ -n \"$NOTMUX_SURFACE_ID\" ] && \"{exe}\" agent-status working >/dev/null 2>&1 || true"
    );
    // Session restore: SessionStart persists {session_id, transcript_path, cwd}
    // from the hook's stdin payload keyed by this pane's surface id; `$PPID` is
    // the claude process (hook shells are its direct children) and serves as
    // liveness evidence on restore. SessionEnd marks a normal exit so the
    // session is not auto-resumed after a relaunch.
    let session_start_cmd = format!(
        "[ -n \"$NOTMUX_SURFACE_ID\" ] && \"{exe}\" agent-session record --kind claude --pid \"$PPID\" >/dev/null 2>&1 || true"
    );
    let session_end_cmd = format!(
        "[ -n \"$NOTMUX_SURFACE_ID\" ] && \"{exe}\" agent-session end --kind claude >/dev/null 2>&1 || true"
    );

    let hooks = settings
        .as_object_mut()
        .ok_or("settings.json is not an object")?
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    let hooks_obj = hooks.as_object_mut().ok_or("hooks is not an object")?;

    hooks_obj.insert(
        "Stop".to_string(),
        serde_json::json!([{ "matcher": "", "hooks": [{ "type": "command", "command": stop_cmd }] }]),
    );
    hooks_obj.insert(
        "UserPromptSubmit".to_string(),
        serde_json::json!([{ "matcher": "", "hooks": [{ "type": "command", "command": working_cmd }] }]),
    );
    hooks_obj.insert(
        "SessionStart".to_string(),
        serde_json::json!([{ "matcher": "", "hooks": [{ "type": "command", "command": session_start_cmd }] }]),
    );
    hooks_obj.insert(
        "SessionEnd".to_string(),
        serde_json::json!([{ "matcher": "", "hooks": [{ "type": "command", "command": session_end_cmd }] }]),
    );
    // Approval prompts: Claude's `Notification` event fires for both permission
    // requests and the ~60s idle ping. Filter on the hook's stdin JSON (the
    // `notification_type` field, with a message-text fallback for older
    // versions) so only approvals ring the bell — the idle ping stays ignored.
    let approval_cmd = format!(
        "[ -n \"$NOTMUX_SURFACE_ID\" ] && grep -qE '\"notification_type\"[[:space:]]*:[[:space:]]*\"permission_prompt\"|needs your permission' && \"{exe}\" notify --title \"Claude Code\" --body \"Approval needed\" >/dev/null 2>&1 || true"
    );
    hooks_obj.insert(
        "Notification".to_string(),
        serde_json::json!([{ "matcher": "", "hooks": [{ "type": "command", "command": approval_cmd }] }]),
    );

    std::fs::write(&settings_path, serde_json::to_string_pretty(&settings).unwrap())
        .map_err(|e| format!("Failed to write {}: {e}", settings_path.display()))?;
    log::info!("Installed Claude Code hooks -> {}", settings_path.display());
    Ok(())
}

/// Remove Claude Code hooks written by [`install_claude`].
pub fn uninstall_claude() -> Result<(), String> {
    let home = home_dir().ok_or("HOME not set")?;
    let settings_path = home.join(".claude/settings.json");
    let mut settings: serde_json::Value = std::fs::read_to_string(&settings_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .ok_or("No Claude Code settings found")?;

    if let Some(hooks) = settings.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        let exe = notmux_binary();
        for key in ["Stop", "Notification", "UserPromptSubmit", "SessionStart", "SessionEnd"] {
         if let Some(arr) = hooks.get_mut(key).and_then(|v| v.as_array_mut()) {
            for matcher_obj in arr.iter_mut() {
                if let Some(inner) = matcher_obj.get_mut("hooks").and_then(|h| h.as_array_mut()) {
                    inner.retain(|hook_entry| {
                        let cmd = hook_entry.get("command").and_then(|c| c.as_str()).unwrap_or("");
                        !cmd.contains(&exe)
                    });
                }
            }
            arr.retain(|entry| {
                if entry.get("hooks").is_some() {
                    entry
                        .get("hooks")
                        .and_then(|h| h.as_array())
                        .map(|a| !a.is_empty())
                        .unwrap_or(true)
                } else {
                    let cmd = entry.get("command").and_then(|c| c.as_str()).unwrap_or("");
                    !cmd.contains(&exe)
                }
            });
            if arr.is_empty() {
                hooks.remove(key);
            }
         }
        }
        if hooks.is_empty() {
            settings.as_object_mut().unwrap().remove("hooks");
        }
    }

    std::fs::write(&settings_path, serde_json::to_string_pretty(&settings).unwrap())
        .map_err(|e| format!("Failed to write {}: {e}", settings_path.display()))?;
    log::info!("Removed Claude Code hooks <- {}", settings_path.display());
    Ok(())
}

/// Install persistent, *trusted* codex hooks so they run WITHOUT the
/// `--dangerously-bypass-hook-trust` warning.
///
/// Writes `~/.codex/hooks.json` (Stop → notify, UserPromptSubmit → clear) and,
/// in `config.toml`, enables `features.hooks` and adds the matching
/// `[hooks.state."…"]` trust hashes codex expects. Each hook command no-ops
/// unless `NOTMUX_SURFACE_ID` is set, so plain codex outside notmux stays quiet.
///
/// Only runs when codex is already set up (`~/.codex` exists) — we never create
/// codex config for users who don't use it.
pub fn install_codex() -> Result<(), String> {
    let home = home_dir().ok_or("HOME not set")?;
    let codex_dir = std::env::var("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home.join(".codex"));
    if !codex_dir.exists() {
        log::info!("Codex not set up ({} missing); skipping", codex_dir.display());
        return Ok(());
    }
    let exe = notmux_binary();
    // ASCII-only, guarded, fire-and-forget (codex blocks on hooks). The exact
    // bytes here feed the trust hash below, so keep them in sync.
    let stop_cmd = format!(
        "[ -n \"$NOTMUX_SURFACE_ID\" ] && ( nohup \"{exe}\" notify --title Codex --body \"Turn complete\" >/dev/null 2>&1 & ) 2>/dev/null; echo {{}}"
    );
    let working_cmd = format!(
        "[ -n \"$NOTMUX_SURFACE_ID\" ] && ( nohup \"{exe}\" agent-status working >/dev/null 2>&1 & ) 2>/dev/null; echo {{}}"
    );
    // Fires right before codex shows an interactive approval prompt. The
    // trailing `echo {}` returns "no decision" so the prompt still appears.
    let approval_cmd = format!(
        "[ -n \"$NOTMUX_SURFACE_ID\" ] && ( nohup \"{exe}\" notify --title Codex --body \"Approval needed\" >/dev/null 2>&1 & ) 2>/dev/null; echo {{}}"
    );
    // Session restore: capture the stdin payload synchronously (codex may
    // close the pipe before a backgrounded child reads it), then hand it to
    // the slow binary in the background so codex is never blocked on us.
    let session_start_cmd = format!(
        "payload=$(cat); [ -n \"$NOTMUX_SURFACE_ID\" ] && ( printf %s \"$payload\" | nohup \"{exe}\" agent-session record --kind codex --pid \"$PPID\" >/dev/null 2>&1 & ) 2>/dev/null; echo {{}}"
    );

    let hooks_path = codex_dir.join("hooks.json");
    let doc = serde_json::json!({
        "hooks": {
            "Stop": [{ "hooks": [{ "type": "command", "command": stop_cmd, "timeout": CODEX_HOOK_TIMEOUT_MS }] }],
            "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": working_cmd, "timeout": CODEX_HOOK_TIMEOUT_MS }] }],
            "PermissionRequest": [{ "hooks": [{ "type": "command", "command": approval_cmd, "timeout": CODEX_HOOK_TIMEOUT_MS }] }],
            "SessionStart": [{ "hooks": [{ "type": "command", "command": session_start_cmd, "timeout": CODEX_HOOK_TIMEOUT_MS }] }],
        }
    });
    let hooks_content =
        serde_json::to_string_pretty(&doc).map_err(|e| format!("Failed to serialize hooks: {e}"))?;
    std::fs::write(&hooks_path, &hooks_content)
        .map_err(|e| format!("Failed to write {}: {e}", hooks_path.display()))?;

    // codex keys hook trust on the *canonicalized* hooks-file path.
    let key_path = std::fs::canonicalize(&hooks_path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| hooks_path.to_string_lossy().to_string());
    let entries = vec![
        (
            format!("{key_path}:stop:0:0"),
            codex_hook_trust_hash("stop", &stop_cmd, CODEX_HOOK_TIMEOUT_MS),
        ),
        (
            format!("{key_path}:user_prompt_submit:0:0"),
            codex_hook_trust_hash("user_prompt_submit", &working_cmd, CODEX_HOOK_TIMEOUT_MS),
        ),
        (
            format!("{key_path}:permission_request:0:0"),
            codex_hook_trust_hash("permission_request", &approval_cmd, CODEX_HOOK_TIMEOUT_MS),
        ),
        (
            format!("{key_path}:session_start:0:0"),
            codex_hook_trust_hash("session_start", &session_start_cmd, CODEX_HOOK_TIMEOUT_MS),
        ),
    ];

    let config_path = codex_dir.join("config.toml");
    let config = std::fs::read_to_string(&config_path).unwrap_or_default();
    let new_config = codex_config_with_hooks(&config, &key_path, &entries);
    if new_config != config {
        std::fs::write(&config_path, &new_config)
            .map_err(|e| format!("Failed to write {}: {e}", config_path.display()))?;
    }
    log::info!("Installed Codex hooks -> {}", config_path.display());
    Ok(())
}

/// Remove the persistent codex hooks + trust written by [`install_codex`].
pub fn uninstall_codex() -> Result<(), String> {
    let home = home_dir().ok_or("HOME not set")?;
    let codex_dir = std::env::var("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home.join(".codex"));
    let hooks_path = codex_dir.join("hooks.json");
    let key_path = std::fs::canonicalize(&hooks_path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| hooks_path.to_string_lossy().to_string());
    let config_path = codex_dir.join("config.toml");
    if let Ok(config) = std::fs::read_to_string(&config_path) {
        let new_config = codex_config_without_hooks(&config, &key_path);
        if new_config != config {
            let _ = std::fs::write(&config_path, &new_config);
        }
    }
    if hooks_path.exists() {
        let _ = std::fs::remove_file(&hooks_path);
    }
    log::info!("Removed Codex hooks <- {}", codex_dir.display());
    Ok(())
}

/// Install shell integration: `notmux-notify` helper function.
pub fn install_shell() -> Result<(), String> {
    let home = home_dir().ok_or("HOME not set")?;
    let exe = notmux_binary();
    let block = format!(
        "{MARKER_BEGIN}\n\
         # Send a notification to notmux (e.g. after a long build)\n\
         # Usage: notmux-notify \"Build complete\" or: make build && notmux-notify \"Done\"\n\
         notmux-notify() {{ {exe} notify --title \"Shell\" --body \"$1\" 2>/dev/null; }}\n\
         {MARKER_END}\n"
    );
    for rc in [".zshrc", ".bashrc"] {
        let rc_path = home.join(rc);
        let existing = std::fs::read_to_string(&rc_path).unwrap_or_default();
        let new_content = if existing.contains(MARKER_BEGIN) {
            let start = existing.find(MARKER_BEGIN).unwrap();
            let end = existing.find(MARKER_END).unwrap() + MARKER_END.len();
            format!("{}{}{}", &existing[..start], block.trim_end(), &existing[end..])
        } else {
            let mut c = existing;
            if !c.ends_with('\n') && !c.is_empty() {
                c.push('\n');
            }
            c.push_str(&block);
            c
        };
        std::fs::write(&rc_path, new_content)
            .map_err(|e| format!("Failed to write {}: {e}", rc_path.display()))?;
        log::info!("Installed shell integration -> {}", rc_path.display());
    }
    Ok(())
}

/// Remove shell integration.
pub fn uninstall_shell() -> Result<(), String> {
    let home = home_dir().ok_or("HOME not set")?;
    for rc in [".zshrc", ".bashrc"] {
        let rc_path = home.join(rc);
        let existing = match std::fs::read_to_string(&rc_path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if let (Some(start), Some(end_idx)) =
            (existing.find(MARKER_BEGIN), existing.find(MARKER_END))
        {
            let mut end = end_idx + MARKER_END.len();
            if end < existing.len() && existing.as_bytes()[end] == b'\n' {
                end += 1;
            }
            let new_content = format!("{}{}", &existing[..start], &existing[end..]);
            let _ = std::fs::write(&rc_path, new_content);
            log::info!("Removed shell integration <- {}", rc_path.display());
        }
    }
    Ok(())
}

/// Resolve the notagent config dir: `$NOTAGENT_CONFIG` or `~/.notagent`.
fn notagent_config_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("NOTAGENT_CONFIG")
        && !dir.is_empty()
    {
        return Some(PathBuf::from(dir));
    }
    home_dir().map(|h| h.join(".notagent"))
}

/// Ensure `hooks_enabled = true` at the top level of a notagent `.notagent.toml`.
/// notagent's hook system is gated on this global switch — when it's `false`,
/// no `hooks.json` is read at all.
fn ensure_notagent_hooks_enabled(existing: &str) -> String {
    let mut lines: Vec<String> = existing.lines().map(|l| l.to_string()).collect();
    for line in lines.iter_mut() {
        let t = line.trim_start();
        if t.starts_with("hooks_enabled") && t.contains('=') {
            *line = "hooks_enabled = true".to_string();
            let mut out = lines.join("\n");
            out.push('\n');
            return out;
        }
    }
    // Not present: insert as a top-level key before the first `[table]` header
    // (a bare key appended after a table would be parsed as part of that table).
    let insert_at = lines
        .iter()
        .position(|l| l.trim_start().starts_with('['))
        .unwrap_or(lines.len());
    lines.insert(insert_at, "hooks_enabled = true".to_string());
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Install notagent hooks. notagent uses the same hook format as Claude/Codex,
/// but hooks are on by default with no trust step — so we simply write our
/// events into `<config>/hooks.json`. Stdout is suppressed so `notify`'s JSON
/// output can't be mistaken for a hook decision. Only runs if `~/.notagent`
/// already exists.
pub fn install_notagent() -> Result<(), String> {
    let config_dir = notagent_config_dir().ok_or("HOME not set")?;
    if !config_dir.exists() {
        log::info!(
            "notagent not set up ({} missing); skipping",
            config_dir.display()
        );
        return Ok(());
    }
    let exe = notmux_binary();
    let stop_cmd = format!(
        "[ -n \"$NOTMUX_SURFACE_ID\" ] && \"{exe}\" notify --title notagent --body \"Turn complete\" >/dev/null 2>&1 || true"
    );
    let working_cmd = format!(
        "[ -n \"$NOTMUX_SURFACE_ID\" ] && \"{exe}\" agent-status working >/dev/null 2>&1 || true"
    );
    let hooks_path = config_dir.join("hooks.json");
    // Preserve any hooks the user already defined; only replace our two events.
    let mut doc: serde_json::Value = std::fs::read_to_string(&hooks_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let hooks = doc
        .as_object_mut()
        .ok_or("hooks.json is not an object")?
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    let hooks_obj = hooks.as_object_mut().ok_or("hooks is not an object")?;
    hooks_obj.insert(
        "Stop".to_string(),
        serde_json::json!([{ "hooks": [{ "type": "command", "command": stop_cmd, "timeout": 10 }] }]),
    );
    hooks_obj.insert(
        "UserPromptSubmit".to_string(),
        serde_json::json!([{ "hooks": [{ "type": "command", "command": working_cmd, "timeout": 10 }] }]),
    );
    // Session restore: notagent's SessionStart payload carries the
    // conversation id as `session_id` (resumable via `notagent --cid <id>`).
    let session_start_cmd = format!(
        "[ -n \"$NOTMUX_SURFACE_ID\" ] && \"{exe}\" agent-session record --kind notagent --pid \"$PPID\" >/dev/null 2>&1 || true"
    );
    hooks_obj.insert(
        "SessionStart".to_string(),
        serde_json::json!([{ "hooks": [{ "type": "command", "command": session_start_cmd, "timeout": 10 }] }]),
    );
    // Fires right before notagent shows an interactive approval prompt.
    // Stdout is suppressed so nothing is mistaken for an allow/deny decision —
    // the prompt still appears; we only ring the bell.
    let approval_cmd = format!(
        "[ -n \"$NOTMUX_SURFACE_ID\" ] && \"{exe}\" notify --title notagent --body \"Approval needed\" >/dev/null 2>&1 || true"
    );
    hooks_obj.insert(
        "PermissionRequest".to_string(),
        serde_json::json!([{ "hooks": [{ "type": "command", "command": approval_cmd, "timeout": 10 }] }]),
    );
    std::fs::write(&hooks_path, serde_json::to_string_pretty(&doc).unwrap())
        .map_err(|e| format!("Failed to write {}: {e}", hooks_path.display()))?;

    // Respect the global on/off switch: notagent ignores hooks.json entirely
    // when `hooks_enabled = false`, so make sure it's enabled.
    let config_toml = config_dir.join(".notagent.toml");
    let existing_cfg = std::fs::read_to_string(&config_toml).unwrap_or_default();
    let updated_cfg = ensure_notagent_hooks_enabled(&existing_cfg);
    if updated_cfg != existing_cfg {
        let _ = std::fs::write(&config_toml, updated_cfg);
    }

    log::info!("Installed notagent hooks -> {}", hooks_path.display());
    Ok(())
}

/// Remove the notagent hooks written by [`install_notagent`].
pub fn uninstall_notagent() -> Result<(), String> {
    let config_dir = notagent_config_dir().ok_or("HOME not set")?;
    let hooks_path = config_dir.join("hooks.json");
    if let Ok(content) = std::fs::read_to_string(&hooks_path)
        && let Ok(mut doc) = serde_json::from_str::<serde_json::Value>(&content)
    {
        let exe = notmux_binary();
        if let Some(hooks_obj) = doc.get_mut("hooks").and_then(|h| h.as_object_mut()) {
            for key in ["Stop", "UserPromptSubmit", "PermissionRequest", "SessionStart"] {
                if hooks_obj
                    .get(key)
                    .map(|v| v.to_string().contains(&exe))
                    .unwrap_or(false)
                {
                    hooks_obj.remove(key);
                }
            }
        }
        let _ = std::fs::write(&hooks_path, serde_json::to_string_pretty(&doc).unwrap());
    }
    log::info!("Removed notagent hooks <- {}", config_dir.display());
    Ok(())
}

/// Marker identifying our OpenCode plugin file (never edit foreign files).
const OPENCODE_PLUGIN_MARKER: &str = "notmux-opencode-plugin-marker";

/// Resolve the OpenCode config dir: `$OPENCODE_CONFIG_DIR` or `~/.config/opencode`.
fn opencode_config_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("OPENCODE_CONFIG_DIR")
        && !dir.is_empty()
    {
        return Some(PathBuf::from(dir));
    }
    home_dir().map(|h| h.join(".config/opencode"))
}

/// Install the OpenCode integration: a small JS plugin (OpenCode has no
/// hooks.json — plugins subscribe to the event bus). Turn completion comes
/// from `session.idle`, approvals from the permission events. No "working"
/// signal: like the the reference implementation reference, `message.updated` is NOT a reliable
/// prompt-submit — OpenCode re-emits it for the *user* message at turn end,
/// which would clear the fresh turn-complete bell and restart the spinner.
/// Only runs if OpenCode is already set up.
pub fn install_opencode() -> Result<(), String> {
    let config_dir = opencode_config_dir().ok_or("HOME not set")?;
    if !config_dir.exists() {
        log::info!(
            "OpenCode not set up ({} missing); skipping",
            config_dir.display()
        );
        return Ok(());
    }
    let exe = serde_json::to_string(&notmux_binary()).unwrap_or_else(|_| "\"notmux\"".to_string());
    let plugin = format!(
        r#"// {OPENCODE_PLUGIN_MARKER} v2
// Bridges OpenCode lifecycle events to notmux (bell + agent status).
// Installed by notmux. DO NOT EDIT MANUALLY — notmux rewrites this file.
import {{ spawnSync }} from "node:child_process";

const NOTMUX = {exe};

function send(args) {{
  // Only inside a notmux terminal — plain opencode elsewhere stays quiet.
  if (!process.env.NOTMUX_SURFACE_ID) return;
  try {{
    spawnSync(NOTMUX, args, {{ stdio: ["ignore", "ignore", "ignore"], timeout: 5000 }});
  }} catch (_) {{}}
}}

// Session restore: same id candidates as the the reference implementation reference plugin
// (resumable via `opencode --session <id>`).
function sessionIdFor(event, props) {{
  const candidates = [
    props.info && props.info.id,
    props.sessionID,
    props.sessionId,
    props.session_id,
    props.session && props.session.id,
    event && event.sessionID,
  ];
  for (const c of candidates) {{
    if (typeof c === "string" && c.length > 0) return c;
  }}
  return null;
}}

function recordSession(event, props) {{
  const id = sessionIdFor(event, props);
  if (!id) return;
  send([
    "agent-session", "record", "--kind", "opencode",
    "--session-id", id,
    "--cwd", process.cwd(),
    "--pid", String(process.pid),
  ]);
}}

export const NotmuxBridge = async () => ({{
  event: async ({{ event }}) => {{
    const type = event && event.type;
    const props = (event && event.properties) || {{}};
    // No prompt-submit/"working" mapping: message.updated re-fires for the
    // user message at turn end and would undo the turn-complete bell.
    if (type === "session.created") {{
      recordSession(event, props);
      return;
    }}
    if (type === "session.deleted") {{
      send(["agent-session", "end", "--kind", "opencode"]);
      return;
    }}
    if (
      type === "session.idle" ||
      (type === "session.status" && props.status && props.status.type === "idle")
    ) {{
      recordSession(event, props);
      send(["notify", "--title", "OpenCode", "--body", "Turn complete"]);
      return;
    }}
    if (type === "permission.updated" || type === "permission.asked") {{
      send(["notify", "--title", "OpenCode", "--body", "Approval needed"]);
    }}
  }},
}});
export default NotmuxBridge;
"#
    );

    let plugins_dir = config_dir.join("plugins");
    std::fs::create_dir_all(&plugins_dir)
        .map_err(|e| format!("Failed to create {}: {e}", plugins_dir.display()))?;
    let plugin_path = plugins_dir.join("notmux-session.js");
    // Never overwrite a foreign file at our path.
    if let Ok(existing) = std::fs::read_to_string(&plugin_path)
        && !existing.contains(OPENCODE_PLUGIN_MARKER)
    {
        return Err(format!(
            "{} exists but was not written by notmux; not overwriting",
            plugin_path.display()
        ));
    }
    std::fs::write(&plugin_path, plugin)
        .map_err(|e| format!("Failed to write {}: {e}", plugin_path.display()))?;
    log::info!("Installed OpenCode plugin -> {}", plugin_path.display());
    Ok(())
}

/// Remove the OpenCode plugin written by [`install_opencode`].
pub fn uninstall_opencode() -> Result<(), String> {
    let Some(config_dir) = opencode_config_dir() else {
        return Ok(());
    };
    let plugin_path = config_dir.join("plugins/notmux-session.js");
    if let Ok(existing) = std::fs::read_to_string(&plugin_path)
        && existing.contains(OPENCODE_PLUGIN_MARKER)
    {
        let _ = std::fs::remove_file(&plugin_path);
        log::info!("Removed OpenCode plugin <- {}", plugin_path.display());
    }
    Ok(())
}

/// Marker identifying our Pi extension file (never edit foreign files).
const PI_EXTENSION_MARKER: &str = "notmux-pi-extension-marker";

/// Resolve the Pi agent dir: `$PI_CODING_AGENT_DIR` or `~/.pi/agent`.
fn pi_agent_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("PI_CODING_AGENT_DIR")
        && !dir.is_empty()
    {
        return Some(PathBuf::from(dir));
    }
    home_dir().map(|h| h.join(".pi/agent"))
}

/// Install the Pi integration: a TypeScript extension (Pi has no hooks.json —
/// extensions subscribe to lifecycle events). `before_agent_start` marks the
/// agent working, `agent_end` rings "Turn complete". Pi has no approval
/// system, so — like the reference implementation's `toolStartMaybeApproval` — a *side-effecting*
/// tool starting (bash/edit/write) rings an attention bell; read-only tools
/// stay quiet. Only runs if Pi is already set up.
pub fn install_pi() -> Result<(), String> {
    let agent_dir = pi_agent_dir().ok_or("HOME not set")?;
    if !agent_dir.exists() {
        log::info!("Pi not set up ({} missing); skipping", agent_dir.display());
        return Ok(());
    }
    let exe = serde_json::to_string(&notmux_binary()).unwrap_or_else(|_| "\"notmux\"".to_string());
    let extension = format!(
        r#"// {PI_EXTENSION_MARKER} v2
// Bridges Pi lifecycle events to notmux (bell + agent status).
// Installed by notmux. DO NOT EDIT MANUALLY — notmux rewrites this file.
import {{ spawn }} from "node:child_process";

const NOTMUX = {exe};

function send(args: string[]) {{
  // Only inside a notmux terminal — plain pi elsewhere stays quiet.
  if (!process.env.NOTMUX_SURFACE_ID) return;
  try {{
    const child = spawn(NOTMUX, args, {{ stdio: "ignore", detached: true }});
    child.on("error", () => {{}});
    child.unref();
  }} catch (_) {{}}
}}

// Pi has no approval system; ring the bell when a side-effecting tool
// starts (the reference implementation's toolStartMaybeApproval semantics) — read-only tools
// (read/grep/find/ls) stay quiet.
const SIDE_EFFECTING = new Set([
  "bash", "write", "edit", "multiedit", "notebookedit", "apply_patch", "shell",
]);

// Session restore: persist the current session id (resumable via
// `pi --session <id>`) keyed by this pane's surface id.
function recordSession(pi: any) {{
  try {{
    const id =
      pi && pi.sessionManager && typeof pi.sessionManager.getSessionId === "function"
        ? pi.sessionManager.getSessionId()
        : null;
    if (typeof id === "string" && id.length > 0) {{
      send([
        "agent-session", "record", "--kind", "pi",
        "--session-id", id,
        "--cwd", process.cwd(),
        "--pid", String(process.pid),
      ]);
    }}
  }} catch (_) {{}}
}}

export default function notmuxPiBridge(pi: any) {{
  pi.on("before_agent_start", async () => {{
    send(["agent-status", "working"]);
    recordSession(pi);
  }});
  pi.on("tool_execution_start", async (event: any) => {{
    const tool = String(
      (event && (event.toolName || event.tool_name || event.name)) || ""
    ).toLowerCase();
    if (SIDE_EFFECTING.has(tool)) {{
      // Mid-turn attention ping — keep the working spinner running.
      send(["notify", "--title", "Pi", "--body", "Running " + tool, "--keep-working"]);
    }}
  }});
  pi.on("agent_end", async () => {{
    send(["notify", "--title", "Pi", "--body", "Turn complete"]);
    recordSession(pi);
  }});
}}
"#
    );

    let extensions_dir = agent_dir.join("extensions");
    std::fs::create_dir_all(&extensions_dir)
        .map_err(|e| format!("Failed to create {}: {e}", extensions_dir.display()))?;
    let extension_path = extensions_dir.join("notmux-session.ts");
    // Never overwrite a foreign file at our path.
    if let Ok(existing) = std::fs::read_to_string(&extension_path)
        && !existing.contains(PI_EXTENSION_MARKER)
    {
        return Err(format!(
            "{} exists but was not written by notmux; not overwriting",
            extension_path.display()
        ));
    }
    std::fs::write(&extension_path, extension)
        .map_err(|e| format!("Failed to write {}: {e}", extension_path.display()))?;
    log::info!("Installed Pi extension -> {}", extension_path.display());
    Ok(())
}

/// Remove the Pi extension written by [`install_pi`].
pub fn uninstall_pi() -> Result<(), String> {
    let Some(agent_dir) = pi_agent_dir() else {
        return Ok(());
    };
    let extension_path = agent_dir.join("extensions/notmux-session.ts");
    if let Ok(existing) = std::fs::read_to_string(&extension_path)
        && existing.contains(PI_EXTENSION_MARKER)
    {
        let _ = std::fs::remove_file(&extension_path);
        log::info!("Removed Pi extension <- {}", extension_path.display());
    }
    Ok(())
}

/// Install Cursor (cursor-agent) hooks into `~/.cursor/hooks.json` — flat
/// format: `{"version": 1, "hooks": {"<event>": [{"command": …}]}}`.
/// `beforeSubmitPrompt` marks the agent working, `stop` rings "Turn
/// complete", and `beforeShellExecution` rings "Approval needed" (Cursor
/// gates shell commands interactively; there is no dedicated approval
/// event). Our commands print nothing, so Cursor's own permission flow is
/// untouched. Only replaces our own event keys; other events are preserved.
pub fn install_cursor() -> Result<(), String> {
    let home = home_dir().ok_or("HOME not set")?;
    let cursor_dir = home.join(".cursor");
    if !cursor_dir.exists() {
        log::info!(
            "Cursor not set up ({} missing); skipping",
            cursor_dir.display()
        );
        return Ok(());
    }
    let exe = notmux_binary();
    // Cursor has no session-start hook event; every hook payload carries the
    // conversation id, so session recording rides along on prompt-submit and
    // stop (`agent-session record` reads the payload from stdin; `$PPID` is
    // the cursor-agent process).
    let working_cmd = format!(
        "[ -n \"$NOTMUX_SURFACE_ID\" ] && {{ \"{exe}\" agent-session record --kind cursor --pid \"$PPID\"; \"{exe}\" agent-status working; }} >/dev/null 2>&1 || true"
    );
    let stop_cmd = format!(
        "[ -n \"$NOTMUX_SURFACE_ID\" ] && {{ \"{exe}\" agent-session record --kind cursor --pid \"$PPID\"; \"{exe}\" notify --title Cursor --body \"Turn complete\"; }} >/dev/null 2>&1 || true"
    );
    let approval_cmd = format!(
        "[ -n \"$NOTMUX_SURFACE_ID\" ] && \"{exe}\" notify --title Cursor --body \"Approval needed\" --keep-working >/dev/null 2>&1 || true"
    );

    let hooks_path = cursor_dir.join("hooks.json");
    let mut doc: serde_json::Value = std::fs::read_to_string(&hooks_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let root = doc.as_object_mut().ok_or("hooks.json is not an object")?;
    root.entry("version").or_insert(serde_json::json!(1));
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    let hooks_obj = hooks.as_object_mut().ok_or("hooks is not an object")?;
    let entry = |cmd: &str| serde_json::json!([{ "command": cmd }]);
    hooks_obj.insert("beforeSubmitPrompt".to_string(), entry(&working_cmd));
    hooks_obj.insert("stop".to_string(), entry(&stop_cmd));
    hooks_obj.insert("beforeShellExecution".to_string(), entry(&approval_cmd));

    std::fs::write(&hooks_path, serde_json::to_string_pretty(&doc).unwrap())
        .map_err(|e| format!("Failed to write {}: {e}", hooks_path.display()))?;
    log::info!("Installed Cursor hooks -> {}", hooks_path.display());
    Ok(())
}

/// Remove the Cursor hooks written by [`install_cursor`].
pub fn uninstall_cursor() -> Result<(), String> {
    let Some(home) = home_dir() else {
        return Ok(());
    };
    let hooks_path = home.join(".cursor/hooks.json");
    if let Ok(content) = std::fs::read_to_string(&hooks_path)
        && let Ok(mut doc) = serde_json::from_str::<serde_json::Value>(&content)
    {
        let exe = notmux_binary();
        let mut changed = false;
        if let Some(hooks_obj) = doc.get_mut("hooks").and_then(|h| h.as_object_mut()) {
            for key in ["beforeSubmitPrompt", "stop", "beforeShellExecution"] {
                if hooks_obj
                    .get(key)
                    .map(|v| v.to_string().contains(&exe))
                    .unwrap_or(false)
                {
                    hooks_obj.remove(key);
                    changed = true;
                }
            }
        }
        if changed {
            let _ = std::fs::write(&hooks_path, serde_json::to_string_pretty(&doc).unwrap());
            log::info!("Removed Cursor hooks <- {}", hooks_path.display());
        }
    }
    Ok(())
}

/// Resolve the Antigravity (agy) config dir: `~/.gemini/config`.
fn antigravity_config_dir() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".gemini/config"))
}

/// Install Antigravity (agy) hooks. Its `hooks.json` holds *named* hook
/// groups at the top level — we own the `"notmux"` key and leave everything
/// else (other tools' groups, user entries) untouched. `PreInvocation` marks
/// the agent working, `Stop`/`turn-completion` ring "Turn complete", and
/// `Notification` rings "Attention needed" (fires when agy blocks on the
/// user, e.g. tool approvals). Only runs if `~/.gemini` is already set up.
pub fn install_antigravity() -> Result<(), String> {
    let home = home_dir().ok_or("HOME not set")?;
    if !home.join(".gemini").exists() {
        log::info!("Antigravity not set up (~/.gemini missing); skipping");
        return Ok(());
    }
    let config_dir = antigravity_config_dir().ok_or("HOME not set")?;
    std::fs::create_dir_all(&config_dir)
        .map_err(|e| format!("Failed to create {}: {e}", config_dir.display()))?;

    let exe = notmux_binary();
    let working_cmd = format!(
        "[ -n \"$NOTMUX_SURFACE_ID\" ] && \"{exe}\" agent-status working >/dev/null 2>&1 || true"
    );
    let stop_cmd = format!(
        "[ -n \"$NOTMUX_SURFACE_ID\" ] && \"{exe}\" notify --title Antigravity --body \"Turn complete\" >/dev/null 2>&1 || true"
    );
    let attention_cmd = format!(
        "[ -n \"$NOTMUX_SURFACE_ID\" ] && \"{exe}\" notify --title Antigravity --body \"Attention needed\" >/dev/null 2>&1 || true"
    );
    // Antigravity has no dedicated approval event — like the reference implementation's
    // toolStartMaybeApproval, a side-effecting tool starting rings the bell
    // (filtered on the hook's stdin payload); read-only tools stay quiet.
    let tool_cmd = format!(
        "[ -n \"$NOTMUX_SURFACE_ID\" ] && grep -qE '\"tool_name\"[[:space:]]*:[[:space:]]*\"(run_command|write_to_file|replace_file_content|multi_replace_file_content|Bash|Write|Edit|shell)\"' && \"{exe}\" notify --title Antigravity --body \"Approval needed\" --keep-working >/dev/null 2>&1 || true"
    );
    // Session restore: the hook payload carries the conversation id
    // (resumable via `agy --conversation <id>`).
    let session_start_cmd = format!(
        "[ -n \"$NOTMUX_SURFACE_ID\" ] && \"{exe}\" agent-session record --kind antigravity --pid \"$PPID\" >/dev/null 2>&1 || true"
    );
    let session_end_cmd = format!(
        "[ -n \"$NOTMUX_SURFACE_ID\" ] && \"{exe}\" agent-session end --kind antigravity >/dev/null 2>&1 || true"
    );
    let entry = |cmd: &str| {
        serde_json::json!([{ "type": "command", "command": cmd, "timeout": 10 }])
    };
    let group = serde_json::json!({
        "PreInvocation": entry(&working_cmd),
        "Stop": entry(&stop_cmd),
        "turn-completion": entry(&stop_cmd),
        "Notification": entry(&attention_cmd),
        "SessionStart": entry(&session_start_cmd),
        "SessionEnd": entry(&session_end_cmd),
        // Tool events take the matcher-wrapped form.
        "PreToolUse": [{
            "matcher": "*",
            "hooks": [{ "type": "command", "command": tool_cmd, "timeout": 10 }],
        }],
    });

    let hooks_path = config_dir.join("hooks.json");
    let mut doc: serde_json::Value = std::fs::read_to_string(&hooks_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    doc.as_object_mut()
        .ok_or("hooks.json is not an object")?
        .insert("notmux".to_string(), group);
    std::fs::write(&hooks_path, serde_json::to_string_pretty(&doc).unwrap())
        .map_err(|e| format!("Failed to write {}: {e}", hooks_path.display()))?;
    log::info!("Installed Antigravity hooks -> {}", hooks_path.display());
    Ok(())
}

/// Remove the Antigravity hook group written by [`install_antigravity`].
pub fn uninstall_antigravity() -> Result<(), String> {
    let Some(config_dir) = antigravity_config_dir() else {
        return Ok(());
    };
    let hooks_path = config_dir.join("hooks.json");
    if let Ok(content) = std::fs::read_to_string(&hooks_path)
        && let Ok(mut doc) = serde_json::from_str::<serde_json::Value>(&content)
        && let Some(obj) = doc.as_object_mut()
        && obj.remove("notmux").is_some()
    {
        let _ = std::fs::write(&hooks_path, serde_json::to_string_pretty(&doc).unwrap());
        log::info!("Removed Antigravity hooks <- {}", hooks_path.display());
    }
    Ok(())
}

/// Install all agent hooks. Returns a list of errors (empty on full success).
pub fn install_all() -> Vec<String> {
    let mut errors = Vec::new();
    if let Err(e) = install_claude() {
        errors.push(e);
    }
    if let Err(e) = install_codex() {
        errors.push(e);
    }
    if let Err(e) = install_notagent() {
        errors.push(e);
    }
    if let Err(e) = install_opencode() {
        errors.push(e);
    }
    if let Err(e) = install_pi() {
        errors.push(e);
    }
    if let Err(e) = install_antigravity() {
        errors.push(e);
    }
    if let Err(e) = install_cursor() {
        errors.push(e);
    }
    if let Err(e) = install_shell() {
        errors.push(e);
    }
    errors
}

/// Uninstall all agent hooks. Returns a list of errors (empty on full success).
pub fn uninstall_all() -> Vec<String> {
    let mut errors = Vec::new();
    if let Err(e) = uninstall_claude() {
        errors.push(e);
    }
    if let Err(e) = uninstall_codex() {
        errors.push(e);
    }
    if let Err(e) = uninstall_notagent() {
        errors.push(e);
    }
    if let Err(e) = uninstall_opencode() {
        errors.push(e);
    }
    if let Err(e) = uninstall_pi() {
        errors.push(e);
    }
    if let Err(e) = uninstall_antigravity() {
        errors.push(e);
    }
    if let Err(e) = uninstall_cursor() {
        errors.push(e);
    }
    if let Err(e) = uninstall_shell() {
        errors.push(e);
    }
    errors
}
