//! App-level agent detection pass.
//!
//! Once a second, walk every live terminal — not just the rendered panes, so
//! a blocked agent in a project that is scrolled or collapsed out of view
//! still lights up its sidebar row — and:
//!
//! 1. identify the agent occupying the terminal from the shell's child
//!    processes (cheap: only when the screen changed or every fifth tick),
//! 2. classify its lifecycle state from the visible screen with
//!    `agent_detect` when no lifecycle hook currently owns the state,
//! 3. drop the agent bookkeeping once the agent process is gone, so a stale
//!    spinner or badge cannot outlive the agent.
//!
//! Hook reports stay authoritative: while the last transition came from a
//! hook, screen detection only steps in to move a `blocked` terminal to
//! `working`/`idle` when the dialog demonstrably went away — the case hooks
//! do not cover (a prompt answered with plain text, an Esc).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use gpui::*;
use notmux_core::agent_state::{AgentState, AgentStateSource};
use notmux_terminal::agent_detect::{self, ScreenSnapshot};
use notmux_terminal::terminal::{Terminal, child_process_commands};

use super::NotMux;

/// Cadence of the pass.
const TICK: Duration = Duration::from_secs(1);
/// Re-probe child processes every this many ticks even without screen
/// changes (catches an agent that exited silently).
const PROBE_EVERY_TICKS: u64 = 5;
/// Consecutive probes without an agent child before the terminal is
/// considered agent-free (one probe can race a re-exec).
const EXIT_MISSES: u8 = 2;

#[derive(Default)]
struct Seen {
    generation: Option<u64>,
    title: Option<String>,
    misses: u8,
}

struct Transition {
    terminal_id: String,
    from: Option<AgentState>,
    to: Option<AgentState>,
    source: &'static str,
    rule: Option<&'static str>,
}

impl NotMux {
    pub(super) fn start_agent_detection_loop(&mut self, cx: &mut Context<Self>) {
        let terminals = self.terminals.clone();
        let workspace = self.workspace.clone();
        cx.spawn(async move |_this: WeakEntity<NotMux>, cx: &mut AsyncApp| {
            let mut seen: HashMap<String, Seen> = HashMap::new();
            let mut tick: u64 = 0;
            loop {
                smol::Timer::after(TICK).await;
                tick += 1;

                // Short lock: copy the handles out, work outside.
                let live: Vec<(String, Arc<Terminal>)> = terminals
                    .lock()
                    .iter()
                    .map(|(id, t)| (id.clone(), t.clone()))
                    .collect();
                seen.retain(|id, _| live.iter().any(|(k, _)| k == id));

                let mut transitions: Vec<Transition> = Vec::new();
                for (id, term) in live {
                    let entry = seen.entry(id.clone()).or_default();
                    let generation = term.content_generation();
                    let title = term.title();
                    let screen_changed =
                        entry.generation != Some(generation) || entry.title != title;
                    entry.generation = Some(generation);
                    entry.title = title.clone();

                    // 1. Who is in there?
                    let kind_before = term.agent_kind();
                    let probe_due = match kind_before {
                        None => screen_changed || tick % PROBE_EVERY_TICKS == 0,
                        Some(_) => tick % PROBE_EVERY_TICKS == 0,
                    };
                    if probe_due && let Some(pid) = term.shell_pid() {
                        let found = smol::unblock(move || child_process_commands(pid))
                            .await
                            .map(|cmds| {
                                cmds.iter()
                                    .find_map(|c| agent_detect::kind_from_command_line(c))
                            });
                        match found {
                            // Cannot look (no probe on this platform): leave
                            // hook-driven bookkeeping alone.
                            None => {}
                            Some(Some(kind)) => {
                                entry.misses = 0;
                                term.set_agent_kind(Some(kind.to_string()));
                            }
                            Some(None) if kind_before.is_some() => {
                                entry.misses = entry.misses.saturating_add(1);
                                if entry.misses >= EXIT_MISSES {
                                    let from = term.agent_state();
                                    term.set_agent_state(None, AgentStateSource::Screen);
                                    entry.misses = 0;
                                    if from.is_some() {
                                        transitions.push(Transition {
                                            terminal_id: id.clone(),
                                            from,
                                            to: None,
                                            source: "exit",
                                            rule: None,
                                        });
                                    }
                                }
                            }
                            Some(None) => {}
                        }
                    }

                    // 2. What is it doing?
                    let Some(kind) = term.agent_kind() else {
                        continue;
                    };
                    if !screen_changed {
                        continue;
                    }
                    let runtime = term.agent_runtime();
                    let hook_owned = runtime.source == Some(AgentStateSource::Hook);
                    if hook_owned && runtime.state != Some(AgentState::Blocked) {
                        continue;
                    }
                    let lines = term.screen_text_lines();
                    let Some(detection) = agent_detect::detect(
                        &kind,
                        ScreenSnapshot {
                            lines: &lines,
                            title: title.as_deref(),
                        },
                    ) else {
                        continue;
                    };
                    // Under hook authority only definite "the dialog is
                    // gone" evidence may override a reported `blocked`.
                    if hook_owned
                        && !matches!(detection.state, AgentState::Working | AgentState::Idle)
                    {
                        continue;
                    }
                    let from = term.agent_state();
                    if term.set_agent_state(Some(detection.state), AgentStateSource::Screen) {
                        transitions.push(Transition {
                            terminal_id: id.clone(),
                            from,
                            to: term.agent_state(),
                            source: "screen",
                            rule: detection.rule,
                        });
                    }
                }

                if transitions.is_empty() {
                    continue;
                }
                let project_ids: Vec<Option<String>> = cx.update(|cx| {
                    let ids = {
                        let ws = workspace.read(cx);
                        transitions
                            .iter()
                            .map(|t| {
                                ws.find_project_for_terminal(&t.terminal_id)
                                    .map(|p| p.id.clone())
                            })
                            .collect()
                    };
                    // Sidebar rollups, pane rings, and remote clients
                    // (`state_version`) all hang off workspace notifications.
                    workspace.update(cx, |_ws, cx| cx.notify());
                    cx.refresh_windows();
                    ids
                });
                for (t, project_id) in transitions.iter().zip(project_ids) {
                    crate::event_log::emit(
                        "agent_state",
                        serde_json::json!({
                            "terminal_id": t.terminal_id,
                            "project_id": project_id,
                            "from": t.from,
                            "to": t.to,
                            "source": t.source,
                            "rule": t.rule,
                        }),
                    );
                }
            }
        })
        .detach();
    }
}
