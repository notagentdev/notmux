//! `notmux browser …` — automation of embedded browser
//! panes. Talks to `POST /v1/browser`; the app resolves element
//! refs (`@eN`) against the pane's latest `snapshot`.

use crate::cli::{discover_server, ensure_token};
use notmux_core::api::BrowserRequest;

pub fn cli_browser(args: &[String]) -> i32 {
    if args.is_empty()
        || matches!(args[0].as_str(), "help" | "--help" | "-h")
    {
        print_browser_help();
        return if args.is_empty() { 1 } else { 0 };
    }

    let (req, json_mode) = match parse_browser_args(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };

    let token = match ensure_token() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let body = match serde_json::to_string(&req) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Failed to serialize request: {e}");
            return 1;
        }
    };

    // `wait` polls inside the app for up to `timeout_ms` — the HTTP request
    // must outlive it (the shared api_post helper caps at 10s).
    let timeout_secs = req.timeout_ms.map_or(15, |ms| ms / 1000 + 10).max(15);

    match post_browser(&token, &body, timeout_secs) {
        Ok(value) => {
            if json_mode {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
                );
            } else if let Some(text) = value.get("text").and_then(|t| t.as_str()) {
                println!("{text}");
            } else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
                );
            }
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// POST /v1/browser with a per-request timeout (`wait` outlives the shared
/// 10s helper). Returns the parsed JSON body; server-side errors come back
/// as their bare `error` message.
fn post_browser(token: &str, body: &str, timeout_secs: u64) -> Result<serde_json::Value, String> {
    let (host, port) = discover_server()?;
    let url = format!("http://{}:{}/v1/browser", host, port);
    let client = reqwest::blocking::Client::new();
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", token))
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .send()
        .map_err(|e| format!("Request failed: {e}"))?;

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(
            "Token expired or revoked. Delete ~/.config/notmux/cli.json and retry.".into(),
        );
    }
    let status = resp.status();
    let text = resp
        .text()
        .map_err(|e| format!("Failed to read body: {e}"))?;
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|_| {
        if status.is_success() {
            // An older instance without the /v1/browser route answers with
            // the web-client fallback page instead of JSON.
            "the running NotMux predates `browser` support — restart it with the new build"
                .to_string()
        } else {
            format!("Server returned {status}: {text}")
        }
    })?;
    if !status.is_success() {
        return Err(value
            .get("error")
            .and_then(|e| e.as_str())
            .map_or_else(|| format!("Server returned {status}: {text}"), String::from));
    }
    Ok(value)
}

