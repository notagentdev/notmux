//! `notmux agent …`: the CLI every orchestrated agent uses.
//!
//! Identity comes from `NOTMUX_RUN_ID` (set by the runtime for launched
//! processes) or `NOTMUX_TERMINAL_ID` (any NotMux pane). Every mutating
//! request carries a fresh request ID; a transport failure is retried with
//! the same ID, so a lost answer never duplicates a worker, message, or
//! result. Output is JSON on stdout; errors go to stderr with the stable
//! error code and exit status 1 (2 for a usage error).
//!
//! Messages delivered by a hook are acknowledged only after they were
//! printed: a hook that dies before printing leaves them unread, and a
//! duplicate delivery is the safe side.

use crate::cli::{discover_server, ensure_token};
use notmux_core::orchestration::*;
use std::time::Duration;

/// Seconds beyond the server-side wait a blocking request may take.
const WAIT_MARGIN_SECS: u64 = 15;
/// Ordinary requests; a spawn writes trust files and resolves an executable.
const REQUEST_TIMEOUT_SECS: u64 = 60;
/// Cooperative `next` without `--timeout`: below the 120 s default of
/// Claude Code's Bash tool, so the call returns before the tool gives up.
const COOPERATIVE_NEXT_TIMEOUT_MS: u64 = 100_000;
/// The Claude Stop hook has 600 s; leave room for the answer.
const STOP_HOOK_WAIT_MS: u64 = 540_000;
/// notagent caps hook stdout at 4000 UTF-16 code units; stay below it,
/// counted in the same units.
const NOTAGENT_HOOK_BUDGET_UNITS: usize = 3200;

// ── Argument parsing ───────────────────────────────────────────────────────

#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    positional: Vec<String>,
    flags: Vec<(String, Option<String>)>,
    /// Everything after `--`, verbatim.
    rest: Vec<String>,
}

const BOOL_FLAGS: [&str; 5] = ["unread", "drain", "json", "help", "claude-stop"];

fn parse(args: &[String]) -> Args {
    let mut out = Args::default();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--" {
            out.rest = args[i + 1..].to_vec();
            break;
        }
        if let Some(flag) = a.strip_prefix("--") {
            if let Some((k, v)) = flag.split_once('=') {
                out.flags.push((k.to_string(), Some(v.to_string())));
            } else if i + 1 < args.len() && !args[i + 1].starts_with("--") && !BOOL_FLAGS.contains(&flag) {
                out.flags.push((flag.to_string(), Some(args[i + 1].clone())));
                i += 1;
            } else {
                out.flags.push((flag.to_string(), None));
            }
        } else {
            out.positional.push(a.to_string());
        }
        i += 1;
    }
    out
}

impl Args {
    fn flag(&self, name: &str) -> Option<&str> {
        self.flags.iter().rev().find(|(k, _)| k == name).and_then(|(_, v)| v.as_deref())
    }
    fn has(&self, name: &str) -> bool {
        self.flags.iter().any(|(k, _)| k == name)
    }
    fn u64_flag(&self, name: &str) -> Result<Option<u64>, String> {
        self.flag(name)
            .map(|v| v.parse::<u64>().map_err(|_| format!("--{name} needs a number, got {v:?}")))
            .transpose()
    }
}

/// The options each verb accepts. Anything else is an error, never
/// silently ignored: `stop --target x` must not stop the whole group.
const ALLOWED_FLAGS: &[(&str, &[&str])] = &[
    ("register", &["harness", "budget", "run"]),
    ("lead", &["harness", "budget", "project", "cwd", "name", "executable", "run"]),
    ("spawn", &["name", "harness", "task", "task-file", "cwd", "executable", "completion", "run"]),
    ("assign", &["worker", "name", "task", "task-file", "run"]),
    ("list", &["run"]),
    ("get", &["run"]),
    ("task", &["run"]),
    ("tasks", &["worker", "after", "limit", "run"]),
    ("message", &["to", "body", "body-file", "run"]),
    ("inbox", &["unread", "after", "limit", "drain", "run"]),
    ("ack", &["run"]),
    ("wait", &["after", "timeout", "run"]),
    ("next", &["after", "timeout", "run"]),
    ("finish", &["task", "outcome", "result", "result-file", "run"]),
    ("stop", &["target", "run"]),
    ("cancel", &["target", "run"]),
    ("hook", &["event", "run"]),
    ("handoff", &["claude-stop", "run"]),
    ("skill", &[]),
    ("help", &["help"]),
];

