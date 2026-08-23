//! Launch-argv capture and per-agent sanitizing for resume commands.
//!
//! When an agent session is recorded, the surrounding `notmux agent-session
//! record` call captures the agent process's argv and stores the *restorable*
//! subset (model/profile/permission flags) so the resume command relaunches
//! the agent the way the user started it. Each agent kind carries its own
//! policy, derived from that agent's CLI surface: which options take a value,
//! which are worth restoring, and which must never be replayed.

/// One agent kind's argument-preservation policy.
struct Policy {
    /// Options that consume the following token as a value.
    value_options: &'static [&'static str],
    /// Options whose value is optional; the follower is consumed only when it
    /// plausibly is a value (see [`looks_like_optional_value`]).
    optional_value_options: &'static [&'static str],
    /// Value options that greedily consume tokens until the next `-` token.
    variadic_options: &'static [&'static str],
    /// Subcommands that mean the launch cannot be replayed as a session.
    non_restorable_commands: &'static [&'static str],
    /// Options (with their value width) silently removed.
    dropped_options: &'static [&'static str],
    /// `--opt=`-style prefixes silently removed as one token.
    dropped_option_prefixes: &'static [&'static str],
    /// Options that make the whole launch non-restorable (one-shot modes).
    reject_options: &'static [&'static str],
    /// Resume subcommand whose following session-id positional is skipped.
    resume_subcommand: Option<&'static str>,
    /// Keep the first positional (opencode's project directory).
    preserve_first_positional: bool,
    /// Strip/unwrap the notmux-injected `--settings` hook JSON (claude).
    skip_claude_hook_settings: bool,
}

const EMPTY: &[&str] = &[];

impl Policy {
    const fn base() -> Self {
        Policy {
            value_options: EMPTY,
            optional_value_options: EMPTY,
            variadic_options: EMPTY,
            non_restorable_commands: EMPTY,
            dropped_options: EMPTY,
            dropped_option_prefixes: EMPTY,
            reject_options: EMPTY,
            resume_subcommand: None,
            preserve_first_positional: false,
            skip_claude_hook_settings: false,
        }
    }
}

// ── Policies ───────────────────────────────────────────────────────────────

static CLAUDE_POLICY: Policy = Policy {
    value_options: &[
        "--add-dir", "--agent", "--agents", "--allowedTools", "--allowed-tools",
        "--append-system-prompt", "--append-system-prompt-file", "--betas",
        "--dangerously-load-development-channels", "--debug-file",
        "--disallowedTools", "--disallowed-tools", "--effort", "--fallback-model",
        "--file", "--from-pr", "--input-format", "--json-schema",
        "--max-budget-usd", "--mcp-config", "--model", "--name", "-n",
        "--output-format", "--permission-mode", "--plugin-dir",
        "--remote-control-session-name-prefix", "--resume", "-r", "--session-id",
        "--setting-sources", "--settings", "--system-prompt",
        "--system-prompt-file", "--teammate-mode", "--tmux", "--tools",
        "--worktree", "-w",
    ],
    optional_value_options: &["--debug"],
    variadic_options: &[
        "--add-dir", "--allowedTools", "--allowed-tools", "--betas",
        "--disallowedTools", "--disallowed-tools", "--file", "--mcp-config",
        "--tools",
    ],
    non_restorable_commands: &[
        "agents", "auth", "auto-mode", "api-key", "config", "doctor", "install",
        "mcp", "plugin", "plugins", "rc", "remote-control", "setup-token",
        "update", "upgrade",
    ],
    dropped_options: &[
        "--continue", "-c", "--file", "--fork-session", "--from-pr", "--resume",
        "-r", "--session-id", "--tmux", "--worktree", "-w",
    ],
    dropped_option_prefixes: &[
        "--file=", "--fork-session=", "--from-pr=", "--resume=", "--session-id=",
        "--tmux=", "--worktree=",
    ],
    reject_options: &["--print", "-p", "--no-session-persistence"],
    resume_subcommand: None,
    preserve_first_positional: false,
    skip_claude_hook_settings: true,
};

