//! Agent notification hook installation (Claude Code, Codex, shell).
//!
//! Installs/uninstalls hooks into agent config files so that agents
//! call `notmux notify` when they finish a turn or need input.

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
fn add_codex_notify_entry(config: &str, cmd: &str) -> String {
    if config.contains(cmd) {
        return config.to_string();
    }
    let escaped = cmd.replace('\\', "\\\\").replace('"', "\\\"");
    if let Some(start) = config.find("notify = [") {
        let bracket_open = config[start..].find('[').unwrap() + start;
        let bracket_close = config[bracket_open..].find(']').unwrap() + bracket_open;
        let before = &config[..bracket_close];
        let after = &config[bracket_close..];
        let inner = config[bracket_open + 1..bracket_close].trim();
        if inner.is_empty() {
         return format!("{}\"{}\"{}", &config[..bracket_open + 1], escaped, after);
        }
        return format!("{}, \"{}\"{}", before, escaped, after);
    }
    let mut result = config.to_string();
    if !result.ends_with('\n') && !result.is_empty() {
        result.push('\n');
    }
    result.push_str(&format!("notify = [\"{}\"]\n", escaped));
    result
}
fn remove_codex_notify_entry(config: &str, exe: &str) -> String {
    if !config.contains(exe) {
        return config.to_string();
    }
    let mut result = String::new();
    for line in config.lines() {
        if line.trim_start().starts_with("notify = [") && line.contains(exe) {
         let escaped_exe = exe.replace('\\', "\\\\").replace('"', "\\\"");
         let entries: Vec<&str> = line[10..]
            .trim_end()
            .trim_end_matches(']')
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
         let remaining: Vec<&str> = entries
            .into_iter()
            .filter(|e| !e.contains(&escaped_exe) && !e.contains(exe))
            .collect();
         if remaining.is_empty() {
            continue;
         }
         result.push_str("notify = [");
         result.push_str(&remaining.join(", "));
         result.push_str("]\n");
         continue;
        }
        result.push_str(line);
        result.push('\n');
    }
    if result.ends_with('\n') && !config.ends_with('\n') {
        result.pop();
    }
    result
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
    let stop_cmd =
        format!("{exe} notify --title \"Claude Code\" --body \"$(cat | jq -r '.last_assistant_message // \"Agent finished\"' 2>/dev/null | head -c 200)\" || true");
    let notify_cmd = format!(
        "{exe} notify --title \"$(cat | jq -r '.title // \"Claude Code\"' 2>/dev/null)\" --body \"$(cat | jq -r '.message // \"Notification\"' 2>/dev/null | head -c 200)\" || true"
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
        "Notification".to_string(),
        serde_json::json!([{ "matcher": "", "hooks": [{ "type": "command", "command": notify_cmd }] }]),
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
        for key in ["Stop", "Notification"] {
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

/// Install Codex hooks: `Stop` event + enable hooks in config.toml.
pub fn install_codex() -> Result<(), String> {
    let home = home_dir().ok_or("HOME not set")?;
    let codex_dir = std::env::var("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home.join(".codex"));
    std::fs::create_dir_all(&codex_dir)
        .map_err(|e| format!("Failed to create {}: {e}", codex_dir.display()))?;
    let exe = notmux_binary();
    let stop_cmd = format!(
        "{exe} notify --title \"Codex\" --body \"Turn complete\""
    );
    let config_path = codex_dir.join("config.toml");
    let config = std::fs::read_to_string(&config_path).unwrap_or_default();
    let new_config = add_codex_notify_entry(&config, &stop_cmd);
    if new_config != config {
        std::fs::write(&config_path, &new_config)
         .map_err(|e| format!("Failed to write {}: {e}", config_path.display()))?;
    }
    log::info!("Installed Codex notify -> {}", config_path.display());
    Ok(())
}

/// Remove Codex hooks.
pub fn uninstall_codex() -> Result<(), String> {
    let home = home_dir().ok_or("HOME not set")?;
    let codex_dir = std::env::var("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home.join(".codex"));
    let config_path = codex_dir.join("config.toml");
    if let Ok(config) = std::fs::read_to_string(&config_path) {
        let exe = notmux_binary();
        let new_config = remove_codex_notify_entry(&config, &exe);
        if new_config != config {
         let _ = std::fs::write(&config_path, &new_config);
        }
    }
    let hooks_path = codex_dir.join("hooks.json");
    if hooks_path.exists() {
        std::fs::remove_file(&hooks_path)
         .map_err(|e| format!("Failed to remove {}: {e}", hooks_path.display()))?;
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

/// Install all agent hooks. Returns a list of errors (empty on full success).
pub fn install_all() -> Vec<String> {
    let mut errors = Vec::new();
    if let Err(e) = install_claude() {
        errors.push(e);
    }
    if let Err(e) = install_codex() {
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
    if let Err(e) = uninstall_shell() {
        errors.push(e);
    }
    errors
}