fn check_flags(verb: &str, args: &Args) -> Result<(), String> {
    let Some((_, allowed)) = ALLOWED_FLAGS.iter().find(|(v, _)| *v == verb) else {
        return Err(format!("unknown agent command {verb:?}"));
    };
    for (flag, value) in &args.flags {
        if flag == "help" {
            continue;
        }
        if !allowed.contains(&flag.as_str()) {
            return Err(format!("unknown option --{flag} for `notmux agent {verb}`"));
        }
        // An option that takes a value must have one: `stop --target`
        // without an id must not turn into "stop the whole group".
        let is_bool = BOOL_FLAGS.contains(&flag.as_str());
        match value {
            None if !is_bool => return Err(format!("option --{flag} needs a value")),
            Some(v) if !is_bool && v.trim().is_empty() => return Err(format!("option --{flag} needs a value")),
            Some(_) if is_bool => return Err(format!("option --{flag} takes no value")),
            _ => {}
        }
    }
    // More than one target is ambiguous.
    if matches!(verb, "stop" | "cancel") && args.flag("target").is_some() && !args.positional.is_empty() {
        return Err(format!("`notmux agent {verb}` takes the target once: <run-id> or --target <run-id>"));
    }
    Ok(())
}

/// Text from `--<name>` or the file named by `--<name>-file` (`-` = stdin).
fn text_arg(args: &Args, name: &str) -> Result<String, String> {
    if let Some(t) = args.flag(name) {
        return Ok(t.to_string());
    }
    let file_flag = format!("{name}-file");
    if let Some(path) = args.flag(&file_flag) {
        return if path == "-" {
            use std::io::Read;
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s).map_err(|e| e.to_string())?;
            Ok(s)
        } else {
            std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))
        };
    }
    Err(format!("--{name} <text> or --{file_flag} <path> is required"))
}

fn caller(args: &Args) -> Caller {
    let run_id = args
        .flag("run")
        .map(str::to_string)
        .or_else(|| std::env::var(ENV_RUN_ID).ok())
        .filter(|s| !s.is_empty());
    let terminal_id = std::env::var("NOTMUX_TERMINAL_ID").ok().filter(|s| !s.is_empty());
    Caller { run_id, terminal_id }
}

fn harness_arg(args: &Args) -> Result<HarnessKind, String> {
    let name = args.flag("harness").ok_or("--harness <notagent|claude|codex|generic> is required")?;
    HarnessKind::parse(name).ok_or_else(|| format!("unknown harness {name:?}; use notagent, claude, codex or generic"))
}

fn harness() -> String {
    std::env::var(ENV_AGENT_HARNESS).unwrap_or_default()
}

fn print_usage() {
    eprintln!("Usage: notmux agent <command> [flags]");
    eprintln!();
    eprintln!("  register --harness <h> [--budget <n>]                  Bind this pane as orchestrator (root)");
    eprintln!("  lead --harness <h> [--budget <n>] [--project <id>] [--cwd <dir>] [--name <n>] [--executable <path>]");
    eprintln!("                                                         Open a new orchestrator pane (not from a worker)");
    eprintln!("  spawn --name <n> --harness <h> --task <t>|--task-file <f> [--cwd <dir>] [--executable <path>] [-- <program> [args…]]");
    eprintln!("                                                         Start a worker (root only)");
    eprintln!("  assign --worker <run-id> --name <n> --task <t>|--task-file <f>   Queue an assignment (root only)");
    eprintln!("  list                                                   Runs of my group");
    eprintln!("  get <run-id> | task <task-id> | tasks --worker <run-id> [--after <seq>]");
    eprintln!("  message --to <run-id> --body <text>|--body-file <f>    Send a message");
    eprintln!("  inbox [--unread] [--after <seq>] [--drain]             Read (and with --drain acknowledge) messages");
    eprintln!("  ack <message-id>");
    eprintln!("  wait [--after <seq>] [--timeout <ms>]                  Root: block for group events");
    eprintln!("  next [--after <task-id>] [--timeout <ms>]              Worker: block for the next assignment;");
    eprintln!("                                                         --after names the task you already hold");
    eprintln!("  finish [--task <id>] --outcome succeeded|failed --result <t>|--result-file <f>");
    eprintln!("  stop [<run-id>|--target <run-id>] | cancel [...]       Stop a worker, the group, or myself");
    eprintln!("  hook --event stop|prompt                               Claude Code hook entry (reads stdin)");
    eprintln!("  skill                                                  Print the orchestration skill");
    eprintln!();
    eprintln!("Identity: NOTMUX_RUN_ID (launched agents) or NOTMUX_TERMINAL_ID (any pane); --run <id> overrides.");
}

// ── Outcome of a command ───────────────────────────────────────────────────

enum Rendered {
    Json(serde_json::Value),
    Text(String),
    Silent,
}

/// What a command prints, and what it acknowledges after printing.
struct Output {
    rendered: Rendered,
    /// Message IDs acknowledged once the output is on stdout.
    ack_after: Vec<String>,
}

impl Output {
    fn json(value: serde_json::Value) -> Self {
        Output {
            rendered: Rendered::Json(value),
            ack_after: vec![],
        }
    }
    fn text(text: String) -> Self {
        Output {
            rendered: Rendered::Text(text),
            ack_after: vec![],
        }
    }
    fn silent() -> Self {
        Output {
            rendered: Rendered::Silent,
            ack_after: vec![],
        }
    }
    fn with_acks(mut self, ids: Vec<String>) -> Self {
        self.ack_after = ids;
        self
    }
}