static CODEX_POLICY: Policy = Policy {
    value_options: &[
        "--config", "-c", "--remote", "--remote-auth-token-env", "--image", "-i",
        "--model", "-m", "--local-provider", "--profile", "-p", "--sandbox",
        "-s", "--ask-for-approval", "-a", "--cd", "-C", "--add-dir", "--enable",
        "--disable",
    ],
    optional_value_options: EMPTY,
    variadic_options: &["--image", "-i"],
    non_restorable_commands: &[
        "exec", "e", "review", "login", "logout", "mcp", "mcp-server",
        "app-server", "app", "completion", "sandbox", "debug", "apply", "a",
        "fork", "cloud", "exec-server", "features", "help",
    ],
    dropped_options: &[
        "--last", "--image", "-i", "--remote", "--remote-auth-token-env", "--all",
    ],
    dropped_option_prefixes: &["--remote=", "--remote-auth-token-env="],
    reject_options: EMPTY,
    resume_subcommand: Some("resume"),
    preserve_first_positional: false,
    skip_claude_hook_settings: false,
};

static PI_POLICY: Policy = Policy {
    value_options: &[
        "--append-system-prompt", "--api-key", "--extension", "--fork",
        "--model", "--models", "--prompt-template", "--provider", "--resume",
        "--session", "--session-dir", "--skill", "--system-prompt", "--theme",
        "--thinking", "--tools", "-e", "-r", "-t",
    ],
    optional_value_options: EMPTY,
    variadic_options: EMPTY,
    non_restorable_commands: &[
        "config", "help", "install", "list", "login", "logout", "remove",
        "uninstall", "update",
    ],
    dropped_options: &[
        "--api-key", "--continue", "--fork", "--resume", "--session", "-c", "-r",
    ],
    dropped_option_prefixes: &["--api-key=", "--fork=", "--resume=", "--session="],
    reject_options: &[
        "--export", "--list-models", "--mode", "--no-session", "--print",
        "--prompt", "--version", "-h", "-p", "-v",
    ],
    resume_subcommand: None,
    preserve_first_positional: false,
    skip_claude_hook_settings: false,
};

static ANTIGRAVITY_POLICY: Policy = Policy {
    value_options: &[
        "--add-dir", "--conversation", "--log-file", "--print-timeout",
        "--prompt", "-p", "--sandbox",
    ],
    optional_value_options: &["--continue", "-c"],
    variadic_options: EMPTY,
    non_restorable_commands: &[
        "changelog", "help", "install", "plugin", "plugins", "update",
    ],
    dropped_options: &["--continue", "-c", "--conversation"],
    dropped_option_prefixes: &["--conversation="],
    reject_options: &["--prompt", "-p", "--prompt-interactive", "-i", "--print"],
    resume_subcommand: None,
    preserve_first_positional: false,
    skip_claude_hook_settings: false,
};

static CURSOR_POLICY: Policy = Policy {
    value_options: &[
        "--api-key", "-H", "--header", "--mode", "--model", "--output-format",
        "--resume", "--sandbox", "--workspace", "-w", "--worktree",
        "--worktree-base",
    ],
    optional_value_options: &["-w", "--resume", "--worktree"],
    variadic_options: EMPTY,
    non_restorable_commands: &[
        "about", "create-chat", "generate-rule", "help",
        "install-shell-integration", "login", "logout", "ls", "mcp", "models",
        "rule", "status", "uninstall-shell-integration", "update", "whoami",
    ],
    dropped_options: &[
        "--api-key", "-H", "--header", "--continue", "--resume", "--workspace",
        "-w", "--worktree", "--worktree-base", "--skip-worktree-setup",
    ],
    dropped_option_prefixes: &[
        "--api-key=", "--header=", "-H=", "--resume=", "--workspace=",
        "--worktree=", "--worktree-base=",
    ],
    reject_options: &[
        "--cloud", "--output-format", "--print", "-p", "--stream-partial-output",
    ],
    resume_subcommand: Some("resume"),
    preserve_first_positional: false,
    skip_claude_hook_settings: false,
};

