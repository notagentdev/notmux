mod agent_commands;
mod agent_session;
mod browser_commands;
mod commands;
mod hooks;
mod terminal_commands;
#[cfg(test)]
mod tests;

use crate::workspace::persistence::config_dir;

const LOCAL_AUTH_ERROR: &str = "Local CLI authentication failed. Restart NotMux with the same build and configuration directory.";

/// Try to handle a CLI subcommand. Returns `Some(exit_code)` if a subcommand
/// was matched (caller should exit), or `None` to continue with GUI startup.
pub fn try_handle_cli() -> Option<i32> {
    let args: Vec<String> = std::env::args().collect();
    let subcommand = args.get(1)?.as_str();

    let rest = &args[2..];

    let code = match subcommand {
        "pair" => commands::cli_pair(),
        "health" => commands::cli_health(rest),
        "state" => commands::cli_state(),
        "action" => {
            let json = args.get(2).map(|s| s.as_str());
            commands::cli_action(json)
        }
        "services" => commands::cli_services(rest),
        "service" => commands::cli_service(rest),
        "projects" => terminal_commands::cli_projects(rest),
        "terminals" => terminal_commands::cli_terminals(rest),
        "send" => terminal_commands::cli_send(rest),
        "run" => terminal_commands::cli_run(rest),
        "key" => terminal_commands::cli_key(rest),
        "split" => terminal_commands::cli_split(rest),
        "focus" => terminal_commands::cli_focus(rest),
        "new-terminal" => terminal_commands::cli_new_terminal(rest),
        "read" => terminal_commands::cli_read(rest),
        "wait" => terminal_commands::cli_wait(rest),
        "wait-output" => terminal_commands::cli_wait_output(rest),
        "agent-explain" => commands::cli_agent_explain(rest),
        "add-project" => terminal_commands::cli_add_project(rest),
        "events" => terminal_commands::cli_events(rest),
        "notify" => commands::cli_notify(rest),
        "clear-notification" => commands::cli_clear_notification(rest),
        "agent-status" => commands::cli_agent_status(rest),
        "browser" => browser_commands::cli_browser(rest),
        "agent" => agent_commands::cli_agent(rest),
        "agent-session" => agent_session::cli_agent_session(rest),
        "hooks" => hooks::cli_hooks(rest),
        "whoami" => commands::cli_whoami(rest),
        "--help" | "-h" | "help" => {
            print_help();
            0
        }
        _ => return None,
    };

    Some(code)
}

