//! Rule-based classification of an agent's lifecycle state from what is on
//! the screen.
//!
//! Lifecycle hooks are the authoritative source of `working` / `blocked` /
//! `idle` — but not every agent has hooks, and those that do fire nothing on
//! some transitions (an interrupted turn, a prompt answered with plain text).
//! This module fills that gap: given the trimmed visible rows of a terminal
//! and its OSC title, it evaluates a small ordered rule table for the agent
//! kind occupying the pane and returns the state of the highest-priority rule
//! that matches. A known agent with no matching rule is `Unknown`, never a
//! guess — the sidebar would rather say "can't tell" than show a wrong state.
//!
//! Rules are plain data (one table per agent kind) so a UI change in an agent
//! is a one-line fix with a fixture test next to it, and `agent-explain` can
//! name the rule that fired.

use notmux_core::agent_state::AgentState;
use regex::Regex;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Which part of the screen a rule looks at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    /// Every visible row.
    Whole,
    /// The last `n` non-empty rows (the live agent UI sits at the bottom).
    LastNonEmpty(usize),
    /// The OSC window title.
    Title,
}

/// A predicate over the text of a region. Text comparisons are
/// case-insensitive; regular expressions are applied verbatim.
#[derive(Clone, Debug)]
pub enum Matcher {
    /// Substring anywhere in the region.
    Contains(&'static str),
    /// Regular expression over the whole region text (rows joined by `\n`).
    Regex(&'static str),
    /// Regular expression that must match at least one row on its own.
    LineRegex(&'static str),
    Not(Box<Matcher>),
    All(Vec<Matcher>),
    Any(Vec<Matcher>),
}

impl Matcher {
    fn eval(&self, text: &str, lower: &str) -> bool {
        match self {
            Matcher::Contains(s) => lower.contains(&s.to_ascii_lowercase()),
            Matcher::Regex(p) => compiled(p).is_match(text),
            Matcher::LineRegex(p) => {
                let re = compiled(p);
                text.lines().any(|l| re.is_match(l))
            }
            Matcher::Not(m) => !m.eval(text, lower),
            Matcher::All(ms) => ms.iter().all(|m| m.eval(text, lower)),
            Matcher::Any(ms) => ms.iter().any(|m| m.eval(text, lower)),
        }
    }
}

/// Compiled-regex cache: rule patterns are static, so compile each once.
fn compiled(pattern: &'static str) -> Regex {
    static CACHE: OnceLock<Mutex<HashMap<&'static str, Regex>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    cache
        .entry(pattern)
        .or_insert_with(|| {
            Regex::new(pattern).unwrap_or_else(|e| panic!("bad agent rule regex {pattern:?}: {e}"))
        })
        .clone()
}

#[derive(Clone, Debug)]
pub struct Rule {
    pub id: &'static str,
    pub state: AgentState,
    /// Higher wins when several rules match.
    pub priority: u16,
    pub region: Region,
    pub matcher: Matcher,
}

/// What the detector saw and decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detection {
    pub state: AgentState,
    /// The rule that produced `state`; `None` when nothing matched.
    pub rule: Option<&'static str>,
    /// The region text the winning rule evaluated (for explain output).
    pub region_text: String,
}

/// Screen input for one evaluation.
#[derive(Clone, Copy, Debug)]
pub struct ScreenSnapshot<'a> {
    /// Visible rows, top to bottom, trailing whitespace trimmed.
    pub lines: &'a [String],
    pub title: Option<&'a str>,
}

impl ScreenSnapshot<'_> {
    fn region_text(&self, region: Region) -> String {
        match region {
            Region::Whole => self.lines.join("\n"),
            Region::LastNonEmpty(n) => {
                let non_empty: Vec<&str> = self
                    .lines
                    .iter()
                    .map(String::as_str)
                    .filter(|l| !l.trim().is_empty())
                    .collect();
                let start = non_empty.len().saturating_sub(n);
                non_empty[start..].join("\n")
            }
            Region::Title => self.title.unwrap_or_default().to_string(),
        }
    }
}

/// Evaluate the rule table for `kind` against the screen. `None` when the
/// kind has no rule table (an agent we do not know how to read).
pub fn detect(kind: &str, screen: ScreenSnapshot<'_>) -> Option<Detection> {
    let rules = rules_for(kind)?;
    let mut best: Option<(&Rule, String)> = None;
    for rule in rules {
        if best.as_ref().is_some_and(|(b, _)| b.priority >= rule.priority) {
            continue;
        }
        let text = screen.region_text(rule.region);
        let lower = text.to_lowercase();
        if rule.matcher.eval(&text, &lower) {
            best = Some((rule, text));
        }
    }
    Some(match best {
        Some((rule, text)) => Detection {
            state: rule.state,
            rule: Some(rule.id),
            region_text: text,
        },
        None => Detection {
            state: AgentState::Unknown,
            rule: None,
            region_text: String::new(),
        },
    })
}