static OPENCODE_POLICY: Policy = Policy {
    value_options: &[
        "--log-level", "--port", "--hostname", "--mdns-domain", "--cors",
        "--file", "-f", "--model", "-m", "--session", "-s", "--prompt",
        "--agent",
    ],
    optional_value_options: EMPTY,
    variadic_options: &["--cors"],
    non_restorable_commands: &[
        "completion", "acp", "mcp", "attach", "run", "debug", "providers",
        "auth", "agent", "upgrade", "uninstall", "serve", "web", "models",
        "stats", "export", "import", "pr", "github", "session", "plugin",
        "plug", "db",
    ],
    dropped_options: &[
        "--continue", "-c", "--file", "-f", "--fork", "--session", "-s",
        "--prompt",
    ],
    dropped_option_prefixes: &[
        "--file=", "-f=", "--fork=", "--session=", "--prompt=",
    ],
    reject_options: EMPTY,
    resume_subcommand: None,
    preserve_first_positional: true,
    skip_claude_hook_settings: false,
};

/// Derived from notagent's CLI (`notagent_main/src/cli.rs:15-74`):
/// keep `--agent`/`--aid` (model rides on the agent profile), `--yolo`,
/// `--verbose`; drop session/one-shot plumbing. notagent has no `--model`
/// flag. Subcommand launches never fire SessionStart, so an unknown
/// positional simply ends the scan.
static NOTAGENT_POLICY: Policy = Policy {
    value_options: &[
        "--conversation", "--conversation-id", "--cid", "--directory", "-C",
        "--sandbox", "--agent", "--aid", "--event", "-e",
    ],
    optional_value_options: EMPTY,
    variadic_options: EMPTY,
    non_restorable_commands: EMPTY,
    dropped_options: &[
        "--conversation", "--conversation-id", "--cid", "--directory", "-C",
        "--sandbox", "--event", "-e",
    ],
    dropped_option_prefixes: &[
        "--conversation=", "--conversation-id=", "--cid=", "--directory=",
        "--sandbox=", "--event=",
    ],
    reject_options: &["--prompt", "-p"],
    resume_subcommand: None,
    preserve_first_positional: false,
    skip_claude_hook_settings: false,
};

// Runtime/interpreter flags that may appear in captured argv but are not
// portable session options.
fn runtime_only_option_width(arg: &str) -> Option<usize> {
    if arg == "--use-system-ca" {
        return Some(1);
    }
    arg.split_once('=')
        .filter(|(name, _)| *name == "--use-system-ca")
        .map(|_| 1)
}

/// The executable names an agent kind runs as — the binaries our own resume
/// commands invoke, plus the alternates antigravity and pi ship under. Used to
/// validate a captured argv before its flags are
/// preserved — the recorded pid may have been reused by an unrelated
/// process by the time it is read.
fn kind_executable_names(kind: &str) -> &'static [&'static str] {
    match kind {
        "claude" => &["claude"],
        "codex" => &["codex"],
        "notagent" => &["notagent"],
        "opencode" => &["opencode"],
        "pi" => &["pi"],
        "cursor" => &["cursor-agent", "cursor"],
        "antigravity" => &["agy", "antigravity"],
        _ => &[],
    }
}

/// The argument tail of a captured argv when it plausibly belongs to `kind`:
/// the agent executable may be argv[0] or — for interpreter launches
/// (node/bun script) — argv[1]. `None` when the argv does not look like this
/// agent at all.
pub fn agent_argv_tail<'a>(kind: &str, argv: &'a [String]) -> Option<&'a [String]> {
    let names = kind_executable_names(kind);
    for index in 0..argv.len().min(2) {
        let file_name = std::path::Path::new(&argv[index])
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if names.contains(&file_name) {
            return Some(&argv[index + 1..]);
        }
    }
    None
}

/// The restorable subset of a captured agent argv tail (everything after the
/// executable). `None` means the launch shape cannot be replayed; the caller
/// then resumes with the plain default command.
pub fn preserved_launch_args(kind: &str, tail: &[String]) -> Option<Vec<String>> {
    match kind {
        "claude" => preserve_options(tail, &CLAUDE_POLICY),
        "codex" => preserve_options(tail, &CODEX_POLICY),
        "cursor" => {
            // `cursor agent …` and the cursor-agent binary share flags.
            let tail = strip_leading(tail, "agent");
            preserve_options(tail, &CURSOR_POLICY)
        }
        "opencode" => preserve_options(strip_opencode_internal(tail), &OPENCODE_POLICY),
        "pi" => preserve_options(tail, &PI_POLICY),
        "antigravity" => preserve_options(tail, &ANTIGRAVITY_POLICY),
        "notagent" => preserve_options(tail, &NOTAGENT_POLICY),
        _ => None,
    }
}