/// Parses the `notmux browser` grammar into a request. Returns the request
/// plus whether `--json` output was asked for.
fn parse_browser_args(args: &[String]) -> Result<(BrowserRequest, bool), String> {
    // Inside a NotMux terminal the request is scoped to that terminal's
    // project (see `BrowserRequest::terminal_id`); explicit --pane/--project
    // still win on the server.
    let mut req = BrowserRequest {
        terminal_id: std::env::var("NOTMUX_TERMINAL_ID")
            .ok()
            .filter(|s| !s.is_empty()),
        ..BrowserRequest::default()
    };
    let mut json_mode = false;
    let mut positional: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let flag_value = |i: &mut usize, name: &str| -> Result<String, String> {
            *i += 1;
            args.get(*i)
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match args[i].as_str() {
            "--pane" => req.pane = Some(flag_value(&mut i, "--pane")?),
            "--project" => req.project_id = Some(flag_value(&mut i, "--project")?),
            "--timeout" => {
                let raw = flag_value(&mut i, "--timeout")?;
                req.timeout_ms = Some(
                    raw.parse::<u64>()
                        .map_err(|_| format!("--timeout must be milliseconds, got `{raw}`"))?,
                );
            }
            "--full" | "-f" => req.full = Some(true),
            "--json" => json_mode = true,
            other => positional.push(other.to_string()),
        }
        i += 1;
    }

    let Some(verb) = positional.first().cloned() else {
        return Err("Usage: notmux browser <command> — see `notmux browser help`".to_string());
    };
    let rest = &positional[1..];

    let element_arg = |what: &str| -> Result<String, String> {
        rest.first()
            .cloned()
            .ok_or_else(|| format!("{what} requires an element ref (e.g. @e3) from `snapshot`"))
    };

    match verb.as_str() {
        "list" | "back" | "forward" | "reload" | "snapshot" | "screenshot" => {
            req.action = verb;
        }
        "open" | "goto" | "navigate" => {
            req.action = "open".to_string();
            req.url = Some(
                rest.first()
                    .cloned()
                    .ok_or("open requires a URL")?,
            );
        }
        "click" | "dblclick" | "hover" | "focus" | "check" | "uncheck" => {
            req.element = Some(element_arg(&verb)?);
            req.action = verb;
        }
        "scrollintoview" | "scroll-into-view" | "scroll_into_view" => {
            req.element = Some(element_arg("scroll_into_view")?);
            req.action = "scroll_into_view".to_string();
        }
        "fill" | "type" => {
            req.element = Some(element_arg(&verb)?);
            // Empty text is deliberate: `fill @e3` clears the field.
            req.text = Some(rest[1..].join(" "));
            req.action = verb;
        }
        "press" => {
            req.element = Some(element_arg("press")?);
            req.key = Some(
                rest.get(1)
                    .cloned()
                    .ok_or("press requires a key (e.g. Enter, Escape, Tab)")?,
            );
            req.action = verb;
        }
        "select" => {
            req.element = Some(element_arg("select")?);
            let values: Vec<String> = rest[1..].to_vec();
            if values.is_empty() {
                return Err("select requires at least one option value".to_string());
            }
            req.values = Some(values);
            req.action = verb;
        }
        "scroll" => {
            let parse_delta = |idx: usize, name: &str| -> Result<i64, String> {
                let raw = rest
                    .get(idx)
                    .ok_or_else(|| format!("scroll requires <dx> <dy> (missing {name})"))?;
                raw.parse::<i64>()
                    .map_err(|_| format!("scroll {name} must be a pixel delta, got `{raw}`"))
            };
            req.dx = Some(parse_delta(0, "dx")?);
            req.dy = Some(parse_delta(1, "dy")?);
            req.element = rest.get(2).cloned();
            req.action = verb;
        }
        "get" => {
            let what = rest
                .first()
                .cloned()
                .ok_or("get requires what: url|title|text|html|value|attr|count|box")?;
            match what.as_str() {
                "url" | "title" => {}
                "count" => {
                    req.selector = Some(
                        rest.get(1)
                            .cloned()
                            .ok_or("get count requires a CSS selector")?,
                    );
                }
                "attr" => {
                    req.element = Some(
                        rest.get(1)
                            .cloned()
                            .ok_or("get attr requires an element ref (e.g. @e3)")?,
                    );
                    req.attr = Some(
                        rest.get(2)
                            .cloned()
                            .ok_or("get attr requires an attribute name")?,
                    );
                }
                "text" | "html" | "value" | "box" => {
                    req.element = Some(
                        rest.get(1)
                            .cloned()
                            .ok_or_else(|| {
                                format!("get {what} requires an element ref (e.g. @e3)")
                            })?,
                    );
                }
                other => {
                    return Err(format!(
                        "unknown getter `{other}` — use url|title|text|html|value|attr|count|box"
                    ));
                }
            }
            req.what = Some(what);
            req.action = verb;
        }
        "is" => {
            let what = rest
                .first()
                .cloned()
                .ok_or("is requires what: visible|enabled|checked")?;
            if !matches!(what.as_str(), "visible" | "enabled" | "checked") {
                return Err(format!(
                    "unknown check `{what}` — use visible|enabled|checked"
                ));
            }
            req.element = Some(
                rest.get(1)
                    .cloned()
                    .ok_or_else(|| format!("is {what} requires an element ref (e.g. @e3)"))?,
            );
            req.what = Some(what);
            req.action = verb;
        }
        "wait" => {
            req.selector = Some(
                rest.first()
                    .cloned()
                    .ok_or("wait requires a CSS selector")?,
            );
            if let Some(raw) = rest.get(1)
                && req.timeout_ms.is_none()
            {
                req.timeout_ms = Some(raw.parse::<u64>().map_err(|_| {
                    format!("wait timeout must be milliseconds, got `{raw}`")
                })?);
            }
            req.action = verb;
        }
        "eval" => {
            let js = rest.join(" ");
            if js.trim().is_empty() {
                return Err("eval requires a JavaScript expression".to_string());
            }
            req.js = Some(js);
            req.action = verb;
        }
        other => {
            return Err(format!(
                "unknown browser command `{other}` — see `notmux browser help`"
            ));
        }
    }

    Ok((req, json_mode))
}

