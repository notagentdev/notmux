use crate::orchestration::runtime::{Intent, Plan};
use crate::remote::types::{ActionRequest, BrowserRequest};
use notmux_core::orchestration::{AgentError, AgentRequest, Caller};
use tokio::sync::oneshot;

/// One step of the agent orchestration API, executed on the GPUI thread.
#[derive(Debug)]
pub enum AgentCommand {
    /// Phase 1 of a launch: validate and allocate.
    Plan { caller: Caller, request: AgentRequest },
    /// Launch step on the UI thread: start the process, insert the pane.
    /// No store access.
    Start { intent: Box<Intent> },
    /// Remove the pane of a start that could not be recorded.
    RemovePane { project_id: String, slot_id: String },
    /// `(project id, path)` of a terminal, for `register`.
    ProjectForTerminal { terminal_id: String },
}

#[derive(Debug)]
pub enum AgentReply {
    Plan(Result<Plan, AgentError>),
    Started(Result<(), String>),
    Project(Option<(String, String)>),
    Done,
}

/// Commands sent from the axum server to the GPUI main thread.
/// Fire-and-forget commands (SendText, Resize, etc.) set `reply` to `None`
/// to skip the oneshot allocation and avoid blocking the sender.
pub struct BridgeMessage {
    pub command: RemoteCommand,
    pub reply: Option<oneshot::Sender<CommandResult>>,
}

/// All operations the remote API can request.
#[derive(Debug)]
pub enum RemoteCommand {
    /// All client-facing actions (workspace + I/O).
    Action(ActionRequest),
    /// Get the full workspace state snapshot.
    GetState,
    /// Render a terminal's visible content as ANSI bytes (for snapshots).
    RenderSnapshot { terminal_id: String },
    /// Get current grid sizes (cols, rows) for multiple terminals.
    GetTerminalSizes { terminal_ids: Vec<String> },
    /// One automation command against a browser pane.
    /// Answered asynchronously (page JavaScript), not by the sync match.
    Browser(BrowserRequest),
    /// Agent orchestration (`/v1/agents`).
    Agent(AgentCommand),
}

/// Result of processing a RemoteCommand.
#[derive(Debug)]
pub enum CommandResult {
    /// Success with optional JSON-serializable payload.
    Ok(Option<serde_json::Value>),
    /// Success with raw bytes (e.g., terminal snapshots).
    OkBytes(Vec<u8>),
    /// Error with a human-readable message.
    Err(String),
    /// Typed answer of an agent command.
    Agent(AgentReply),
}

/// Channel types for the bridge.
pub type BridgeSender = async_channel::Sender<BridgeMessage>;
pub type BridgeReceiver = async_channel::Receiver<BridgeMessage>;

/// Create a new bridge channel pair (bounded to prevent memory exhaustion).
pub fn bridge_channel() -> (BridgeSender, BridgeReceiver) {
    async_channel::bounded(256)
}