/// codex shows a blocking "Update available!" picker on promptless startup —
/// exactly the shape of `codex resume <id>` — so the check is suppressed
/// per-process unless the preserved args already set it explicitly.
pub fn codex_resume_config_overrides(preserved: &[String]) -> Vec<String> {
    if has_explicit_update_check_override(preserved) {
        return Vec::new();
    }
    vec!["-c".to_string(), "check_for_update_on_startup=false".to_string()]
}

fn has_explicit_update_check_override(arguments: &[String]) -> bool {
    for (index, argument) in arguments.iter().enumerate() {
        if (argument == "-c" || argument == "--config")
            && arguments
                .get(index + 1)
                .is_some_and(|v| v.starts_with("check_for_update_on_startup="))
        {
            return true;
        }
        if argument.starts_with("-c=check_for_update_on_startup=")
            || argument.starts_with("--config=check_for_update_on_startup=")
        {
            return true;
        }
    }
    false
}

fn strip_leading<'a>(tail: &'a [String], word: &str) -> &'a [String] {
    match tail.first() {
        Some(first) if first == word => &tail[1..],
        _ => tail,
    }
}

/// opencode's bun bundle re-execs itself with internal arguments; strip
/// `tui-settings` and the `$bunfs` worker path so they never reach a resume.
fn strip_opencode_internal(tail: &[String]) -> &[String] {
    let mut t = tail;
    while let Some(first) = t.first() {
        let normalized = first.replace('\\', "/");
        let internal = first == "tui-settings"
            || (normalized.contains("/$bunfs/") && normalized.ends_with("/tui/worker.js"));
        if !internal {
            break;
        }
        t = &t[1..];
    }
    t
}

// ── Core scan ──────────────────────────────────────────────────────────────

fn preserve_options(args: &[String], policy: &Policy) -> Option<Vec<String>> {
    let mut result: Vec<String> = Vec::new();
    let mut index = 0;
    let mut consumed_first_positional = false;
    let mut skipping_resume_positionals = false;

    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            break;
        }

        if !arg.starts_with('-') || arg == "-" {
            if let Some(sub) = policy.resume_subcommand
                && arg == sub
            {
                skipping_resume_positionals = true;
                index += 1;
                continue;
            }
            if skipping_resume_positionals {
                skipping_resume_positionals = false;
                index += 1;
                continue;
            }
            if policy.non_restorable_commands.contains(&arg.as_str()) {
                return None;
            }
            if policy.preserve_first_positional && !consumed_first_positional {
                result.push(arg.clone());
                consumed_first_positional = true;
                index += 1;
                continue;
            }
            break;
        }

        if should_drop_option(arg, policy.reject_options) {
            return None;
        }
        if policy.dropped_option_prefixes.iter().any(|p| arg.starts_with(p)) {
            index += 1;
            continue;
        }

        let runtime_only = runtime_only_option_width(arg);
        let width = runtime_only.unwrap_or_else(|| option_width(args, index, policy));
        if runtime_only.is_some() || should_drop_option(arg, policy.dropped_options) {
            index += width;
            continue;
        }

        if policy.skip_claude_hook_settings
            && let Some(replacement) = notmux_hook_settings_replacement(args, index)
        {
            result.extend(replacement);
            index += width;
            continue;
        }

        let end = (index + width).min(args.len());
        result.extend(args[index..end].iter().cloned());
        index += width;
    }

    Some(result)
}

fn should_drop_option(arg: &str, dropped: &[&str]) -> bool {
    if dropped.contains(&arg) {
        return true;
    }
    match arg.split_once('=') {
        Some((name, _)) => dropped.contains(&name),
        None => false,
    }
}