#[derive(Debug)]
enum CliError {
    Usage(String),
    Agent(AgentError),
    Transport(String),
}

impl From<String> for CliError {
    fn from(s: String) -> Self {
        CliError::Usage(s)
    }
}

impl From<&str> for CliError {
    fn from(s: &str) -> Self {
        CliError::Usage(s.to_string())
    }
}

pub fn cli_agent(args: &[String]) -> i32 {
    let Some(verb) = args.first().map(String::as_str) else {
        print_usage();
        return 1;
    };
    let parsed = parse(&args[1..]);
    if parsed.has("help") || verb == "help" {
        print_usage();
        return 0;
    }
    if let Err(msg) = check_flags(verb, &parsed) {
        eprintln!("error: {msg}");
        eprintln!("Run `notmux agent help` for usage.");
        return 2;
    }
    let me = caller(&parsed);
    match run(verb, &parsed, &me) {
        Ok(output) => {
            // Write and flush explicitly: a failed write (closed pipe, hook
            // killed) means the messages did not leave this process, so
            // they must stay unread.
            if let Err(e) = write_output(&mut std::io::stdout().lock(), &output.rendered) {
                eprintln!("error: output could not be written ({e}); nothing was acknowledged");
                return 1;
            }
            // Only now are delivered messages acknowledged.
            for id in &output.ack_after {
                if let Err(e) = call(&me, &AgentRequest::Ack { message_id: id.clone() }, REQUEST_TIMEOUT_SECS) {
                    eprintln!("warning: message {id} not acknowledged ({}); it will be delivered again", describe(&e));
                }
            }
            0
        }
        Err(CliError::Usage(msg)) => {
            eprintln!("error: {msg}");
            eprintln!("Run `notmux agent help` for usage.");
            2
        }
        Err(e) => {
            eprintln!("error: {}", describe(&e));
            1
        }
    }
}

fn write_output(out: &mut dyn std::io::Write, rendered: &Rendered) -> std::io::Result<()> {
    match rendered {
        Rendered::Json(value) => writeln!(out, "{}", serde_json::to_string_pretty(value).unwrap_or_default())?,
        Rendered::Text(text) if !text.is_empty() => writeln!(out, "{text}")?,
        _ => {}
    }
    out.flush()
}

fn describe(e: &CliError) -> String {
    match e {
        CliError::Usage(m) | CliError::Transport(m) => m.clone(),
        CliError::Agent(a) => format!("{}: {}", a.code.as_str(), a.message),
    }
}

// ── Request building ───────────────────────────────────────────────────────

/// What a verb resolves to, before any network call.
#[derive(Debug, PartialEq, Eq)]
enum Built {
    /// One request and its transport timeout in seconds.
    Request(AgentRequest, u64),
    /// `inbox --drain`: read, print, then acknowledge.
    Drain(AgentRequest),
    Hook(String),
    Skill,
}

