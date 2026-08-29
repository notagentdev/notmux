//! App-level agent detection pass and the lifecycle event drain.
//!
//! Once a second, walk every live terminal — not just the rendered panes, so
//! a blocked agent in a project that is scrolled or collapsed out of view
//! still lights up its sidebar row — and:
//!
//! 1. identify the agent occupying the terminal. The shell's child processes
//!    are the ground truth wherever a probe exists; the session store
//!    written by the agents' own hooks (keyed by layout slot) names the kind
//!    exactly when the probe sees an agent, and is the only source on
//!    platforms without a probe. Probing happens when the screen changed or
//!    every fifth tick,
//! 2. classify its lifecycle state from the visible screen with
//!    `agent_detect` when no lifecycle hook currently owns the state,
//! 3. drop the agent bookkeeping once the agent is gone — two consecutive
//!    probes without an agent child (a "live" store record does not veto
//!    this: integrations without a reliable session-end hook would otherwise
//!    pin the state forever), or, without a probe, a session end recorded
//!    in the store — so a stale spinner or badge cannot outlive the agent.
//!
//! Hook reports stay authoritative: while the last transition came from a
//! hook, screen detection only steps in to move a `blocked` terminal to
//! `working`/`idle` when the dialog demonstrably went away — the case hooks
//! do not cover (a prompt answered with plain text, an Esc).
//!
//! The same tick drains `agent_events`: every effective transition recorded
//! by `Terminal` (hook, screen, exit, notification, seen, interrupt) is
//! resolved to its project and appended to the event log, and the workspace
//! is notified so sidebar rollups and remote clients repaint. Both the GUI
//! and the headless server run this loop.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use gpui::*;
use notmux_core::agent_state::{AgentState, AgentStateSource};
use notmux_terminal::TerminalsRegistry;
use notmux_terminal::agent_detect::{self, ScreenSnapshot};
use notmux_terminal::agent_events;
use notmux_terminal::agent_sessions::{self, AgentSlotRecord};
use notmux_terminal::terminal::{Terminal, child_process_commands};
use notmux_workspace::state::Workspace;

use super::NotMux;

/// Cadence of the pass.
const TICK: Duration = Duration::from_secs(1);
/// Re-probe identity every this many ticks even without screen changes
/// (catches an agent that exited silently).
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

/// What the session store knows about a slot: the agent kind of its live
/// session, or that the session ended.
enum StoreView {
    Live(String),
    Ended,
    Nothing,
}

fn store_view(store: &HashMap<String, AgentSlotRecord>, slot: &str) -> StoreView {
    let Some(rec) = store.get(slot) else {
        return StoreView::Nothing;
    };
    let latest = rec.pending.as_ref().or(rec.confirmed.as_ref());
    match latest {
        Some(r) if !r.ended => StoreView::Live(r.kind.clone()),
        Some(_) => StoreView::Ended,
        None => StoreView::Nothing,
    }
}

/// Outcome of one identity probe.
struct Identity {
    /// The agent kind occupying the terminal, when one is present.
    kind: Option<String>,
    /// This probe found no agent (counts towards the exit threshold).
    miss: bool,
    /// Definitive exit signal without a process probe.
    ended: bool,
}

/// Combine the process probe (`None` = no probe on this platform,
/// `Some(None)` = looked, no agent child) with the session store.
fn identify(probe: Option<Option<String>>, store: StoreView) -> Identity {
    match (probe, store) {
        // The probe saw an agent: the store names it exactly when it can.
        (Some(Some(_)), StoreView::Live(kind)) => Identity { kind: Some(kind), miss: false, ended: false },
        (Some(Some(kind)), _) => Identity { kind: Some(kind), miss: false, ended: false },
        // The probe looked and found nothing — a "live" store record does
        // not override that (no reliable session-end hook everywhere).
        (Some(None), _) => Identity { kind: None, miss: true, ended: false },
        // No probe: the store is all there is.
        (None, StoreView::Live(kind)) => Identity { kind: Some(kind), miss: false, ended: false },
        (None, StoreView::Ended) => Identity { kind: None, miss: false, ended: true },
        (None, StoreView::Nothing) => Identity { kind: None, miss: false, ended: false },
    }
}

impl NotMux {
    pub(super) fn start_agent_detection_loop(&mut self, cx: &mut Context<Self>) {
        spawn_agent_detection(self.terminals.clone(), self.workspace.clone(), cx);
    }
}