fn option_width(args: &[String], index: usize, policy: &Policy) -> usize {
    let arg = &args[index];
    if arg.contains('=') {
        return 1;
    }
    if policy.optional_value_options.contains(&arg.as_str()) {
        let Some(value) = args.get(index + 1) else {
            return 1;
        };
        let following = args.get(index + 2).map(|s| s.as_str());
        return if looks_like_optional_value(value, following) { 2 } else { 1 };
    }
    if !policy.value_options.contains(&arg.as_str()) || index + 1 >= args.len() {
        return 1;
    }
    if policy.variadic_options.contains(&arg.as_str()) {
        let mut end = index + 1;
        while end < args.len() && !args[end].starts_with('-') {
            end += 1;
        }
        return (end - index).max(1);
    }
    2
}

fn looks_like_optional_value(value: &str, following: Option<&str>) -> bool {
    if value.is_empty() || value.starts_with('-') || value.chars().any(char::is_whitespace) {
        return false;
    }
    following.is_none()
        || value.contains(',')
        || following.map(|f| f.starts_with('-')) == Some(true)
}

// ── notmux-injected claude `--settings` unwrapping ──────────────────────────
//
// The claude wrapper shim injects `--settings {hooks…, preferredNotifChannel}`
// (with the user's own --settings deep-merged in). The wrapper re-injects on
// resume, so the captured copy must be stripped back to the user's part,
// identified by our marker.

/// `None` — not our settings, handle normally. `Some(vec![])` — drop.
/// `Some(parts)` — replace with the user's remaining settings.
fn notmux_hook_settings_replacement(args: &[String], index: usize) -> Option<Vec<String>> {
    let arg = &args[index];
    if let Some(value) = arg.strip_prefix("--settings=") {
        return match settings_replacement_value(value)? {
            None => Some(Vec::new()),
            Some(user) => Some(vec![format!("--settings={user}")]),
        };
    }
    if arg != "--settings" {
        return None;
    }
    let value = args.get(index + 1)?;
    match settings_replacement_value(value)? {
        None => Some(Vec::new()),
        Some(user) => Some(vec!["--settings".to_string(), user]),
    }
}

/// `None` — not a notmux hook-settings value. `Some(None)` — entirely ours,
/// drop. `Some(Some(json))` — mixed, keep the user's remainder.
fn settings_replacement_value(value: &str) -> Option<Option<String>> {
    if let Ok(serde_json::Value::Object(mut object)) = serde_json::from_str(value) {
        let ours = object
            .get("preferredNotifChannel")
            .and_then(|v| v.as_str())
            == Some("notifications_disabled")
            || object
                .get("hooks")
                .map(|h| json_contains_marker(h, "notmux"))
                .unwrap_or(false);
        if !ours {
            return None;
        }
        object.remove("hooks");
        object.remove("preferredNotifChannel");
        if object.is_empty() {
            return Some(None);
        }
        return Some(Some(serde_json::Value::Object(object).to_string()));
    }
    if value.contains("notmux") { Some(None) } else { None }
}

fn json_contains_marker(value: &serde_json::Value, marker: &str) -> bool {
    match value {
        serde_json::Value::String(s) => s.contains(marker),
        serde_json::Value::Array(items) => items.iter().any(|v| json_contains_marker(v, marker)),
        serde_json::Value::Object(map) => map.values().any(|v| json_contains_marker(v, marker)),
        _ => false,
    }
}

// ── Process argv capture ────────────────────────────────────────────────────

/// The full argv of a live process, or `None` when it cannot be read.
/// Exact (NUL-separated) on Linux and macOS — never parsed from `ps` output.
#[cfg(target_os = "linux")]
pub fn capture_process_argv(pid: u32) -> Option<Vec<String>> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let args: Vec<String> = raw
        .split(|b| *b == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).to_string())
        .collect();
    if args.is_empty() { None } else { Some(args) }
}

#[cfg(target_os = "macos")]
pub fn capture_process_argv(pid: u32) -> Option<Vec<String>> {
    if pid == 0 || pid > i32::MAX as u32 {
        return None;
    }
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    let mut size: libc::size_t = 0;
    unsafe {
        if libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        ) != 0
        {
            return None;
        }
        let mut buf = vec![0u8; size];
        if libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            buf.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        ) != 0
        {
            return None;
        }
        buf.truncate(size);
        parse_procargs2(&buf)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn capture_process_argv(_pid: u32) -> Option<Vec<String>> {
    None
}

