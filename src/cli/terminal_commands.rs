//! First-class CLI subcommands for terminal and workspace control.
//!
//! These wrap the existing remote `ActionRequest` API (everything here can
//! also be done through `notmux action <json>`): list projects/terminals,
//! send text or keys, run commands, split panes, focus terminals, create
//! terminals, read pane content, and add projects.

use notmux_core::api::{ApiLayoutNode, ApiProject, StateResponse};

use crate::cli::{api_get, api_post, ensure_token};

// ── State helpers (pure over StateResponse, unit-tested below) ──────────────

/// Recursively collect all terminal ids in a layout, in visual order.
fn collect_terminal_ids(node: &ApiLayoutNode, out: &mut Vec<String>) {
    match node {
        ApiLayoutNode::Terminal { terminal_id, .. } => {
            // Uninitialized terminals (no PTY yet) have no id.
            if let Some(id) = terminal_id {
                out.push(id.clone());
            }
        }
        ApiLayoutNode::Split { children, .. } | ApiLayoutNode::Tabs { children, .. } => {
            for child in children {
                collect_terminal_ids(child, out);
            }
        }
    }
}

/// Layout-tree path (child indices, root = `[]`) of a terminal — the same
/// path representation `ActionRequest::SplitTerminal` expects.
fn find_terminal_path(node: &ApiLayoutNode, target: &str) -> Option<Vec<usize>> {
    match node {
        ApiLayoutNode::Terminal { terminal_id, .. } => {
            (terminal_id.as_deref() == Some(target)).then(Vec::new)
        }
        ApiLayoutNode::Split { children, .. } | ApiLayoutNode::Tabs { children, .. } => {
            for (i, child) in children.iter().enumerate() {
                if let Some(mut path) = find_terminal_path(child, target) {
                    path.insert(0, i);
                    return Some(path);
                }
            }
            None
        }
    }
}

/// The project containing a terminal id.
fn find_project_for_terminal<'a>(
    state: &'a StateResponse,
    terminal_id: &str,
) -> Option<&'a ApiProject> {
    state.projects.iter().find(|p| {
        p.layout.as_ref().is_some_and(|l| {
            let mut ids = Vec::new();
            collect_terminal_ids(l, &mut ids);
            ids.iter().any(|id| id == terminal_id)
        })
    })
}

/// Resolve a project by id, name, or path; `None` needle falls back to the
/// focused project, or the only project when there is exactly one.
fn resolve_project<'a>(
    state: &'a StateResponse,
    needle: Option<&str>,
) -> Result<&'a ApiProject, String> {
    match needle {
        Some(n) => state
            .projects
            .iter()
            .find(|p| p.id == n || p.name == n || p.path == n)
            .ok_or_else(|| format!("project not found: {n}")),
        None => {
            if let Some(focused) = state
                .focused_project_id
                .as_ref()
                .and_then(|id| state.projects.iter().find(|p| &p.id == id))
            {
                return Ok(focused);
            }
            if state.projects.len() == 1 {
                return Ok(&state.projects[0]);
            }
            Err("multiple projects — specify one by id, name, or path".to_string())
        }
    }
}

/// Map a user-facing key name to the `SpecialKey` variant string the API
/// expects (serde serializes the enum by variant name).
fn parse_special_key(name: &str) -> Option<&'static str> {
    Some(match name.to_ascii_lowercase().as_str() {
        "enter" | "return" => "Enter",
        "escape" | "esc" => "Escape",
        "ctrl-c" | "ctrlc" | "c-c" => "CtrlC",
        "ctrl-d" | "ctrld" | "c-d" => "CtrlD",
        "ctrl-z" | "ctrlz" | "c-z" => "CtrlZ",
        "tab" => "Tab",
        "up" | "arrow-up" => "ArrowUp",
        "down" | "arrow-down" => "ArrowDown",
        "left" | "arrow-left" => "ArrowLeft",
        "right" | "arrow-right" => "ArrowRight",
        "home" => "Home",
        "end" => "End",
        "page-up" | "pageup" => "PageUp",
        "page-down" | "pagedown" => "PageDown",
        "backspace" => "Backspace",
        "delete" | "del" => "Delete",
        _ => return None,
    })
}