fn print_browser_help() {
    eprintln!("Usage: notmux browser [--pane <id>] [--project <id>] [--json] <command>");
    eprintln!();
    eprintln!("Automate an embedded browser pane.");
    eprintln!("Element refs (@eN) come from the latest `snapshot` of that pane.");
    eprintln!();
    eprintln!("Commands:");
    eprintln!("  list                               List open browser panes (pane, project, URL)");
    eprintln!("  open <url>                         Navigate the pane (creates one if none exists;");
    eprintln!("                                     --project forces a new pane in that project)");
    eprintln!("                                     Inside a NotMux terminal, pane lookup and new panes are");
    eprintln!("                                     scoped to that terminal's project (pinned next to a pinned");
    eprintln!("                                     caller; otherwise only in that project, focus untouched)");
    eprintln!("  back | forward | reload            History navigation");
    eprintln!("  snapshot [--full]                  Page outline with element refs (--full adds content roles)");
    eprintln!("  screenshot                         Save a PNG of the page, print its path (macOS)");
    eprintln!("  click|dblclick|hover|focus <@ref>  Pointer actions");
    eprintln!("  fill <@ref> [text]                 Set an input's value (no text clears it)");
    eprintln!("  type <@ref> <text>                 Type character by character");
    eprintln!("  press <@ref> <key>                 Send a key (Enter, Escape, Tab, …)");
    eprintln!("  check|uncheck <@ref>               Toggle a checkbox/radio");
    eprintln!("  select <@ref> <value…>             Choose <select> option(s)");
    eprintln!("  scroll <dx> <dy> [@ref]            Scroll the window (or an element)");
    eprintln!("  scrollintoview <@ref>              Scroll an element into view");
    eprintln!("  get url|title                      Page-level getters");
    eprintln!("  get text|html|value|box <@ref>     Element getters");
    eprintln!("  get attr <@ref> <name>             Attribute getter");
    eprintln!("  get count <selector>               Count elements matching a CSS selector");
    eprintln!("  is visible|enabled|checked <@ref>  State checks");
    eprintln!("  wait <selector> [ms]               Wait until a selector is visible (default 5000ms)");
    eprintln!("  eval <js>                          Evaluate JavaScript, print the JSON result");
    eprintln!();
    eprintln!("Targeting: with one open browser pane no flags are needed;");
    eprintln!("otherwise pass --pane <id> (see `notmux browser list`).");
}

#[cfg(test)]
mod tests {
    use super::parse_browser_args;

    fn parse(args: &[&str]) -> Result<(notmux_core::api::BrowserRequest, bool), String> {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse_browser_args(&args)
    }

    #[test]
    fn open_aliases_map_to_open_and_require_a_url() {
        for alias in ["open", "goto", "navigate"] {
            let (req, _) = parse(&[alias, "example.com"]).unwrap();
            assert_eq!(req.action, "open");
            assert_eq!(req.url.as_deref(), Some("example.com"));
        }
        assert!(parse(&["open"]).is_err(), "missing URL is an error");
    }

    #[test]
    fn fill_joins_text_and_allows_clearing() {
        let (req, _) = parse(&["fill", "@e3", "hello", "world"]).unwrap();
        assert_eq!(req.action, "fill");
        assert_eq!(req.element.as_deref(), Some("@e3"));
        assert_eq!(req.text.as_deref(), Some("hello world"));

        // No text = clear, not an error.
        let (req, _) = parse(&["fill", "e3"]).unwrap();
        assert_eq!(req.text.as_deref(), Some(""));
        assert!(parse(&["fill"]).is_err(), "missing ref is an error");
    }