/// Agent kinds with a rule table.
pub fn known_kinds() -> &'static [&'static str] {
    &["claude", "codex", "gemini", "opencode", "cursor"]
}

/// Map a child process command line to an agent kind. Looks at the first
/// few argv tokens because interpreter launches (`node …/cli.js`) put the
/// agent after the runtime, and at package paths because npm shims name the
/// script, not the product.
pub fn kind_from_command_line(cmdline: &str) -> Option<&'static str> {
    let lower = cmdline.to_ascii_lowercase();
    for (needle, kind) in [
        ("@anthropic-ai/claude-code", "claude"),
        ("@openai/codex", "codex"),
        ("@google/gemini-cli", "gemini"),
        ("opencode-ai", "opencode"),
    ] {
        if lower.contains(needle) {
            return Some(kind);
        }
    }
    for token in lower.split_whitespace().take(3) {
        let base = token.rsplit(['/', '\\']).next().unwrap_or(token);
        let base = base.strip_suffix(".exe").unwrap_or(base);
        match base {
            "claude" => return Some("claude"),
            "codex" => return Some("codex"),
            "gemini" => return Some("gemini"),
            "opencode" => return Some("opencode"),
            "cursor-agent" | "cursor" => return Some("cursor"),
            "notagent" => return Some("notagent"),
            "pi" => return Some("pi"),
            "agy" | "antigravity" => return Some("antigravity"),
            "kimi" => return Some("kimi"),
            _ => {}
        }
    }
    None
}

fn rules_for(kind: &str) -> Option<&'static [Rule]> {
    static TABLES: OnceLock<HashMap<&'static str, Vec<Rule>>> = OnceLock::new();
    let tables = TABLES.get_or_init(build_tables);
    tables.get(kind).map(Vec::as_slice)
}