fn build(verb: &str, args: &Args) -> Result<Built, CliError> {
    let request = match verb {
        "register" => AgentRequest::Register {
            harness: harness_arg(args)?,
            total_start_budget: args.u64_flag("budget")?.unwrap_or(8) as u32,
        },
        "lead" => AgentRequest::Lead {
            request_id: crate::orchestration::new_id(),
            project_id: args.flag("project").map(str::to_string),
            harness: harness_arg(args)?,
            total_start_budget: args.u64_flag("budget")?.unwrap_or(8) as u32,
            name: args.flag("name").map(str::to_string),
            cwd: args.flag("cwd").map(str::to_string),
            executable: args.flag("executable").map(str::to_string),
        },
        "spawn" => {
            let harness = harness_arg(args)?;
            let completion = match args.flag("completion") {
                Some(c) => Some(CompletionMode::parse(c).ok_or_else(|| format!("unknown completion {c:?}"))?),
                None => None,
            };
            AgentRequest::Spawn {
                request_id: crate::orchestration::new_id(),
                name: args.flag("name").ok_or("--name <name> is required")?.to_string(),
                harness,
                task: text_arg(args, "task")?,
                cwd: args.flag("cwd").map(str::to_string),
                executable: args.flag("executable").map(str::to_string),
                argv: args.rest.clone(),
                completion,
            }
        }
        "assign" => AgentRequest::Assign {
            request_id: crate::orchestration::new_id(),
            worker_id: args.flag("worker").ok_or("--worker <run-id> is required")?.to_string(),
            name: args.flag("name").ok_or("--name <name> is required")?.to_string(),
            task: text_arg(args, "task")?,
        },
        "list" => AgentRequest::List,
        "get" => AgentRequest::Get {
            run_id: args.positional.first().ok_or("get <run-id>")?.to_string(),
        },
        "task" => AgentRequest::Task {
            task_id: args.positional.first().ok_or("task <task-id>")?.to_string(),
        },
        "tasks" => AgentRequest::Tasks {
            worker_id: args.flag("worker").ok_or("--worker <run-id> is required")?.to_string(),
            after_sequence: args.u64_flag("after")?.unwrap_or(0),
            limit: args.u64_flag("limit")?.map(|n| n as usize),
        },
        "message" => AgentRequest::Message {
            request_id: crate::orchestration::new_id(),
            recipient_id: args.flag("to").ok_or("--to <run-id> is required")?.to_string(),
            body: text_arg(args, "body")?,
        },
        "inbox" => {
            let request = AgentRequest::Inbox {
                after_sequence: args.u64_flag("after")?.unwrap_or(0),
                limit: args.u64_flag("limit")?.map(|n| n as usize),
                unacknowledged_only: args.has("unread") || args.has("drain"),
            };
            if args.has("drain") {
                return Ok(Built::Drain(request));
            }
            request
        }
        "ack" => AgentRequest::Ack {
            message_id: args.positional.first().ok_or("ack <message-id>")?.to_string(),
        },
        "wait" => AgentRequest::Wait {
            after_sequence: args.u64_flag("after")?.unwrap_or(0),
            timeout_ms: args.u64_flag("timeout")?,
        },
        "next" => AgentRequest::Next {
            request_id: crate::orchestration::new_id(),
            after_task_id: args.flag("after").map(str::to_string),
            timeout_ms: Some(args.u64_flag("timeout")?.unwrap_or(COOPERATIVE_NEXT_TIMEOUT_MS)),
        },
        "finish" => {
            let outcome_text = args.flag("outcome").ok_or("--outcome succeeded|failed is required")?;
            AgentRequest::Finish {
                request_id: crate::orchestration::new_id(),
                task_id: args.flag("task").map(str::to_string),
                target_id: None,
                outcome: Outcome::parse(outcome_text).ok_or_else(|| format!("unknown outcome {outcome_text:?}"))?,
                body: text_arg(args, "result")?,
            }
        }
        "stop" | "cancel" => {
            let target = args
                .flag("target")
                .map(str::to_string)
                .or_else(|| args.positional.first().cloned());
            if verb == "stop" {
                AgentRequest::Stop { target_id: target }
            } else {
                AgentRequest::Cancel { target_id: target }
            }
        }
        "hook" => return Ok(Built::Hook(args.flag("event").ok_or("--event stop|prompt is required")?.to_string())),
        "handoff" => return Ok(Built::Hook("stop".to_string())),
        "skill" => return Ok(Built::Skill),
        other => return Err(format!("unknown agent command {other:?}").into()),
    };
    request.validate().map_err(CliError::Agent)?;
    let timeout = match &request {
        AgentRequest::Wait { timeout_ms, .. } | AgentRequest::Next { timeout_ms, .. } => {
            timeout_ms.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS) / 1000 + WAIT_MARGIN_SECS
        }
        _ => REQUEST_TIMEOUT_SECS,
    };
    Ok(Built::Request(request, timeout))
}

fn run(verb: &str, args: &Args, me: &Caller) -> Result<Output, CliError> {
    match build(verb, args)? {
        Built::Request(request, timeout) => {
            let response = call(me, &request, timeout)?;
            Ok(Output::json(serde_json::to_value(response).unwrap_or_default()))
        }
        Built::Drain(request) => {
            let response = call(me, &request, REQUEST_TIMEOUT_SECS)?;
            let ids = match &response {
                AgentResponse::Messages(page) => page.items.iter().map(|m| m.id.clone()).collect(),
                _ => vec![],
            };
            Ok(Output::json(serde_json::to_value(response).unwrap_or_default()).with_acks(ids))
        }
        Built::Hook(event) => hook(me, &event),
        Built::Skill => Ok(Output::text(crate::orchestration::bootstrap::SKILL_SOURCE.to_string())),
    }
}

// ── Message delivery for hooks ─────────────────────────────────────────────