    #[test]
    fn get_variants_route_their_arguments() {
        let (req, _) = parse(&["get", "url"]).unwrap();
        assert_eq!((req.action.as_str(), req.what.as_deref()), ("get", Some("url")));
        assert!(req.element.is_none() && req.selector.is_none());

        let (req, _) = parse(&["get", "count", ".row"]).unwrap();
        assert_eq!(req.selector.as_deref(), Some(".row"));

        let (req, _) = parse(&["get", "attr", "@e2", "href"]).unwrap();
        assert_eq!(req.element.as_deref(), Some("@e2"));
        assert_eq!(req.attr.as_deref(), Some("href"));

        let (req, _) = parse(&["get", "text", "@e1"]).unwrap();
        assert_eq!(req.element.as_deref(), Some("@e1"));

        assert!(parse(&["get", "attr", "@e2"]).is_err(), "attr name required");
        assert!(parse(&["get", "cookies"]).is_err(), "unknown getter");
        assert!(parse(&["get", "text"]).is_err(), "element ref required");
    }

    #[test]
    fn scroll_parses_deltas_and_optional_element() {
        let (req, _) = parse(&["scroll", "0", "-400"]).unwrap();
        assert_eq!((req.dx, req.dy, req.element), (Some(0), Some(-400), None));

        let (req, _) = parse(&["scroll", "10", "20", "@e5"]).unwrap();
        assert_eq!(req.element.as_deref(), Some("@e5"));

        assert!(parse(&["scroll", "10"]).is_err(), "dy required");
        assert!(parse(&["scroll", "x", "20"]).is_err(), "numeric deltas only");
    }

    #[test]
    fn wait_takes_positional_timeout_but_flag_wins() {
        let (req, _) = parse(&["wait", "#done", "8000"]).unwrap();
        assert_eq!(req.selector.as_deref(), Some("#done"));
        assert_eq!(req.timeout_ms, Some(8000));

        let (req, _) = parse(&["wait", "#done", "8000", "--timeout", "2000"]).unwrap();
        assert_eq!(req.timeout_ms, Some(2000), "--timeout takes precedence");

        assert!(parse(&["wait"]).is_err(), "selector required");
        assert!(parse(&["wait", "#d", "soon"]).is_err(), "timeout must be ms");
    }

    #[test]
    fn flags_fill_the_request_envelope() {
        let (req, json) = parse(&[
            "--pane", "slot-1", "--project", "p1", "--full", "--json", "snapshot",
        ])
        .unwrap();
        assert_eq!(req.action, "snapshot");
        assert_eq!(req.pane.as_deref(), Some("slot-1"));
        assert_eq!(req.project_id.as_deref(), Some("p1"));
        assert_eq!(req.full, Some(true));
        assert!(json);
        assert!(parse(&["--pane"]).is_err(), "--pane needs a value");
    }

    #[test]
    fn select_press_and_is_validate_their_arguments() {
        let (req, _) = parse(&["select", "@e4", "one", "two"]).unwrap();
        assert_eq!(req.values, Some(vec!["one".to_string(), "two".to_string()]));
        assert!(parse(&["select", "@e4"]).is_err(), "values required");

        let (req, _) = parse(&["press", "@e1", "Enter"]).unwrap();
        assert_eq!(req.key.as_deref(), Some("Enter"));
        assert!(parse(&["press", "@e1"]).is_err(), "key required");

        let (req, _) = parse(&["is", "checked", "e7"]).unwrap();
        assert_eq!(req.what.as_deref(), Some("checked"));
        assert_eq!(req.element.as_deref(), Some("e7"));
        assert!(parse(&["is", "happy", "e7"]).is_err(), "unknown check");
    }

    #[test]
    fn scroll_into_view_aliases_and_unknown_verbs() {
        for alias in ["scrollintoview", "scroll-into-view", "scroll_into_view"] {
            let (req, _) = parse(&[alias, "@e2"]).unwrap();
            assert_eq!(req.action, "scroll_into_view");
        }
        assert!(parse(&["teleport"]).is_err(), "unknown verb is an error");
        assert!(parse(&[]).is_err(), "no verb is an error");
    }

    #[test]
    fn eval_joins_the_expression() {
        let (req, _) = parse(&["eval", "1", "+", "1"]).unwrap();
        assert_eq!(req.js.as_deref(), Some("1 + 1"));
        assert!(parse(&["eval"]).is_err(), "expression required");
    }
}
