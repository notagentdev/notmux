//! Queue of effective agent-state transitions.
//!
//! Every path that can change a terminal's *effective* state — hook reports,
//! screen detection, agent exit, a notification arriving (idle → done), the
//! user dismissing it or answering a prompt (done/blocked → idle) — funnels
//! through `Terminal`, which pushes one record here per change. The app
//! drains the queue on its detection tick, resolves project ids, and writes
//! `agent_state` events, so the event log is exhaustive by construction
//! rather than by remembering to emit at every call site (pane views, for
//! one, cannot reach the event log at all).

use notmux_core::agent_state::AgentState;
use std::sync::Mutex;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentTransition {
    pub terminal_id: String,
    pub from: Option<AgentState>,
    pub to: Option<AgentState>,
    /// What moved the state: `hook`, `screen`, `exit`, `notification`
    /// (a badge arrived), `seen` (the user dismissed it / answered),
    /// `interrupt` (Esc / Ctrl+C in the pane).
    pub cause: &'static str,
    /// The screen rule that fired, for `screen` transitions.
    pub rule: Option<&'static str>,
}

static QUEUE: Mutex<Vec<AgentTransition>> = Mutex::new(Vec::new());

/// Upper bound on queued records while nothing drains (e.g. headless without
/// the detection loop); older records are dropped first.
const CAP: usize = 4096;

pub fn push(transition: AgentTransition) {
    let mut q = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    if q.len() >= CAP {
        let overflow = q.len() + 1 - CAP;
        q.drain(..overflow);
    }
    q.push(transition);
}

/// Take everything queued so far, in order.
pub fn drain() -> Vec<AgentTransition> {
    let mut q = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    std::mem::take(&mut *q)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_is_fifo_and_drains_completely() {
        drain();
        push(AgentTransition {
            terminal_id: "t1".into(),
            from: None,
            to: Some(AgentState::Working),
            cause: "hook",
            rule: None,
        });
        push(AgentTransition {
            terminal_id: "t1".into(),
            from: Some(AgentState::Working),
            to: Some(AgentState::Blocked),
            cause: "screen",
            rule: Some("claude.dialog_blocked"),
        });
        let got = drain();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].to, Some(AgentState::Working));
        assert_eq!(got[1].rule, Some("claude.dialog_blocked"));
        assert!(drain().is_empty());
    }
}