/// Map a split direction word to the API's `SplitDirection`. `right` places
/// the new pane beside the current one (horizontal split), `down` below it
/// (vertical split).
fn parse_split_direction(word: &str) -> Option<&'static str> {
    Some(match word.to_ascii_lowercase().as_str() {
        "right" | "horizontal" | "h" => "horizontal",
        "down" | "vertical" | "v" => "vertical",
        _ => return None,
    })
}

// ── Arg parsing ─────────────────────────────────────────────────────────────

/// Split args into positionals and the values of `--terminal`, `--name`;
/// also reports the presence of `--json` and `--enter`.
struct ParsedArgs {
    positional: Vec<String>,
    terminal: Option<String>,
    name: Option<String>,
    json: bool,
    enter: bool,
}

fn parse_args(args: &[String]) -> ParsedArgs {
    let mut out = ParsedArgs {
        positional: Vec::new(),
        terminal: None,
        name: None,
        json: false,
        enter: false,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--terminal" | "--terminal-id" | "-t" => {
                i += 1;
                out.terminal = args.get(i).cloned();
            }
            "--name" => {
                i += 1;
                out.name = args.get(i).cloned();
            }
            "--json" => out.json = true,
            "--enter" => out.enter = true,
            other => out.positional.push(other.to_string()),
        }
        i += 1;
    }
    out
}

// ── Server plumbing ─────────────────────────────────────────────────────────

fn fetch_state(token: &str) -> Result<StateResponse, String> {
    let raw = api_get("/v1/state", token)?;
    serde_json::from_str(&raw).map_err(|e| format!("Failed to parse state: {e}"))
}

fn post_action(token: &str, payload: &serde_json::Value) -> Result<String, String> {
    api_post("/v1/actions", token, &payload.to_string())
}

/// The target terminal: explicit `--terminal` beats the `NOTMUX_TERMINAL_ID`
/// environment (set inside every notmux terminal).
fn resolve_terminal_id(explicit: Option<String>) -> Result<String, String> {
    if let Some(t) = explicit.filter(|s| !s.is_empty()) {
        return Ok(t);
    }
    std::env::var("NOTMUX_TERMINAL_ID")
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            "no target terminal: pass --terminal <id> or run inside a NotMux terminal".to_string()
        })
}

fn fail(e: String) -> i32 {
    eprintln!("{e}");
    1
}

// ── Subcommands ─────────────────────────────────────────────────────────────