/// Start the detection/drain loop. Shared by the GUI app and the headless
/// server.
pub(super) fn spawn_agent_detection(
    terminals: TerminalsRegistry,
    workspace: Entity<Workspace>,
    cx: &mut App,
) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        let mut seen: HashMap<String, Seen> = HashMap::new();
        let mut tick: u64 = 0;
        loop {
            smol::Timer::after(TICK).await;
            tick += 1;
            let probe_tick = tick.is_multiple_of(PROBE_EVERY_TICKS);

            // Short lock: copy the handles out, work outside.
            let live: Vec<(String, Arc<Terminal>)> = terminals
                .lock()
                .iter()
                .map(|(id, t)| (id.clone(), t.clone()))
                .collect();
            seen.retain(|id, _| live.iter().any(|(k, _)| k == id));

            // terminal id → (project id, layout slot) for the session store
            // lookup and the event log.
            let placement: HashMap<String, (String, Option<String>)> = cx.update(|cx| {
                let ws = workspace.read(cx);
                live.iter()
                    .filter_map(|(id, _)| {
                        let project = ws.find_project_for_terminal(id)?;
                        let slot = project
                            .layout
                            .as_ref()
                            .and_then(|l| l.find_terminal_path(id).and_then(|p| l.get_at_path(&p)))
                            .and_then(|n| n.slot_id().map(str::to_string));
                        Some((id.clone(), (project.id.clone(), slot)))
                    })
                    .collect()
            });

            // The session store is read at most once per tick, and only on
            // ticks that probe identity.
            let mut store: Option<HashMap<String, AgentSlotRecord>> = None;

            let mut changed = false;
            for (id, term) in live {
                let entry = seen.entry(id.clone()).or_default();
                let generation = term.content_generation();
                let title = term.title();
                let screen_changed = entry.generation != Some(generation) || entry.title != title;
                entry.generation = Some(generation);
                entry.title = title.clone();

                // 1. Who is in there?
                let kind_before = term.agent_kind();
                let probe_due = match kind_before {
                    None => screen_changed || probe_tick,
                    Some(_) => probe_tick,
                };
                if probe_due {
                    let slot = placement.get(&id).and_then(|(_, s)| s.clone());
                    let from_store = match slot {
                        Some(slot) => {
                            let store = store.get_or_insert_with(agent_sessions::load_store);
                            store_view(store, &slot)
                        }
                        None => StoreView::Nothing,
                    };
                    let from_probe = match term.shell_pid() {
                        Some(pid) => smol::unblock(move || child_process_commands(pid))
                            .await
                            .map(|cmds| {
                                cmds.iter()
                                    .find_map(|c| agent_detect::kind_from_command_line(c))
                                    .map(str::to_string)
                            }),
                        None => None,
                    };
                    let identity = identify(from_probe, from_store);
                    if identity.miss {
                        entry.misses = entry.misses.saturating_add(1);
                    }
                    let gone = identity.ended || entry.misses >= EXIT_MISSES;
                    match identity.kind {
                        Some(kind) => {
                            entry.misses = 0;
                            term.set_agent_kind(Some(kind));
                        }
                        None if gone && kind_before.is_some() => {
                            entry.misses = 0;
                            if term.set_agent_state(None, AgentStateSource::Screen) {
                                changed = true;
                            }
                        }
                        None => {}
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
                // Under hook authority only definite "the dialog is gone"
                // evidence may override a reported `blocked`.
                if hook_owned && !matches!(detection.state, AgentState::Working | AgentState::Idle) {
                    continue;
                }
                if term.set_agent_state_detailed(
                    Some(detection.state),
                    AgentStateSource::Screen,
                    "screen",
                    detection.rule,
                ) {
                    changed = true;
                }
            }

            // 3. Drain the transition queue into the event log. Anything that
            // moved an effective state — including badge arrivals and the
            // user dismissing them in a pane — shows up here.
            let transitions = agent_events::drain();
            if transitions.is_empty() && !changed {
                continue;
            }
            for t in &transitions {
                crate::event_log::emit(
                    "agent_state",
                    serde_json::json!({
                        "terminal_id": t.terminal_id,
                        "project_id": placement.get(&t.terminal_id).map(|(p, _)| p.clone()),
                        "from": t.from,
                        "to": t.to,
                        "source": t.cause,
                        "rule": t.rule,
                    }),
                );
            }
            // Sidebar rollups, pane rings, and remote clients
            // (`state_version`) all hang off workspace notifications.
            cx.update(|cx| {
                workspace.update(cx, |_ws, cx| cx.notify());
                cx.refresh_windows();
            });
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: that would pull in `gpui::test` and shadow the
    // built-in `#[test]`.
    use super::{StoreView, identify};

    #[test]
    fn probe_outranks_a_live_store_record() {
        // Store says live, probe looked and found no agent → a miss, not an
        // identification, so a stale hook state can expire.
        let id = identify(Some(None), StoreView::Live("codex".into()));
        assert!(id.kind.is_none());
        assert!(id.miss);
        assert!(!id.ended);
    }

    #[test]
    fn store_names_the_kind_when_the_probe_sees_an_agent() {
        let id = identify(Some(Some("node".into())), StoreView::Live("claude".into()));
        assert_eq!(id.kind.as_deref(), Some("claude"));
        let id = identify(Some(Some("gemini".into())), StoreView::Nothing);
        assert_eq!(id.kind.as_deref(), Some("gemini"));
    }

    #[test]
    fn without_a_probe_the_store_decides() {
        let id = identify(None, StoreView::Live("claude".into()));
        assert_eq!(id.kind.as_deref(), Some("claude"));
        let id = identify(None, StoreView::Ended);
        assert!(id.kind.is_none());
        assert!(id.ended);
        let id = identify(None, StoreView::Nothing);
        assert!(id.kind.is_none() && !id.miss && !id.ended);
    }
}