/// KERN_PROCARGS2 layout: `argc` (native i32), the exec path (NUL-terminated),
/// NUL padding, then `argc` NUL-terminated argv strings.
#[cfg(any(target_os = "macos", test))]
fn parse_procargs2(buf: &[u8]) -> Option<Vec<String>> {
    let argc = i32::from_ne_bytes(buf.get(0..4)?.try_into().ok()?);
    if argc <= 0 {
        return None;
    }
    let argc = argc as usize;
    let mut i = 4;
    while i < buf.len() && buf[i] != 0 {
        i += 1;
    }
    while i < buf.len() && buf[i] == 0 {
        i += 1;
    }
    let mut args = Vec::with_capacity(argc);
    while args.len() < argc && i < buf.len() {
        let start = i;
        while i < buf.len() && buf[i] != 0 {
            i += 1;
        }
        args.push(String::from_utf8_lossy(&buf[start..i]).to_string());
        i += 1;
    }
    if args.len() == argc { Some(args) } else { None }
}

// Keep the unused-field lint quiet for `Policy::base` on non-test builds.
#[allow(dead_code)]
fn _policy_base_is_available() -> Policy {
    Policy::base()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn claude_keeps_model_and_permission_mode_drops_session_plumbing() {
        let tail = args(&[
            "--session-id", "abc", "--model", "opus", "--permission-mode",
            "acceptEdits", "--resume", "old-id", "--fork-session",
        ]);
        assert_eq!(
            preserved_launch_args("claude", &tail).unwrap(),
            args(&["--model", "opus", "--permission-mode", "acceptEdits"])
        );
    }

    #[test]
    fn claude_variadic_add_dir_preserved() {
        let tail = args(&["--add-dir", "/a", "/b", "--model", "opus"]);
        assert_eq!(
            preserved_launch_args("claude", &tail).unwrap(),
            args(&["--add-dir", "/a", "/b", "--model", "opus"])
        );
    }

    #[test]
    fn claude_print_and_commands_are_non_restorable() {
        assert!(preserved_launch_args("claude", &args(&["-p", "hi"])).is_none());
        assert!(preserved_launch_args("claude", &args(&["mcp", "list"])).is_none());
        // Unknown positional (a prompt) ends the scan but keeps earlier flags.
        assert_eq!(
            preserved_launch_args("claude", &args(&["--model", "opus", "do things"])).unwrap(),
            args(&["--model", "opus"])
        );
    }

    #[test]
    fn claude_injected_hook_settings_are_stripped() {
        let injected = r#"{"preferredNotifChannel":"notifications_disabled","hooks":{"Stop":[]}}"#;
        let tail = args(&["--settings", injected, "--model", "opus"]);
        assert_eq!(
            preserved_launch_args("claude", &tail).unwrap(),
            args(&["--model", "opus"])
        );
        // A merged user key survives as the user's own --settings.
        let merged = r#"{"preferredNotifChannel":"notifications_disabled","hooks":{},"theme":"dark"}"#;
        let tail = args(&["--settings", merged]);
        assert_eq!(
            preserved_launch_args("claude", &tail).unwrap(),
            args(&["--settings", r#"{"theme":"dark"}"#])
        );
        // A user's own settings file path is preserved untouched.
        let tail = args(&["--settings", "~/my.json", "--model", "opus"]);
        assert_eq!(
            preserved_launch_args("claude", &tail).unwrap(),
            args(&["--settings", "~/my.json", "--model", "opus"])
        );
    }

    #[test]
    fn codex_keeps_model_skips_resume_positional_rejects_exec() {
        let tail = args(&["resume", "0199aaaa-bbbb", "-m", "gpt-5.3-codex", "--last"]);
        assert_eq!(
            preserved_launch_args("codex", &tail).unwrap(),
            args(&["-m", "gpt-5.3-codex"])
        );
        assert!(preserved_launch_args("codex", &args(&["exec", "ls"])).is_none());
        // Variadic --image is dropped with all its values.
        let tail = args(&["--image", "a.png", "b.png", "-m", "gpt-5.3-codex"]);
        assert_eq!(
            preserved_launch_args("codex", &tail).unwrap(),
            args(&["-m", "gpt-5.3-codex"])
        );
    }

    #[test]
    fn codex_update_check_override_injection() {
        assert_eq!(
            codex_resume_config_overrides(&args(&["-m", "x"])),
            args(&["-c", "check_for_update_on_startup=false"])
        );
        assert!(codex_resume_config_overrides(&args(&[
            "-c",
            "check_for_update_on_startup=true"
        ]))
        .is_empty());
        assert!(codex_resume_config_overrides(&args(&[
            "--config=check_for_update_on_startup=false"
        ]))
        .is_empty());
    }

    #[test]
    fn cursor_strips_agent_and_resume_value() {
        let tail = args(&["agent", "--resume", "conv-1", "--model", "sonnet"]);
        assert_eq!(
            preserved_launch_args("cursor", &tail).unwrap(),
            args(&["--model", "sonnet"])
        );
        assert!(preserved_launch_args("cursor", &args(&["login"])).is_none());
    }

    #[test]
    fn opencode_keeps_first_positional_and_model() {
        let tail = args(&["/my/project", "--model", "anthropic/claude", "--session", "s1"]);
        assert_eq!(
            preserved_launch_args("opencode", &tail).unwrap(),
            args(&["/my/project", "--model", "anthropic/claude"])
        );
        assert!(preserved_launch_args("opencode", &args(&["run", "x"])).is_none());
    }

    #[test]
    fn pi_and_antigravity_policies() {
        assert_eq!(
            preserved_launch_args("pi", &args(&["--model", "m1", "--session", "s"])).unwrap(),
            args(&["--model", "m1"])
        );
        assert!(preserved_launch_args("pi", &args(&["--print"])).is_none());
        assert_eq!(
            preserved_launch_args("antigravity", &args(&["--conversation", "c1", "--sandbox", "on"]))
                .unwrap(),
            args(&["--sandbox", "on"])
        );
        assert!(preserved_launch_args("antigravity", &args(&["-p", "x"])).is_none());
    }

    #[test]
    fn notagent_keeps_agent_profile_and_yolo() {
        let tail = args(&["--agent", "scout", "--yolo", "--cid", "old", "-C", "/tmp"]);
        assert_eq!(
            preserved_launch_args("notagent", &tail).unwrap(),
            args(&["--agent", "scout", "--yolo"])
        );
        assert!(preserved_launch_args("notagent", &args(&["-p", "do it"])).is_none());
    }

    #[test]
    fn unknown_kind_yields_none() {
        assert!(preserved_launch_args("kimi", &args(&["--model", "x"])).is_none());
    }

    #[test]
    fn argv_tail_requires_matching_executable() {
        // Direct launch.
        let argv = args(&["/opt/homebrew/bin/claude", "--model", "opus"]);
        assert_eq!(
            agent_argv_tail("claude", &argv).unwrap(),
            &args(&["--model", "opus"])[..]
        );
        // Interpreter launch: the script is argv[1].
        let argv = args(&["node", "/usr/local/lib/claude", "--model", "opus"]);
        assert_eq!(
            agent_argv_tail("claude", &argv).unwrap(),
            &args(&["--model", "opus"])[..]
        );
        // A reused pid pointing at an unrelated process yields nothing.
        let argv = args(&["/bin/zsh", "-c", "some script"]);
        assert!(agent_argv_tail("codex", &argv).is_none());
        assert!(agent_argv_tail("claude", &args(&[])).is_none());
    }

    #[test]
    fn procargs2_parse() {
        // argc=2, exec path, padding, "claude\0--resume\0"
        let mut buf = Vec::new();
        buf.extend_from_slice(&2i32.to_ne_bytes());
        buf.extend_from_slice(b"/bin/claude\0\0\0");
        buf.extend_from_slice(b"claude\0--resume\0");
        assert_eq!(
            parse_procargs2(&buf).unwrap(),
            args(&["claude", "--resume"])
        );
        assert!(parse_procargs2(&[0, 0]).is_none());
    }
}