/// The messages a hook may deliver within `budget_units` (UTF-16 code
/// units, the unit notagent caps stdout in; `None` = unlimited). A message
/// that does not fit is not delivered; it is named so the agent reads it
/// with `notmux agent inbox`.
///
/// Hooks never acknowledge. Printing a message proves nothing about the
/// agent having received it (the harness may drop the hook's output), so
/// only the agent marks a message read, with `notmux agent ack <id>`;
/// until then every hook and every `next` delivers it again.
#[derive(Debug, PartialEq, Eq)]
struct Delivery {
    text: String,
    delivered_ids: Vec<String>,
    skipped: Vec<u64>,
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

fn select_deliverable(messages: &[MessageRecord], budget_units: Option<usize>) -> Delivery {
    const HEADER_UNITS: usize = 260;
    let mut delivered: Vec<&MessageRecord> = Vec::new();
    let mut skipped: Vec<u64> = Vec::new();
    let mut used = HEADER_UNITS;
    for m in messages {
        // Body plus the per-message header line with sender and id.
        let cost = utf16_len(&m.body) + utf16_len(&m.id) + utf16_len(&m.sender_id) + 40;
        match budget_units {
            Some(limit) if used + cost > limit => skipped.push(m.sequence),
            _ => {
                used += cost;
                delivered.push(m);
            }
        }
    }
    let mut text = String::from(
        "Unread messages from your NotMux group. After reading one, acknowledge it with the shell command `notmux agent ack <id>`; an unacknowledged message is delivered again.\n",
    );
    for m in &delivered {
        text.push_str(&format!(
            "\n--- message {} from {} (id {}) ---\n{}\n",
            m.sequence,
            m.sender_id,
            m.id,
            m.body.trim_end()
        ));
    }
    if !skipped.is_empty() {
        // Bounded: at most ten sequence numbers, then a count.
        let mut list = skipped.iter().take(10).map(u64::to_string).collect::<Vec<_>>().join(", ");
        if skipped.len() > 10 {
            list.push_str(&format!(" and {} more", skipped.len() - 10));
        }
        text.push_str(&format!(
            "\nNot shown here because of the hook output limit, still unread: message(s) {list}. Read them with `notmux agent inbox --unread`.\n"
        ));
    }
    Delivery {
        text,
        delivered_ids: delivered.iter().map(|m| m.id.clone()).collect(),
        skipped,
    }
}

/// The output of a hook that delivers messages: as the prompt's context,
/// or as a Stop-hook continuation. Never with acknowledgements attached.
fn hook_delivery(d: Delivery, as_stop_continuation: bool) -> Output {
    if as_stop_continuation {
        Output::json(block(d.text))
    } else {
        Output::text(d.text)
    }
}

fn hook_budget() -> Option<usize> {
    (harness() == "notagent").then_some(NOTAGENT_HOOK_BUDGET_UNITS)
}

fn unread_inbox(me: &Caller) -> Result<Vec<MessageRecord>, CliError> {
    let response = call(
        me,
        &AgentRequest::Inbox {
            after_sequence: 0,
            limit: None,
            unacknowledged_only: true,
        },
        REQUEST_TIMEOUT_SECS,
    )?;
    Ok(match response {
        AgentResponse::Messages(page) => page.items,
        _ => vec![],
    })
}

/// The Claude Code hook entry. `stop`: deliver the next assignment or
/// pending messages into the conversation, or let Claude stop when the
/// run is over. `prompt`: drain the inbox into the prompt's context.
fn hook(me: &Caller, event: &str) -> Result<Output, CliError> {
    let role = std::env::var(ENV_AGENT_ROLE).unwrap_or_default();
    if me.run_id.is_none() {
        // Not a launched agent: hooks stay silent.
        return Ok(Output::silent());
    }
    match event {
        "prompt" => {
            let messages = unread_inbox(me)?;
            if messages.is_empty() {
                return Ok(Output::silent());
            }
            let d = select_deliverable(&messages, hook_budget());
            Ok(hook_delivery(d, false))
        }
        "stop" => {
            // Only Claude Code has a Stop hook that continues the
            // conversation. Any other harness reaching this stays silent.
            if harness() != "claude" {
                return Ok(Output::silent());
            }
            if role != "worker" {
                // A root decides for itself; only pending messages are worth
                // a continuation.
                let messages = unread_inbox(me)?;
                if messages.is_empty() {
                    return Ok(Output::silent());
                }
                let d = select_deliverable(&messages, hook_budget());
                return Ok(hook_delivery(d, true));
            }
            // The hook payload says whether this turn was already one of
            // our continuations (`stop_hook_active`).
            let hook_driven = super::agent_session::read_stdin_json()
                .and_then(|v| v.get("stop_hook_active").and_then(|b| b.as_bool()))
                .unwrap_or(false);
            let open_task = current_task_open(me)?;
            if let Some(task_id) = &open_task {
                let finish_hint = format!(
                    "Your assignment {task_id} is not reported yet. Run the shell command `notmux agent finish --task {task_id} --outcome succeeded|failed --result …` (or `--result-file <file>`) with what you did, verified, and left out. Then run `notmux agent next --after {task_id}` and wait for the next assignment. If you are waiting for the orchestrator's answer, run `notmux agent next --after {task_id}` and repeat it until messages arrive."
                );
                if !hook_driven {
                    // First stop without a report: say so at once, after
                    // delivering any answer the root already sent.
                    let messages = unread_inbox(me)?;
                    if !messages.is_empty() {
                        let d = select_deliverable(&messages, hook_budget());
                        return Ok(hook_delivery(d, true));
                    }
                    return Ok(Output::json(block(finish_hint)));
                }
                // Stopped again without reporting: the worker is probably
                // waiting for the root. Wait for messages for the hook time
                // instead of nudging in a hot loop; `--after` keeps the held
                // task from being delivered again.
                let response = call(
                    me,
                    &AgentRequest::Next {
                        request_id: crate::orchestration::new_id(),
                        after_task_id: Some(task_id.clone()),
                        timeout_ms: Some(STOP_HOOK_WAIT_MS),
                    },
                    STOP_HOOK_WAIT_MS / 1000 + WAIT_MARGIN_SECS,
                )?;
                if let AgentResponse::Next(next) = response {
                    if next.stopped {
                        return Ok(Output::silent());
                    }
                    if !next.messages.is_empty() {
                        let d = select_deliverable(&next.messages, hook_budget());
                        return Ok(hook_delivery(d, true));
                    }
                }
                return Ok(Output::json(block(finish_hint)));
            }
            let response = call(
                me,
                &AgentRequest::Next {
                    request_id: crate::orchestration::new_id(),
                    after_task_id: None,
                    timeout_ms: Some(STOP_HOOK_WAIT_MS),
                },
                STOP_HOOK_WAIT_MS / 1000 + WAIT_MARGIN_SECS,
            )?;
            let AgentResponse::Next(next) = response else {
                return Ok(Output::silent());
            };
            if next.stopped {
                return Ok(Output::silent());
            }
            if let Some(task) = next.task {
                return Ok(Output::json(block(render_task(&task))));
            }
            if !next.messages.is_empty() {
                let d = select_deliverable(&next.messages, hook_budget());
                return Ok(hook_delivery(d, true));
            }
            // Nothing arrived while the hook waited: hand over to the
            // cooperative loop so the worker keeps listening.
            Ok(Output::json(block(
                "No new assignment arrived yet. Run the shell command `notmux agent next` (it blocks up to 100 s) and repeat it until it returns a task or says you are stopped. Do nothing else meanwhile.".to_string(),
            )))
        }
        other => Err(format!("unknown hook event {other:?}; use stop or prompt").into()),
    }
}

/// The ID of the worker's unreported assignment, if it has one.
fn current_task_open(me: &Caller) -> Result<Option<String>, CliError> {
    let Some(run_id) = &me.run_id else { return Ok(None) };
    let AgentResponse::Run(run) = call(me, &AgentRequest::Get { run_id: run_id.clone() }, REQUEST_TIMEOUT_SECS)? else {
        return Ok(None);
    };
    Ok(run.current_task_id)
}

/// The continuation a Claude Code Stop hook hands back: Claude continues
/// on `decision: block` with `reason` as the next instruction.
fn block(reason: String) -> serde_json::Value {
    serde_json::json!({ "decision": "block", "reason": reason })
}

fn render_task(task: &AssignmentRecord) -> String {
    let mut s = format!(
        "New assignment from the NotMux orchestrator.\nTask ID: {}\nName: {}\n",
        task.id, task.name
    );
    if !task.document_path.is_empty() {
        s.push_str(&format!("File: {}\n", task.document_path));
    }
    s.push('\n');
    s.push_str(task.body.trim_end());
    s.push_str(&format!(
        "\n\nWhen done, report with `notmux agent finish --task {} --outcome succeeded|failed --result …`, then run `notmux agent next --after {}`.",
        task.id, task.id
    ));
    s
}

// ── Transport ──────────────────────────────────────────────────────────────

/// POST the envelope; retry the same request ID on transport failures.
fn call(me: &Caller, request: &AgentRequest, timeout_secs: u64) -> Result<AgentResponse, CliError> {
    let token = ensure_token().map_err(CliError::Transport)?;
    let (host, port) = discover_server().map_err(CliError::Transport)?;
    let url = format!("http://{host}:{port}/v1/agents");
    let envelope = AgentEnvelope {
        caller: me.clone(),
        request: request.clone(),
    };
    let body = serde_json::to_string(&envelope).map_err(|e| CliError::Transport(e.to_string()))?;
    let client = super::local_http_client().map_err(CliError::Transport)?;
    let mut last_error = String::new();
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(300 * attempt));
        }
        let sent = client
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .body(body.clone())
            .timeout(Duration::from_secs(timeout_secs))
            .send();
        let resp = match sent {
            Ok(r) => r,
            Err(e) => {
                last_error = format!("Request failed: {e}");
                continue;
            }
        };
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(CliError::Transport(super::LOCAL_AUTH_ERROR.into()));
        }
        let status = resp.status();
        let text = resp.text().map_err(|e| CliError::Transport(format!("Failed to read body: {e}")))?;
        let value: serde_json::Value = serde_json::from_str(&text).map_err(|_| {
            let snippet: String = text.chars().take(300).collect();
            CliError::Transport(if status.is_success() && snippet.contains("<html") {
                "the running NotMux predates `agent` support — restart it with the new build".to_string()
            } else {
                format!("Server returned {status} with a non-JSON body: {snippet}")
            })
        })?;
        if let Some(error) = value.get("error") {
            let err: AgentError = serde_json::from_value(error.clone()).unwrap_or_else(|_| {
                AgentError::new(AgentErrorCode::StorageFailed, error.to_string())
            });
            return Err(CliError::Agent(err));
        }
        return serde_json::from_value(value).map_err(|e| CliError::Transport(format!("unexpected answer: {e}")));
    }
    Err(CliError::Transport(last_error))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Args {
        parse(&list.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    fn message(seq: u64, body: &str) -> MessageRecord {
        MessageRecord {
            id: format!("m{seq}"),
            sequence: seq,
            sender_id: "root".into(),
            recipient_id: "w".into(),
            body: body.into(),
            created_at_ms: 0,
            acknowledged: false,
            request_id: "r".into(),
        }
    }

    #[test]
    fn parses_flags_positionals_and_rest() {
        let p = args(&["--name", "Worker A", "--harness=generic", "--unread", "id-1", "--", "prog", "--x", "y z"]);
        assert_eq!(p.flag("name"), Some("Worker A"));
        assert_eq!(p.flag("harness"), Some("generic"));
        assert!(p.has("unread"));
        assert_eq!(p.positional, vec!["id-1"]);
        assert_eq!(p.rest, vec!["prog", "--x", "y z"]);
    }

    #[test]
    fn task_text_comes_from_flag_or_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("t.md");
        std::fs::write(&file, "from file").unwrap();
        assert_eq!(text_arg(&args(&["--task-file", file.to_str().unwrap()]), "task").unwrap(), "from file");
        assert_eq!(text_arg(&args(&["--task", "inline"]), "task").unwrap(), "inline");
        assert!(text_arg(&args(&[]), "task").is_err());
    }

    const RUN_X: &str = "11111111-1111-4111-8111-111111111111";
    const RUN_Y: &str = "22222222-2222-4222-8222-222222222222";

    #[test]
    fn stop_accepts_target_flag_and_positional_and_rejects_unknown_options() {
        match build("stop", &args(&["--target", RUN_X])).unwrap() {
            Built::Request(AgentRequest::Stop { target_id }, _) => assert_eq!(target_id.as_deref(), Some(RUN_X)),
            other => panic!("{other:?}"),
        }
        match build("cancel", &args(&[RUN_Y])).unwrap() {
            Built::Request(AgentRequest::Cancel { target_id }, _) => assert_eq!(target_id.as_deref(), Some(RUN_Y)),
            other => panic!("{other:?}"),
        }
        // Ids are validated before anything is sent.
        assert!(matches!(build("stop", &args(&["--target", "run-x"])), Err(CliError::Agent(_))));
        // Whole group only when nothing is named.
        match build("stop", &args(&[])).unwrap() {
            Built::Request(AgentRequest::Stop { target_id }, _) => assert_eq!(target_id, None),
            other => panic!("{other:?}"),
        }
        // An option the verb does not know is an error, never silently a
        // group stop.
        let e = check_flags("stop", &args(&["--worker", RUN_X])).unwrap_err();
        assert!(e.contains("unknown option --worker"), "{e}");
        assert!(check_flags("stop", &args(&["--target", RUN_X])).is_ok());
        assert!(check_flags("spawn", &args(&["--name", "a", "--harness", "codex", "--task", "t"])).is_ok());
        assert!(check_flags("nope", &args(&[])).is_err());
    }

    #[test]
    fn next_carries_the_held_task_through_after() {
        match build("next", &args(&["--after", RUN_X, "--timeout", "5000"])).unwrap() {
            Built::Request(AgentRequest::Next { after_task_id, timeout_ms, .. }, secs) => {
                assert_eq!(after_task_id.as_deref(), Some(RUN_X));
                assert_eq!(timeout_ms, Some(5000));
                assert_eq!(secs, 5 + WAIT_MARGIN_SECS);
            }
            other => panic!("{other:?}"),
        }
        match build("next", &args(&[])).unwrap() {
            Built::Request(AgentRequest::Next { after_task_id, timeout_ms, .. }, _) => {
                assert_eq!(after_task_id, None);
                assert_eq!(timeout_ms, Some(COOPERATIVE_NEXT_TIMEOUT_MS));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn hooks_never_acknowledge_and_drain_acknowledges_only_after_printing() {
        // A hook's output proves nothing about the agent having read it:
        // hook deliveries carry no acknowledgements, in either shape, and
        // tell the agent how to acknowledge by id.
        let d = select_deliverable(&[message(1, "a"), message(2, "b")], None);
        assert_eq!(d.delivered_ids, vec!["m1".to_string(), "m2".to_string()]);
        let text = d.text.clone();
        let prompt = hook_delivery(d, false);
        assert!(prompt.ack_after.is_empty());
        assert!(matches!(prompt.rendered, Rendered::Text(ref t) if t.contains("(id m2)") && t.contains("notmux agent ack <id>")));
        let d = select_deliverable(&[message(1, "a")], None);
        let stop = hook_delivery(d, true);
        assert!(stop.ack_after.is_empty());
        assert!(matches!(stop.rendered, Rendered::Json(ref v) if v["decision"] == "block"));
        assert!(!text.contains("already acknowledged"));
        // `inbox --drain` is the explicit user command: it acknowledges,
        // but only through the output, after it was written.
        assert!(matches!(build("inbox", &args(&["--drain"])).unwrap(), Built::Drain(_)));
    }

    #[test]
    fn delivery_budget_counts_utf16_units_and_skips_what_does_not_fit() {
        // Each of these is one char but two UTF-16 code units.
        let astral = "😀".repeat(1_600); // 3 200 units, plus overhead
        assert_eq!(utf16_len(&astral), 3_200);
        assert_eq!(astral.chars().count(), 1_600);
        let d = select_deliverable(&[message(1, &astral), message(2, "short")], Some(NOTAGENT_HOOK_BUDGET_UNITS));
        assert_eq!(d.delivered_ids, vec!["m2".to_string()], "the oversize message is not delivered");
        assert_eq!(d.skipped, vec![1]);
        assert!(d.text.contains("message(s) 1"));
        assert!(d.text.contains("--- message 2"));
        assert!(utf16_len(&d.text) < 4_000, "notagent's cap");
        // Without a budget everything is delivered.
        let d = select_deliverable(&[message(1, &astral), message(2, "short")], None);
        assert_eq!(d.delivered_ids.len(), 2);
        assert!(d.skipped.is_empty());
        // Messages that fit are delivered in order until the budget is hit,
        // and the text stays within the budget it was given.
        let many: Vec<MessageRecord> = (1..=100).map(|i| message(i, &"x".repeat(100))).collect();
        let d = select_deliverable(&many, Some(1_000));
        assert!(!d.delivered_ids.is_empty() && d.delivered_ids.len() < 100);
        assert_eq!(d.skipped.len(), 100 - d.delivered_ids.len());
        assert!(utf16_len(&d.text) <= 1_000 + 250, "skipped list adds a bounded tail");
        // Thousands of unread messages still fit notagent's cap.
        let flood: Vec<MessageRecord> = (1..=5_000).map(|i| message(i, &"y".repeat(50))).collect();
        let d = select_deliverable(&flood, Some(NOTAGENT_HOOK_BUDGET_UNITS));
        assert!(utf16_len(&d.text) < 4_000, "{}", utf16_len(&d.text));
        assert!(d.text.contains("more"));
    }

    #[test]
    fn stop_hook_block_shape_and_task_rendering() {
        let v = block("go".into());
        assert_eq!(v["decision"], "block");
        assert_eq!(v["reason"], "go");
        let task = AssignmentRecord {
            id: "t1".into(),
            worker_id: "w".into(),
            root_id: "r".into(),
            sequence: 2,
            name: "second".into(),
            state: AssignmentState::Running,
            body: "do more".into(),
            document_path: "/x/task.md".into(),
            created_at_ms: 0,
            started_at_ms: None,
            result: None,
            request_id: "q".into(),
        };
        let text = render_task(&task);
        assert!(text.contains("Task ID: t1"));
        assert!(text.contains("do more"));
        assert!(text.contains("finish --task t1"));
        assert!(text.contains("next --after t1"));
    }

    #[test]
    fn options_that_need_a_value_refuse_to_run_without_one() {
        // `stop --target` with nothing after it would otherwise stop the group.
        let e = check_flags("stop", &args(&["--target"])).unwrap_err();
        assert!(e.contains("--target needs a value"), "{e}");
        let e = check_flags("cancel", &args(&["--target="])).unwrap_err();
        assert!(e.contains("needs a value"), "{e}");
        // A following option is not taken as the value.
        let e = check_flags("stop", &args(&["--target", "--run", RUN_X])).unwrap_err();
        assert!(e.contains("--target needs a value"), "{e}");
        // Target named twice is ambiguous.
        let e = check_flags("stop", &args(&["--target", RUN_X, RUN_Y])).unwrap_err();
        assert!(e.contains("takes the target once"), "{e}");
        // Flags without values stay valid, and cannot be given one.
        assert!(check_flags("inbox", &args(&["--unread", "--drain"])).is_ok());
        assert!(check_flags("inbox", &args(&["--unread=yes"])).is_err());
        assert!(check_flags("next", &args(&["--after"])).is_err());
    }

    /// A writer whose flush fails, like a pipe closed by a killed hook.
    struct BrokenPipe;
    impl std::io::Write for BrokenPipe {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "closed"))
        }
    }

    #[test]
    fn a_failed_write_is_reported_so_nothing_gets_acknowledged() {
        let rendered = Rendered::Text("message".into());
        assert!(write_output(&mut BrokenPipe, &rendered).is_err());
        let mut ok = Vec::new();
        write_output(&mut ok, &rendered).unwrap();
        assert_eq!(ok, b"message\n");
    }
}