fn print_help() {
    eprintln!("Usage: notmux <command> [args]");
    eprintln!();
    eprintln!("Commands:");
    eprintln!("  state                              Print workspace state (JSON)");
    eprintln!("  action <json>                      Execute a raw action (JSON ActionRequest)");
    eprintln!("  projects [--json]                  List projects (id, name, path; * = focused)");
    eprintln!("  terminals [project] [--json]       List terminals (id, name, project)");
    eprintln!("  send <text> [--terminal <id>] [--enter]  Type text into a terminal");
    eprintln!("  run <command> [--terminal <id>]    Run a command in a terminal (text + Enter)");
    eprintln!("  key <key> [--terminal <id>]        Send a special key (enter, ctrl-c, up, …)");
    eprintln!("  split [right|down] [--terminal <id>]  Split the terminal's pane");
    eprintln!("  focus <terminal-id>                Focus a terminal");
    eprintln!("  new-terminal [project]             Create a terminal in a project");
    eprintln!("  read [--terminal <id>] [--json]    Print a terminal's visible content");
    eprintln!("  wait [--terminal <id>] [--until <states>] [--timeout <ms>]  Block until the agent reaches a state");
    eprintln!("  wait-output [--terminal <id>] <text>|--regex <re> [--timeout <ms>]  Block until the screen shows text");
    eprintln!("  agent-status <working|blocked|idle|done> [-t <id>]  Report the agent lifecycle state (called by hooks)");
    eprintln!("  agent-explain [--terminal <id>]    Show how a terminal's agent state was determined");
    eprintln!("  add-project <path> [--name <n>]    Add a project to the workspace");
    eprintln!("  events [-n <count>] [--follow]     Print the event log (events.jsonl)");
    eprintln!("  browser <command> [--pane <id>]    Automate a browser pane (open, snapshot, click, …);");
    eprintln!("                                     see `notmux browser help`");
    eprintln!("  services [project] [--json]        List services and their status");
    eprintln!("  service start <name> [project]     Start a service");
    eprintln!("  service stop <name> [project]      Stop a service");
    eprintln!("  service restart <name> [project]   Restart a service");
    eprintln!("  notify [--title <t>] [--body <b>]  Send a notification to the current or specified terminal");
    eprintln!("  agent <command> …                  Orchestrate agents: register, lead, spawn, assign, next, finish, …;");
    eprintln!("                                     see `notmux agent help`");
    eprintln!("  agent-session <record|end> --kind <agent>  Persist a restorable agent session (called by hooks)");
    eprintln!("  hooks setup [claude|codex|shell]  Install agent notification hooks");
    eprintln!("  hooks uninstall [agent]           Remove agent notification hooks");
    eprintln!("  whoami [--json]                    Identify current terminal and project");
    eprintln!("  health [--json]                    Server health check");
    eprintln!("  pair                               Generate a pairing code for remote clients");
    eprintln!();
    eprintln!("Default output is tab-separated (grep/awk friendly).");
    eprintln!("Use --json for structured JSON output.");
    eprintln!("Local authentication is automatic; no registration or expiring token is needed.");
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Discover a running NotMux instance by reading `remote.json`.
/// Returns `(host, port)`.
fn discover_server() -> Result<(String, u16), String> {
    let path = config_dir().join("remote.json");
    let data =
        std::fs::read_to_string(&path).map_err(|_| "NotMux is not running (no remote.json).")?;
    let json: serde_json::Value =
        serde_json::from_str(&data).map_err(|_| "Invalid remote.json.")?;

    let port = json
        .get("port")
        .and_then(|v| v.as_u64())
        .ok_or("Missing port in remote.json.")? as u16;

    let pid = json.get("pid").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    if pid != 0 && !is_process_alive(pid) {
        return Err("NotMux is not running (stale remote.json).".to_string());
    }

    Ok(("127.0.0.1".to_string(), port))
}

fn is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// Derive a local-only credential without network probes or token-store writes.
/// A transient request failure must never replace a working credential.
fn ensure_token() -> Result<String, String> {
    let secret = std::fs::read(crate::remote::auth::secret_path())
        .map_err(|_| "No NotMux config found. Has NotMux been started at least once?".to_string())?;
    if secret.len() != 32 {
        return Err("Invalid remote_secret (wrong size).".into());
    }
    Ok(crate::remote::auth::local_cli_token(&secret))
}

/// Credentials stay on loopback, even with system proxies or HTTP redirects.
fn local_http_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("Failed to create local HTTP client: {e}"))
}

fn api_get(path: &str, token: &str) -> Result<String, String> {
    let (host, port) = discover_server()?;
    let url = format!("http://{}:{}{}", host, port, path);
    let client = local_http_client()?;
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", token))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .map_err(|e| format!("Request failed: {e}"))?;

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(LOCAL_AUTH_ERROR.into());
    }
    if !resp.status().is_success() {
        return Err(format!("Server returned {}", resp.status()));
    }

    resp.text().map_err(|e| format!("Failed to read body: {e}"))
}

fn api_post(path: &str, token: &str, body: &str) -> Result<String, String> {
    let (host, port) = discover_server()?;
    let url = format!("http://{}:{}{}", host, port, path);
    let client = local_http_client()?;
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", token))
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .map_err(|e| format!("Request failed: {e}"))?;

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(LOCAL_AUTH_ERROR.into());
    }
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        return Err(format!("Server returned {}: {}", status, body));
    }

    resp.text().map_err(|e| format!("Failed to read body: {e}"))
}