pub fn cli_projects(args: &[String]) -> i32 {
    let parsed = parse_args(args);
    let token = match ensure_token() {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    let state = match fetch_state(&token) {
        Ok(s) => s,
        Err(e) => return fail(e),
    };
    if parsed.json {
        let rows: Vec<_> = state
            .projects
            .iter()
            .map(|p| {
                serde_json::json!({
                    "id": p.id,
                    "name": p.name,
                    "path": p.path,
                    "focused": state.focused_project_id.as_deref() == Some(p.id.as_str()),
                })
            })
            .collect();
        println!("{}", serde_json::json!(rows));
    } else {
        for p in &state.projects {
            let focused = if state.focused_project_id.as_deref() == Some(p.id.as_str()) {
                "*"
            } else {
                ""
            };
            println!("{}\t{}\t{}\t{}", p.id, p.name, p.path, focused);
        }
    }
    0
}

pub fn cli_terminals(args: &[String]) -> i32 {
    let parsed = parse_args(args);
    let token = match ensure_token() {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    let state = match fetch_state(&token) {
        Ok(s) => s,
        Err(e) => return fail(e),
    };
    let projects: Vec<&ApiProject> = match parsed.positional.first() {
        Some(needle) => match resolve_project(&state, Some(needle)) {
            Ok(p) => vec![p],
            Err(e) => return fail(e),
        },
        None => state.projects.iter().collect(),
    };
    let mut rows = Vec::new();
    for p in projects {
        let mut ids = Vec::new();
        if let Some(layout) = &p.layout {
            collect_terminal_ids(layout, &mut ids);
        }
        for id in ids {
            let name = p.terminal_names.get(&id).cloned().unwrap_or_default();
            let runtime = p.terminal_states.get(&id).cloned().unwrap_or_default();
            rows.push((id, name, p.id.clone(), p.name.clone(), runtime));
        }
    }
    if parsed.json {
        let json_rows: Vec<_> = rows
            .iter()
            .map(|(id, name, pid, pname, rt)| {
                serde_json::json!({
                    "terminal_id": id,
                    "name": name,
                    "project_id": pid,
                    "project_name": pname,
                    "agent_state": rt.agent_state,
                    "agent_kind": rt.agent_kind,
                    "notification": rt.notification,
                })
            })
            .collect();
        println!("{}", serde_json::json!(json_rows));
    } else {
        for (id, name, _pid, pname, rt) in rows {
            let state = rt
                .agent_state
                .map(|s| s.as_str())
                .unwrap_or("-");
            println!("{}\t{}\t{}\t{}", id, name, pname, state);
        }
    }
    0
}

pub fn cli_send(args: &[String]) -> i32 {
    let parsed = parse_args(args);
    if parsed.positional.is_empty() {
        eprintln!("Usage: notmux send <text> [--terminal <id>] [--enter]");
        return 1;
    }
    let text = parsed.positional.join(" ");
    let token = match ensure_token() {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    let tid = match resolve_terminal_id(parsed.terminal) {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    let payload = serde_json::json!({
        "action": "send_text",
        "terminal_id": tid,
        "text": text,
    });
    if let Err(e) = post_action(&token, &payload) {
        return fail(e);
    }
    if parsed.enter {
        let enter = serde_json::json!({
            "action": "send_special_key",
            "terminal_id": tid,
            "key": "Enter",
        });
        if let Err(e) = post_action(&token, &enter) {
            return fail(e);
        }
    }
    0
}

pub fn cli_run(args: &[String]) -> i32 {
    let parsed = parse_args(args);
    if parsed.positional.is_empty() {
        eprintln!("Usage: notmux run <command> [--terminal <id>]");
        return 1;
    }
    let command = parsed.positional.join(" ");
    let token = match ensure_token() {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    let tid = match resolve_terminal_id(parsed.terminal) {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    let payload = serde_json::json!({
        "action": "run_command",
        "terminal_id": tid,
        "command": command,
    });
    match post_action(&token, &payload) {
        Ok(_) => 0,
        Err(e) => fail(e),
    }
}

pub fn cli_key(args: &[String]) -> i32 {
    let parsed = parse_args(args);
    let Some(key_name) = parsed.positional.first() else {
        eprintln!("Usage: notmux key <key> [--terminal <id>]");
        eprintln!(
            "Keys: enter, escape, tab, up, down, left, right, home, end, \
             page-up, page-down, backspace, delete, ctrl-c, ctrl-d, ctrl-z"
        );
        return 1;
    };
    let Some(key) = parse_special_key(key_name) else {
        eprintln!("Unknown key: {key_name}");
        return 1;
    };
    let token = match ensure_token() {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    let tid = match resolve_terminal_id(parsed.terminal) {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    let payload = serde_json::json!({
        "action": "send_special_key",
        "terminal_id": tid,
        "key": key,
    });
    match post_action(&token, &payload) {
        Ok(_) => 0,
        Err(e) => fail(e),
    }
}

pub fn cli_split(args: &[String]) -> i32 {
    let parsed = parse_args(args);
    let direction_word = parsed.positional.first().map(|s| s.as_str()).unwrap_or("right");
    let Some(direction) = parse_split_direction(direction_word) else {
        eprintln!("Usage: notmux split [right|down] [--terminal <id>]");
        return 1;
    };
    let token = match ensure_token() {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    let tid = match resolve_terminal_id(parsed.terminal) {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    let state = match fetch_state(&token) {
        Ok(s) => s,
        Err(e) => return fail(e),
    };
    let Some(project) = find_project_for_terminal(&state, &tid) else {
        return fail(format!("terminal not found in any project: {tid}"));
    };
    let Some(path) = project
        .layout
        .as_ref()
        .and_then(|l| find_terminal_path(l, &tid))
    else {
        return fail(format!("terminal not found in layout: {tid}"));
    };
    let payload = serde_json::json!({
        "action": "split_terminal",
        "project_id": project.id,
        "path": path,
        "direction": direction,
    });
    match post_action(&token, &payload) {
        Ok(_) => 0,
        Err(e) => fail(e),
    }
}

pub fn cli_focus(args: &[String]) -> i32 {
    let parsed = parse_args(args);
    let tid = match parsed
        .positional
        .first()
        .cloned()
        .or(parsed.terminal)
        .ok_or_else(|| "Usage: notmux focus <terminal-id>".to_string())
    {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    let token = match ensure_token() {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    let state = match fetch_state(&token) {
        Ok(s) => s,
        Err(e) => return fail(e),
    };
    let Some(project) = find_project_for_terminal(&state, &tid) else {
        return fail(format!("terminal not found in any project: {tid}"));
    };
    let payload = serde_json::json!({
        "action": "focus_terminal",
        "project_id": project.id,
        "terminal_id": tid,
    });
    match post_action(&token, &payload) {
        Ok(_) => 0,
        Err(e) => fail(e),
    }
}

pub fn cli_new_terminal(args: &[String]) -> i32 {
    let parsed = parse_args(args);
    let token = match ensure_token() {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    let state = match fetch_state(&token) {
        Ok(s) => s,
        Err(e) => return fail(e),
    };
    let project = match resolve_project(&state, parsed.positional.first().map(|s| s.as_str())) {
        Ok(p) => p,
        Err(e) => return fail(e),
    };
    let payload = serde_json::json!({
        "action": "create_terminal",
        "project_id": project.id,
    });
    match post_action(&token, &payload) {
        Ok(resp) => {
            if !resp.is_empty() {
                println!("{resp}");
            }
            0
        }
        Err(e) => fail(e),
    }
}

pub fn cli_read(args: &[String]) -> i32 {
    let parsed = parse_args(args);
    let token = match ensure_token() {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    let tid = match resolve_terminal_id(parsed.terminal.or_else(|| parsed.positional.first().cloned())) {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    let payload = serde_json::json!({
        "action": "read_content",
        "terminal_id": tid,
    });
    match post_action(&token, &payload) {
        Ok(resp) => {
            if parsed.json {
                println!("{resp}");
            } else {
                // Unwrap {"content": "..."} for plain-text piping.
                match serde_json::from_str::<serde_json::Value>(&resp) {
                    Ok(v) => println!(
                        "{}",
                        v.get("content").and_then(|c| c.as_str()).unwrap_or(&resp)
                    ),
                    Err(_) => println!("{resp}"),
                }
            }
            0
        }
        Err(e) => fail(e),
    }
}

// ── Wait primitives ─────────────────────────────────────────────────────────

/// Parsed `notmux wait` options.
#[derive(Debug, PartialEq)]
struct WaitOpts {
    terminal: Option<String>,
    /// Accepted lifecycle states; `None` entries mean "no agent" (`--until
    /// exited`).
    until: Vec<Option<notmux_core::agent_state::AgentState>>,
    timeout_ms: Option<u64>,
    interval_ms: u64,
}

const WAIT_USAGE: &str = "Usage: notmux wait [--terminal <id>] [--until <state>[,<state>…]]… [--timeout <ms>] [--interval <ms>]\n  Block until the terminal's agent reaches one of the states.\n  States: working, blocked, done, idle, unknown, exited (no agent).\n  Default --until: blocked, done, idle. No timeout by default; interval 500 ms.\n  Prints a JSON line on success; exit 1 on timeout or when the terminal is gone.";

fn parse_wait_args(args: &[String]) -> Result<WaitOpts, String> {
    use notmux_core::agent_state::AgentState;
    let mut opts = WaitOpts {
        terminal: None,
        until: Vec::new(),
        timeout_ms: None,
        interval_ms: 500,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--terminal" | "--terminal-id" | "-t" => {
                i += 1;
                opts.terminal = args.get(i).cloned();
            }
            "--until" | "-u" => {
                i += 1;
                let Some(spec) = args.get(i) else {
                    return Err(WAIT_USAGE.to_string());
                };
                for part in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    match part {
                        "exited" | "none" | "gone" => opts.until.push(None),
                        other => match AgentState::parse(other) {
                            Some(s) => opts.until.push(Some(s)),
                            None => return Err(format!("unknown state '{other}'\n{WAIT_USAGE}")),
                        },
                    }
                }
            }
            "--timeout" => {
                i += 1;
                opts.timeout_ms = Some(
                    args.get(i)
                        .and_then(|s| s.parse::<u64>().ok())
                        .ok_or_else(|| format!("--timeout needs milliseconds\n{WAIT_USAGE}"))?,
                );
            }
            "--interval" => {
                i += 1;
                opts.interval_ms = args
                    .get(i)
                    .and_then(|s| s.parse::<u64>().ok())
                    .filter(|ms| *ms > 0)
                    .ok_or_else(|| format!("--interval needs milliseconds > 0\n{WAIT_USAGE}"))?;
            }
            "--help" | "-h" => return Err(WAIT_USAGE.to_string()),
            other if !other.starts_with('-') && opts.terminal.is_none() => {
                opts.terminal = Some(other.to_string());
            }
            other => return Err(format!("unknown option '{other}'\n{WAIT_USAGE}")),
        }
        i += 1;
    }
    if opts.until.is_empty() {
        opts.until = vec![
            Some(AgentState::Blocked),
            Some(AgentState::Done),
            Some(AgentState::Idle),
        ];
    }
    Ok(opts)
}

fn json_error_exit(value: serde_json::Value) -> i32 {
    eprintln!("{value}");
    1
}

/// `notmux wait` — poll `/v1/state` until the target terminal's agent state
/// is one of the accepted ones. Returns immediately when it already matches.
pub fn cli_wait(args: &[String]) -> i32 {
    let opts = match parse_wait_args(args) {
        Ok(o) => o,
        Err(usage) => {
            eprintln!("{usage}");
            return 2;
        }
    };
    let token = match ensure_token() {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    let tid = match resolve_terminal_id(opts.terminal.clone()) {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    let start = std::time::Instant::now();
    loop {
        let state = match fetch_state(&token) {
            Ok(s) => s,
            Err(e) => return fail(e),
        };
        let Some(project) = find_project_for_terminal(&state, &tid) else {
            return json_error_exit(serde_json::json!({
                "error": "terminal_not_found",
                "terminal": tid,
                "elapsed_ms": start.elapsed().as_millis() as u64,
            }));
        };
        let runtime = project.terminal_states.get(&tid).cloned().unwrap_or_default();
        let last_state = runtime.agent_state;
        if opts.until.iter().any(|u| *u == runtime.agent_state) {
            println!(
                "{}",
                serde_json::json!({
                    "terminal": tid,
                    "project_id": project.id,
                    "state": runtime.agent_state,
                    "agent_kind": runtime.agent_kind,
                    "notification": runtime.notification,
                    "elapsed_ms": start.elapsed().as_millis() as u64,
                })
            );
            return 0;
        }
        if let Some(timeout) = opts.timeout_ms
            && start.elapsed().as_millis() as u64 >= timeout
        {
            return json_error_exit(serde_json::json!({
                "error": "timeout",
                "terminal": tid,
                "state": last_state,
                "elapsed_ms": start.elapsed().as_millis() as u64,
            }));
        }
        std::thread::sleep(std::time::Duration::from_millis(opts.interval_ms));
    }
}

/// What `wait-output` looks for on the screen.
#[derive(Debug)]
enum OutputMatcher {
    Literal(String),
    Regex(regex::Regex),
}

impl OutputMatcher {
    fn matches(&self, line: &str) -> bool {
        match self {
            OutputMatcher::Literal(s) => line.contains(s.as_str()),
            OutputMatcher::Regex(re) => re.is_match(line),
        }
    }
}

/// First matching line among the last `last_n` screen rows (all rows when
/// `None`), searched bottom-up so the newest occurrence wins. Returns the
/// zero-based row index within the screen and the row text.
fn find_screen_match(
    content: &str,
    matcher: &OutputMatcher,
    last_n: Option<usize>,
) -> Option<(usize, String)> {
    let lines: Vec<&str> = content.lines().collect();
    let start = last_n
        .map(|n| lines.len().saturating_sub(n))
        .unwrap_or(0);
    lines
        .iter()
        .enumerate()
        .skip(start)
        .rev()
        .find(|(_, l)| matcher.matches(l))
        .map(|(i, l)| (i, l.to_string()))
}

const WAIT_OUTPUT_USAGE: &str = "Usage: notmux wait-output [--terminal <id>] (<text> | --regex <pattern>) [--lines <n>] [--timeout <ms>] [--interval <ms>]\n  Block until the terminal's visible screen (not scrollback) contains the text or\n  matches the pattern on one line. Prints the matched line as JSON; exit 1 on timeout.";

#[derive(Debug)]
struct WaitOutputOpts {
    terminal: Option<String>,
    matcher: OutputMatcher,
    lines: Option<usize>,
    timeout_ms: Option<u64>,
    interval_ms: u64,
}

fn parse_wait_output_args(args: &[String]) -> Result<WaitOutputOpts, String> {
    let mut terminal = None;
    let mut literal: Option<String> = None;
    let mut pattern: Option<String> = None;
    let mut lines = None;
    let mut timeout_ms = None;
    let mut interval_ms = 500;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--terminal" | "--terminal-id" | "-t" => {
                i += 1;
                terminal = args.get(i).cloned();
            }
            "--regex" | "-r" => {
                i += 1;
                pattern = args.get(i).cloned();
            }
            "--lines" | "-n" => {
                i += 1;
                lines = Some(
                    args.get(i)
                        .and_then(|s| s.parse::<usize>().ok())
                        .filter(|n| *n > 0)
                        .ok_or_else(|| format!("--lines needs a number > 0\n{WAIT_OUTPUT_USAGE}"))?,
                );
            }
            "--timeout" => {
                i += 1;
                timeout_ms = Some(
                    args.get(i)
                        .and_then(|s| s.parse::<u64>().ok())
                        .ok_or_else(|| format!("--timeout needs milliseconds\n{WAIT_OUTPUT_USAGE}"))?,
                );
            }
            "--interval" => {
                i += 1;
                interval_ms = args
                    .get(i)
                    .and_then(|s| s.parse::<u64>().ok())
                    .filter(|ms| *ms > 0)
                    .ok_or_else(|| format!("--interval needs milliseconds > 0\n{WAIT_OUTPUT_USAGE}"))?;
            }
            "--help" | "-h" => return Err(WAIT_OUTPUT_USAGE.to_string()),
            other if !other.starts_with('-') && literal.is_none() => {
                literal = Some(other.to_string());
            }
            other => return Err(format!("unknown option '{other}'\n{WAIT_OUTPUT_USAGE}")),
        }
        i += 1;
    }
    let matcher = match (pattern, literal) {
        (Some(p), _) => OutputMatcher::Regex(
            regex::Regex::new(&p).map_err(|e| format!("invalid --regex: {e}"))?,
        ),
        (None, Some(l)) => OutputMatcher::Literal(l),
        (None, None) => return Err(WAIT_OUTPUT_USAGE.to_string()),
    };
    Ok(WaitOutputOpts {
        terminal,
        matcher,
        lines,
        timeout_ms,
        interval_ms,
    })
}

/// `notmux wait-output` — poll the terminal's visible screen until a line
/// matches. Text already on screen matches immediately.
pub fn cli_wait_output(args: &[String]) -> i32 {
    let opts = match parse_wait_output_args(args) {
        Ok(o) => o,
        Err(usage) => {
            eprintln!("{usage}");
            return 2;
        }
    };
    let token = match ensure_token() {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    let tid = match resolve_terminal_id(opts.terminal.clone()) {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    let payload = serde_json::json!({ "action": "read_content", "terminal_id": tid });
    let start = std::time::Instant::now();
    loop {
        let content = match post_action(&token, &payload) {
            Ok(resp) => serde_json::from_str::<serde_json::Value>(&resp)
                .ok()
                .and_then(|v| v.get("content").and_then(|c| c.as_str()).map(str::to_string))
                .unwrap_or(resp),
            Err(e) => return fail(e),
        };
        if let Some((row, line)) = find_screen_match(&content, &opts.matcher, opts.lines) {
            println!(
                "{}",
                serde_json::json!({
                    "terminal": tid,
                    "matched_line": line,
                    "row": row,
                    "elapsed_ms": start.elapsed().as_millis() as u64,
                })
            );
            return 0;
        }
        if let Some(timeout) = opts.timeout_ms
            && start.elapsed().as_millis() as u64 >= timeout
        {
            return json_error_exit(serde_json::json!({
                "error": "timeout",
                "terminal": tid,
                "elapsed_ms": start.elapsed().as_millis() as u64,
            }));
        }
        std::thread::sleep(std::time::Duration::from_millis(opts.interval_ms));
    }
}

pub fn cli_add_project(args: &[String]) -> i32 {
    let parsed = parse_args(args);
    let Some(path) = parsed.positional.first() else {
        eprintln!("Usage: notmux add-project <path> [--name <name>]");
        return 1;
    };
    let abs = match std::fs::canonicalize(path) {
        Ok(p) => p,
        Err(e) => return fail(format!("invalid path {path}: {e}")),
    };
    let name = parsed.name.unwrap_or_else(|| {
        abs.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| abs.display().to_string())
    });
    let token = match ensure_token() {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    let payload = serde_json::json!({
        "action": "add_project",
        "name": name,
        "path": abs.to_string_lossy(),
    });
    match post_action(&token, &payload) {
        Ok(_) => 0,
        Err(e) => fail(e),
    }
}

pub fn cli_events(args: &[String]) -> i32 {
    let mut count: usize = 20;
    let mut follow = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-n" | "--lines" => {
                i += 1;
                match args.get(i).map(|s| s.parse::<usize>()) {
                    Some(Ok(n)) => count = n,
                    _ => {
                        eprintln!("Usage: notmux events [-n <count>] [--follow]");
                        return 1;
                    }
                }
            }
            "--follow" | "-f" => follow = true,
            _ => {
                eprintln!("Usage: notmux events [-n <count>] [--follow]");
                return 1;
            }
        }
        i += 1;
    }

    let path = crate::event_log::event_log_path();
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    if content.is_empty() && !follow {
        eprintln!("No events logged yet ({}).", path.display());
        return 0;
    }
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(count);
    for line in &lines[start..] {
        println!("{line}");
    }

    if follow {
        // Poll for appended lines; a shrunk file means the log rotated —
        // start over from the beginning of the fresh file.
        let mut offset = content.len() as u64;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(200));
            let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            if len < offset {
                offset = 0;
            }
            if len > offset {
                use std::io::{Read, Seek, SeekFrom};
                let Ok(mut file) = std::fs::File::open(&path) else {
                    continue;
                };
                if file.seek(SeekFrom::Start(offset)).is_err() {
                    continue;
                }
                let mut buf = String::new();
                if file.read_to_string(&mut buf).is_err() {
                    continue;
                }
                offset = len;
                print!("{buf}");
                use std::io::Write;
                let _ = std::io::stdout().flush();
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_state() -> StateResponse {
        let json = serde_json::json!({
            "state_version": 1,
            "focused_project_id": "p2",
            "fullscreen_terminal": null,
            "projects": [
                {
                    "id": "p1",
                    "name": "alpha",
                    "path": "/tmp/alpha",
                    "show_in_overview": true,
                    "terminal_names": {"t1": "build"},
                    "layout": {
                        "type": "split",
                        "direction": "horizontal",
                        "sizes": [50.0, 50.0],
                        "children": [
                            {"type": "terminal", "terminal_id": "t1", "minimized": false, "detached": false},
                            {"type": "tabs", "active_tab": 0, "children": [
                                {"type": "terminal", "terminal_id": "t2", "minimized": false, "detached": false},
                                {"type": "terminal", "terminal_id": "t3", "minimized": false, "detached": false}
                            ]}
                        ]
                    }
                },
                {
                    "id": "p2",
                    "name": "beta",
                    "path": "/tmp/beta",
                    "show_in_overview": true,
                    "terminal_names": {},
                    "layout": {"type": "terminal", "terminal_id": "t9", "minimized": false, "detached": false}
                }
            ]
        });
        serde_json::from_value(json).expect("sample state must parse")
    }

    #[test]
    fn collects_terminals_in_visual_order() {
        let state = sample_state();
        let mut ids = Vec::new();
        collect_terminal_ids(state.projects[0].layout.as_ref().unwrap(), &mut ids);
        assert_eq!(ids, vec!["t1", "t2", "t3"]);
    }

    #[test]
    fn finds_layout_paths_through_splits_and_tabs() {
        let state = sample_state();
        let layout = state.projects[0].layout.as_ref().unwrap();
        assert_eq!(find_terminal_path(layout, "t1"), Some(vec![0]));
        assert_eq!(find_terminal_path(layout, "t3"), Some(vec![1, 1]));
        assert_eq!(find_terminal_path(layout, "missing"), None);
        // Root-level terminal has the empty path.
        let root = state.projects[1].layout.as_ref().unwrap();
        assert_eq!(find_terminal_path(root, "t9"), Some(vec![]));
    }

    #[test]
    fn finds_project_for_terminal() {
        let state = sample_state();
        assert_eq!(find_project_for_terminal(&state, "t2").unwrap().id, "p1");
        assert_eq!(find_project_for_terminal(&state, "t9").unwrap().id, "p2");
        assert!(find_project_for_terminal(&state, "nope").is_none());
    }

    #[test]
    fn resolves_projects_by_id_name_path_and_focus() {
        let state = sample_state();
        assert_eq!(resolve_project(&state, Some("p1")).unwrap().id, "p1");
        assert_eq!(resolve_project(&state, Some("alpha")).unwrap().id, "p1");
        assert_eq!(resolve_project(&state, Some("/tmp/beta")).unwrap().id, "p2");
        // No needle → focused project.
        assert_eq!(resolve_project(&state, None).unwrap().id, "p2");
        assert!(resolve_project(&state, Some("gamma")).is_err());
    }

    #[test]
    fn parses_special_keys() {
        assert_eq!(parse_special_key("enter"), Some("Enter"));
        assert_eq!(parse_special_key("Ctrl-C"), Some("CtrlC"));
        assert_eq!(parse_special_key("page-up"), Some("PageUp"));
        assert_eq!(parse_special_key("banana"), None);
    }

    #[test]
    fn parses_split_directions() {
        assert_eq!(parse_split_direction("right"), Some("horizontal"));
        assert_eq!(parse_split_direction("down"), Some("vertical"));
        assert_eq!(parse_split_direction("sideways"), None);
    }

    #[test]
    fn parse_args_separates_flags_and_positionals() {
        let args: Vec<String> = ["hello", "world", "--terminal", "t1", "--enter", "--json"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let parsed = parse_args(&args);
        assert_eq!(parsed.positional, vec!["hello", "world"]);
        assert_eq!(parsed.terminal.as_deref(), Some("t1"));
        assert!(parsed.enter);
        assert!(parsed.json);
    }

    #[test]
    fn wait_args_default_to_settled_states() {
        use notmux_core::agent_state::AgentState;
        let opts = parse_wait_args(&[]).unwrap();
        assert_eq!(
            opts.until,
            vec![
                Some(AgentState::Blocked),
                Some(AgentState::Done),
                Some(AgentState::Idle)
            ]
        );
        assert_eq!(opts.timeout_ms, None);
        assert_eq!(opts.interval_ms, 500);
    }

    #[test]
    fn wait_args_accept_lists_repeats_and_exited() {
        use notmux_core::agent_state::AgentState;
        let args: Vec<String> = ["-t", "t9", "--until", "blocked,done", "--until", "exited", "--timeout", "1500", "--interval", "50"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let opts = parse_wait_args(&args).unwrap();
        assert_eq!(opts.terminal.as_deref(), Some("t9"));
        assert_eq!(
            opts.until,
            vec![Some(AgentState::Blocked), Some(AgentState::Done), None]
        );
        assert_eq!(opts.timeout_ms, Some(1500));
        assert_eq!(opts.interval_ms, 50);
    }

    #[test]
    fn wait_args_reject_bad_state_and_options() {
        let args = vec!["--until".to_string(), "flying".to_string()];
        assert!(parse_wait_args(&args).unwrap_err().contains("unknown state"));
        let args = vec!["--bogus".to_string()];
        assert!(parse_wait_args(&args).unwrap_err().contains("unknown option"));
        let args = vec!["--timeout".to_string(), "soon".to_string()];
        assert!(parse_wait_args(&args).unwrap_err().contains("--timeout"));
    }

    #[test]
    fn wait_output_args_need_text_or_regex() {
        assert!(parse_wait_output_args(&[]).is_err());
        let args = vec!["--regex".to_string(), "[".to_string()];
        assert!(parse_wait_output_args(&args).unwrap_err().contains("invalid --regex"));
        let args: Vec<String> = ["passed", "-n", "5", "--timeout", "10"].iter().map(|s| s.to_string()).collect();
        let opts = parse_wait_output_args(&args).unwrap();
        assert!(matches!(opts.matcher, OutputMatcher::Literal(ref s) if s == "passed"));
        assert_eq!(opts.lines, Some(5));
        assert_eq!(opts.timeout_ms, Some(10));
    }

    #[test]
    fn screen_match_prefers_newest_row_within_window() {
        let content = "old: test result: ok\nbuilding\n\ntest result: FAILED\n\n";
        let lit = OutputMatcher::Literal("test result".into());
        assert_eq!(
            find_screen_match(content, &lit, None),
            Some((3, "test result: FAILED".to_string()))
        );
        // Only the last row (blank) → no match.
        assert_eq!(find_screen_match(content, &lit, Some(1)), None);
        let re = OutputMatcher::Regex(regex::Regex::new("passed|FAILED").unwrap());
        assert_eq!(find_screen_match(content, &re, Some(3)).map(|m| m.0), Some(3));
        assert_eq!(find_screen_match("nothing here", &re, None), None);
    }
}
