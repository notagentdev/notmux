//! Agent lifecycle state shared by the terminal runtime, the sidebar, the
//! remote API, and the CLI.
//!
//! One vocabulary and one precedence for everybody: a terminal is `working`
//! while its agent runs a turn, `blocked` while the agent waits for a decision
//! (permission, question), `done` once a turn finished and nobody has looked
//! at the pane since, `idle` when the agent is ready for input, and `unknown`
//! when an agent is present but its state cannot be classified. A terminal
//! without an agent has no state at all (`None` wherever an
//! `Option<AgentState>` is carried).

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Working,
    Blocked,
    Done,
    Idle,
    Unknown,
}

impl AgentState {
    /// Wire/CLI spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            AgentState::Working => "working",
            AgentState::Blocked => "blocked",
            AgentState::Done => "done",
            AgentState::Idle => "idle",
            AgentState::Unknown => "unknown",
        }
    }

    /// Parse the wire spelling plus the aliases the `agent-status` CLI has
    /// always accepted (`busy`/`running`/`start` for working,
    /// `complete`/`stop` for idle).
    pub fn parse(s: &str) -> Option<AgentState> {
        match s.trim().to_ascii_lowercase().as_str() {
            "working" | "busy" | "running" | "start" => Some(AgentState::Working),
            "blocked" | "waiting" | "needs_input" | "needs-input" => Some(AgentState::Blocked),
            "done" | "complete" | "stop" | "finished" => Some(AgentState::Done),
            "idle" | "ready" => Some(AgentState::Idle),
            "unknown" => Some(AgentState::Unknown),
            _ => None,
        }
    }

    /// How urgently the state wants the user's attention. Drives the sidebar
    /// rollup: a blocked agent outranks a working one, which outranks a
    /// finished-but-unseen one; an unclassifiable agent still beats plain
    /// idle because it may be stuck.
    pub fn urgency(self) -> u8 {
        match self {
            AgentState::Blocked => 4,
            AgentState::Working => 3,
            AgentState::Done => 2,
            AgentState::Unknown => 1,
            AgentState::Idle => 0,
        }
    }

    /// Reduce the states of several terminals to the one a container row
    /// (project, folder, pinned list) should show. `None` when no terminal
    /// hosts an agent.
    pub fn rollup<I>(states: I) -> Option<AgentState>
    where
        I: IntoIterator<Item = Option<AgentState>>,
    {
        states
            .into_iter()
            .flatten()
            .max_by_key(|s| s.urgency())
    }

    /// True for the states the default `wait` accepts: the agent has settled
    /// and either needs the user or is ready for the next prompt.
    pub fn is_settled(self) -> bool {
        matches!(
            self,
            AgentState::Blocked | AgentState::Done | AgentState::Idle
        )
    }
}

impl std::fmt::Display for AgentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Who last decided a terminal's agent state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStateSource {
    /// A lifecycle hook installed into the agent reported the transition.
    /// Authoritative: screen detection stands down while the agent lives.
    Hook,
    /// The rule-based reading of the visible screen classified the state.
    Screen,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollup_prefers_the_most_urgent_state() {
        let states = [
            Some(AgentState::Idle),
            Some(AgentState::Working),
            Some(AgentState::Blocked),
            Some(AgentState::Done),
        ];
        assert_eq!(AgentState::rollup(states), Some(AgentState::Blocked));

        let states = [Some(AgentState::Idle), Some(AgentState::Done), None];
        assert_eq!(AgentState::rollup(states), Some(AgentState::Done));

        let states = [Some(AgentState::Idle), Some(AgentState::Unknown)];
        assert_eq!(AgentState::rollup(states), Some(AgentState::Unknown));

        let states = [Some(AgentState::Working), Some(AgentState::Done)];
        assert_eq!(AgentState::rollup(states), Some(AgentState::Working));
    }

    #[test]
    fn rollup_is_none_without_agents() {
        assert_eq!(AgentState::rollup([None, None]), None);
        assert_eq!(AgentState::rollup(Vec::<Option<AgentState>>::new()), None);
    }

    #[test]
    fn parse_accepts_wire_names_and_legacy_aliases() {
        assert_eq!(AgentState::parse("working"), Some(AgentState::Working));
        assert_eq!(AgentState::parse("busy"), Some(AgentState::Working));
        assert_eq!(AgentState::parse("BLOCKED"), Some(AgentState::Blocked));
        assert_eq!(AgentState::parse("stop"), Some(AgentState::Done));
        assert_eq!(AgentState::parse("idle"), Some(AgentState::Idle));
        assert_eq!(AgentState::parse("unknown"), Some(AgentState::Unknown));
        assert_eq!(AgentState::parse("bogus"), None);
    }

    #[test]
    fn serde_uses_snake_case() {
        assert_eq!(
            serde_json::to_string(&AgentState::Blocked).unwrap(),
            "\"blocked\""
        );
        let parsed: AgentState = serde_json::from_str("\"done\"").unwrap();
        assert_eq!(parsed, AgentState::Done);
        assert_eq!(
            serde_json::to_string(&AgentStateSource::Screen).unwrap(),
            "\"screen\""
        );
    }
}