fn build_tables() -> HashMap<&'static str, Vec<Rule>> {
    use AgentState::*;
    use Matcher::*;
    let mut t = HashMap::new();

    // Claude Code: bottom-anchored input box with a `❯` prompt; a running
    // turn shows a spinner line ending in "esc to interrupt"; approval and
    // question dialogs offer numbered choices with "Esc to cancel"; the OSC
    // title carries a braille spinner glyph while busy and `✳` when ready.
    t.insert(
        "claude",
        vec![
            Rule {
                id: "claude.title_spinner_working",
                state: Working,
                priority: 110,
                region: Region::Title,
                matcher: Regex(r"^[\x{2800}-\x{28FF}\x{25D0}-\x{25D3}]"),
            },
            Rule {
                id: "claude.esc_to_interrupt_working",
                state: Working,
                priority: 100,
                region: Region::LastNonEmpty(12),
                matcher: LineRegex(r"(?i)esc to interrupt"),
            },
            Rule {
                id: "claude.dialog_blocked",
                state: Blocked,
                priority: 90,
                region: Region::LastNonEmpty(24),
                matcher: All(vec![
                    Contains("esc to cancel"),
                    Any(vec![
                        Contains("do you want to proceed"),
                        Contains("do you want to"),
                        Contains("enter to confirm"),
                        Contains("enter to select"),
                        Contains("would you like to"),
                    ]),
                ]),
            },
            Rule {
                id: "claude.permission_blocked",
                state: Blocked,
                priority: 85,
                region: Region::LastNonEmpty(24),
                matcher: Any(vec![
                    Contains("waiting for permission"),
                    Contains("do you want to allow this connection"),
                    Contains("needs your permission"),
                ]),
            },
            Rule {
                id: "claude.prompt_idle",
                state: Idle,
                priority: 50,
                region: Region::LastNonEmpty(8),
                matcher: All(vec![
                    LineRegex(r"^[\s│]*❯"),
                    Not(Box::new(Contains("esc to cancel"))),
                ]),
            },
            Rule {
                id: "claude.title_idle",
                state: Idle,
                priority: 40,
                region: Region::Title,
                matcher: Regex(r"^\x{2733}"),
            },
        ],
    );

    // Codex: "• Working (…s • esc to interrupt)" while a turn runs; approval
    // dialogs ask "Allow command?" / "Would you like to …" with y/n choices
    // and an (esc) escape; the composer prompt is `›`.
    t.insert(
        "codex",
        vec![
            Rule {
                id: "codex.working",
                state: Working,
                priority: 100,
                region: Region::LastNonEmpty(12),
                matcher: Any(vec![
                    LineRegex(r"(?i)esc to interrupt"),
                    LineRegex(r"(?i)^\s*[•·▪]?\s*working\b"),
                ]),
            },
            Rule {
                id: "codex.approval_blocked",
                state: Blocked,
                priority: 90,
                region: Region::LastNonEmpty(24),
                matcher: All(vec![
                    Any(vec![
                        Contains("allow command"),
                        Contains("would you like to"),
                        Contains("do you want to"),
                        Contains("approve"),
                    ]),
                    Any(vec![
                        Contains("yes (y)"),
                        Contains("(esc)"),
                        Contains("press enter"),
                        LineRegex(r"(?i)^\s*[›>]?\s*\d+\.\s*yes\b"),
                    ]),
                ]),
            },
            Rule {
                id: "codex.prompt_idle",
                state: Idle,
                priority: 50,
                region: Region::LastNonEmpty(8),
                matcher: LineRegex(r"^\s*›"),
            },
        ],
    );

    // Gemini CLI: a running turn shows "(esc to cancel, …s)" next to the
    // spinner; tool approvals offer "Yes, allow once" / "Yes, allow always";
    // the idle composer reads "> Type your message …".
    t.insert(
        "gemini",
        vec![
            Rule {
                id: "gemini.working",
                state: Working,
                priority: 100,
                region: Region::LastNonEmpty(12),
                matcher: LineRegex(r"(?i)esc to cancel,?\s*\d*\s*s?\)?"),
            },
            Rule {
                id: "gemini.approval_blocked",
                state: Blocked,
                priority: 90,
                region: Region::LastNonEmpty(24),
                matcher: Any(vec![
                    Contains("allow once"),
                    Contains("allow always"),
                    Contains("yes, allow"),
                    Contains("apply this change?"),
                ]),
            },
            Rule {
                id: "gemini.prompt_idle",
                state: Idle,
                priority: 50,
                region: Region::LastNonEmpty(8),
                matcher: LineRegex(r"(?i)^\s*>\s*(type your message|$)"),
            },
        ],
    );

    // OpenCode: a permission dialog names the permission and offers
    // accept/reject keys; a running turn shows "esc interrupt" in the footer;
    // the idle composer footer offers "enter send".
    t.insert(
        "opencode",
        vec![
            Rule {
                id: "opencode.working",
                state: Working,
                priority: 100,
                region: Region::LastNonEmpty(8),
                matcher: Any(vec![
                    Contains("esc interrupt"),
                    Contains("esc to interrupt"),
                ]),
            },
            Rule {
                id: "opencode.permission_blocked",
                state: Blocked,
                priority: 90,
                region: Region::LastNonEmpty(24),
                matcher: All(vec![
                    Contains("permission"),
                    Any(vec![Contains("accept"), Contains("allow"), Contains("reject")]),
                ]),
            },
            Rule {
                id: "opencode.prompt_idle",
                state: Idle,
                priority: 50,
                region: Region::LastNonEmpty(8),
                matcher: Any(vec![Contains("enter send"), Contains("enter to send")]),
            },
        ],
    );

    // Cursor Agent CLI: command approvals ask "Run this command?" with a
    // y/n choice; a running turn shows "esc to interrupt"; the composer
    // footer offers "enter to send".
    t.insert(
        "cursor",
        vec![
            Rule {
                id: "cursor.working",
                state: Working,
                priority: 100,
                region: Region::LastNonEmpty(12),
                matcher: LineRegex(r"(?i)esc to interrupt|generating"),
            },
            Rule {
                id: "cursor.approval_blocked",
                state: Blocked,
                priority: 90,
                region: Region::LastNonEmpty(24),
                matcher: Any(vec![
                    Contains("run this command?"),
                    Contains("apply this edit?"),
                    Contains("(y/n)"),
                ]),
            },
            Rule {
                id: "cursor.prompt_idle",
                state: Idle,
                priority: 50,
                region: Region::LastNonEmpty(8),
                matcher: Any(vec![Contains("enter to send"), LineRegex(r"^\s*[›>]\s*$")]),
            },
        ],
    );

    t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(|l| l.trim_end().to_string()).collect()
    }

    fn run(kind: &str, screen: &str, title: Option<&str>) -> Detection {
        let ls = lines(screen);
        detect(
            kind,
            ScreenSnapshot {
                lines: &ls,
                title,
            },
        )
        .expect("known kind")
    }

    #[test]
    fn unknown_kind_has_no_table() {
        let ls = lines("anything");
        assert!(detect("mystery-agent", ScreenSnapshot { lines: &ls, title: None }).is_none());
    }

    #[test]
    fn claude_working_from_interrupt_line() {
        let screen = "\n· Thinking… (12s · esc to interrupt)\n\n╭──────╮\n│ ❯    │\n╰──────╯\n";
        let d = run("claude", screen, None);
        assert_eq!(d.state, AgentState::Working);
        assert_eq!(d.rule, Some("claude.esc_to_interrupt_working"));
    }

    #[test]
    fn claude_working_from_title_spinner_beats_prompt() {
        let screen = "╭──────╮\n│ ❯    │\n╰──────╯";
        let d = run("claude", screen, Some("⠋ Editing files"));
        assert_eq!(d.state, AgentState::Working);
        assert_eq!(d.rule, Some("claude.title_spinner_working"));
    }

    #[test]
    fn claude_blocked_on_permission_dialog() {
        let screen = "Bash command\n\n  cargo test\n\nDo you want to proceed?\n❯ 1. Yes\n  2. No\n\nEsc to cancel";
        let d = run("claude", screen, None);
        assert_eq!(d.state, AgentState::Blocked);
        assert_eq!(d.rule, Some("claude.dialog_blocked"));
        assert!(d.region_text.contains("Do you want to proceed?"));
    }

    #[test]
    fn claude_idle_at_prompt() {
        let screen = "Done.\n\n╭──────────╮\n│ ❯        │\n╰──────────╯\n  ? for shortcuts";
        let d = run("claude", screen, Some("✳ Claude Code"));
        assert_eq!(d.state, AgentState::Idle);
        assert_eq!(d.rule, Some("claude.prompt_idle"));
    }

    #[test]
    fn claude_unknown_when_nothing_matches() {
        let screen = "some transcript text\nwith no prompt box";
        let d = run("claude", screen, None);
        assert_eq!(d.state, AgentState::Unknown);
        assert_eq!(d.rule, None);
    }

    #[test]
    fn codex_states() {
        let d = run("codex", "• Working (8s • esc to interrupt)\n\n› ", None);
        assert_eq!(d.state, AgentState::Working);
        let d = run(
            "codex",
            "Allow command?\n\n  git push\n\n› 1. Yes (y)\n  2. No, and tell Codex what to do differently (esc)",
            None,
        );
        assert_eq!(d.state, AgentState::Blocked);
        let d = run("codex", "Done.\n\n› ", None);
        assert_eq!(d.state, AgentState::Idle);
    }

    #[test]
    fn gemini_states() {
        let d = run("gemini", "⠋ Reading files (esc to cancel, 4s)", None);
        assert_eq!(d.state, AgentState::Working);
        let d = run("gemini", "Apply this change?\n● 1. Yes, allow once\n  2. Yes, allow always\n  3. No", None);
        assert_eq!(d.state, AgentState::Blocked);
        let d = run("gemini", "> Type your message or @path/to/file", None);
        assert_eq!(d.state, AgentState::Idle);
    }

    #[test]
    fn last_non_empty_region_ignores_blank_padding() {
        let ls = lines("a\n\n\nb\n\nc\n\n");
        let s = ScreenSnapshot { lines: &ls, title: None };
        assert_eq!(s.region_text(Region::LastNonEmpty(2)), "b\nc");
        assert_eq!(s.region_text(Region::LastNonEmpty(10)), "a\nb\nc");
    }

    #[test]
    fn command_line_kind_mapping() {
        assert_eq!(kind_from_command_line("claude --resume abc"), Some("claude"));
        assert_eq!(
            kind_from_command_line("node /usr/lib/node_modules/@anthropic-ai/claude-code/cli.js"),
            Some("claude")
        );
        assert_eq!(kind_from_command_line("/opt/homebrew/bin/codex resume 1"), Some("codex"));
        assert_eq!(kind_from_command_line("cursor-agent"), Some("cursor"));
        assert_eq!(kind_from_command_line("C:\\tools\\gemini.exe"), Some("gemini"));
        assert_eq!(kind_from_command_line("vim main.rs"), None);
        assert_eq!(kind_from_command_line("cargo build"), None);
    }
}
