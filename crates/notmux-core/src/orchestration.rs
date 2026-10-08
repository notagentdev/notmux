//! Agent orchestration contract shared by the application, the remote bridge,
//! the `notmux agent` CLI, and the pane UI.
//!
//! One root orchestrates up to [`MAX_WORKERS_PER_ROOT`] direct workers. Workers
//! are leaves: the runtime rejects spawn, register, and assign from a worker
//! before any side effect. Every mutating request carries a caller-generated
//! request ID so a retried request after a lost response never creates a
//! second worker, message, or result.
//!
//! Three things are kept apart on purpose because they answer different
//! questions: the outcome of an assignment ([`AssignmentState`]), the lifetime
//! of the native process ([`ProcessState`]), and whether the worker can take
//! new work ([`WorkerAvailability`]). The advisory attention state from hooks
//! and screen rules (`AgentState`) is not part of this contract at all.
//!
//! Nothing in this module depends on GPUI, the store, or the terminal runtime.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Live workers (starting, running, or completed-but-live) one root may own.
pub const MAX_WORKERS_PER_ROOT: usize = 4;
/// Live workers across the whole NotMux instance.
pub const MAX_WORKERS_PER_INSTANCE: usize = 8;
/// Assignments that may wait on one worker while another one runs.
pub const MAX_QUEUED_ASSIGNMENTS_PER_WORKER: usize = 16;
/// Messages a recipient may hold unacknowledged before senders are refused.
pub const MAX_UNACKNOWLEDGED_MESSAGES: usize = 1024;
/// UTF-8 bytes of one task document, before the bootstrap is prepended.
pub const MAX_TASK_BYTES: usize = 64 * 1024;
/// UTF-8 bytes of one message body.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
/// UTF-8 bytes of one reported result.
pub const MAX_RESULT_BYTES: usize = 256 * 1024;
/// Entries one page of messages, assignments, or events may carry.
pub const MAX_PAGE_SIZE: usize = 64;
/// Display names are labels, not identities; keep them short.
pub const MAX_NAME_CHARS: usize = 80;
/// `wait` and `next` block at most this long unless the caller says otherwise.
pub const DEFAULT_WAIT_TIMEOUT_MS: u64 = 60_000;
/// Upper bound a caller may ask `wait` or `next` to block.
pub const MAX_WAIT_TIMEOUT_MS: u64 = 30 * 60 * 1000;
/// A launch that has not produced a live process by then is failed.
pub const LAUNCH_DEADLINE_MS: u64 = 30_000;

/// Environment variables the runtime reserves for managed processes. A
/// caller-supplied environment may never override them, and ordinary shells
/// never inherit them.
pub const ENV_RUN_ID: &str = "NOTMUX_RUN_ID";
pub const ENV_PARENT_RUN_ID: &str = "NOTMUX_PARENT_RUN_ID";
pub const ENV_TASK_FILE: &str = "NOTMUX_TASK_FILE";
pub const ENV_AGENT_ROLE: &str = "NOTMUX_AGENT_ROLE";
pub const ENV_AGENT_HARNESS: &str = "NOTMUX_AGENT_HARNESS";
pub const RESERVED_ENV: [&str; 5] = [
    ENV_RUN_ID,
    ENV_PARENT_RUN_ID,
    ENV_TASK_FILE,
    ENV_AGENT_ROLE,
    ENV_AGENT_HARNESS,
];

// ── Enumerations ────────────────────────────────────────────────────────────

/// Which CLI a run executes. `Generic` is any executable with argv and
/// exit-code completion; it has no adapter behaviour beyond launching.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessKind {
    Notagent,
    Claude,
    Codex,
    Generic,
}

impl HarnessKind {
    pub const ALL: [HarnessKind; 4] = [
        HarnessKind::Notagent,
        HarnessKind::Claude,
        HarnessKind::Codex,
        HarnessKind::Generic,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            HarnessKind::Notagent => "notagent",
            HarnessKind::Claude => "claude",
            HarnessKind::Codex => "codex",
            HarnessKind::Generic => "generic",
        }
    }

    pub fn parse(s: &str) -> Option<HarnessKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "notagent" => Some(HarnessKind::Notagent),
            "claude" | "claude-code" => Some(HarnessKind::Claude),
            "codex" => Some(HarnessKind::Codex),
            "generic" | "exec" => Some(HarnessKind::Generic),
            _ => None,
        }
    }

    /// Whether the harness can take part in the mailbox protocol at all.
    /// A generic executable only runs and exits.
    pub fn is_cooperating(self) -> bool {
        !matches!(self, HarnessKind::Generic)
    }
}

impl fmt::Display for HarnessKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How follow-up work reaches a worker. Both modes are pull-based; the
/// runtime never types into a PTY.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryMode {
    /// The worker blocks in `notmux agent next`.
    Cooperative,
    /// A vendor hook returns the next assignment into the running
    /// conversation; on timeout it hands over to `Cooperative`.
    Hook,
}

impl DeliveryMode {
    pub fn as_str(self) -> &'static str {
        match self {
            DeliveryMode::Cooperative => "cooperative",
            DeliveryMode::Hook => "hook",
        }
    }
}

/// What ends an assignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionMode {
    /// The worker (or its root on its behalf) reports through `finish`.
    Reported,
    /// The process exit code decides. Only for `Generic` runs.
    ExitCode,
}

impl CompletionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            CompletionMode::Reported => "reported",
            CompletionMode::ExitCode => "exit_code",
        }
    }

    pub fn parse(s: &str) -> Option<CompletionMode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "reported" => Some(CompletionMode::Reported),
            "exit_code" | "exit-code" | "exit" => Some(CompletionMode::ExitCode),
            _ => None,
        }
    }
}

/// Who a run is in its group.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum RunRole {
    Root,
    Worker { root_id: String },
}

impl RunRole {
    pub fn is_root(&self) -> bool {
        matches!(self, RunRole::Root)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            RunRole::Root => "root",
            RunRole::Worker { .. } => "worker",
        }
    }
}

/// Outcome of one assignment. Terminal states never change again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssignmentState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
}

impl AssignmentState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            AssignmentState::Succeeded
                | AssignmentState::Failed
                | AssignmentState::Cancelled
                | AssignmentState::Interrupted
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            AssignmentState::Queued => "queued",
            AssignmentState::Running => "running",
            AssignmentState::Succeeded => "succeeded",
            AssignmentState::Failed => "failed",
            AssignmentState::Cancelled => "cancelled",
            AssignmentState::Interrupted => "interrupted",
        }
    }
}

/// Lifetime of the native process behind a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    /// Accepted, launch not yet confirmed.
    Starting,
    Running,
    Exited,
    /// Launch failed, or the application restarted while it was live.
    Lost,
}

impl ProcessState {
    pub fn is_live(self) -> bool {
        matches!(self, ProcessState::Starting | ProcessState::Running)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ProcessState::Starting => "starting",
            ProcessState::Running => "running",
            ProcessState::Exited => "exited",
            ProcessState::Lost => "lost",
        }
    }
}

/// Whether a worker can take an assignment right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerAvailability {
    Starting,
    /// Working on the current assignment.
    Busy,
    /// Blocked in `next` or a hook handoff; a queued assignment is claimed
    /// immediately.
    Waiting,
    /// Between assignments without an active handoff. Queued work waits.
    Idle,
    /// Explicitly stopped, cancelled, exited, or interrupted; no more work.
    Stopped,
}

impl WorkerAvailability {
    pub fn accepts_work(self) -> bool {
        !matches!(self, WorkerAvailability::Stopped)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            WorkerAvailability::Starting => "starting",
            WorkerAvailability::Busy => "busy",
            WorkerAvailability::Waiting => "waiting",
            WorkerAvailability::Idle => "idle",
            WorkerAvailability::Stopped => "stopped",
        }
    }
}

/// Lifecycle of a root orchestration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootState {
    Active,
    Completed,
    Cancelled,
    Interrupted,
}

impl RootState {
    pub fn is_open(self) -> bool {
        matches!(self, RootState::Active)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            RootState::Active => "active",
            RootState::Completed => "completed",
            RootState::Cancelled => "cancelled",
            RootState::Interrupted => "interrupted",
        }
    }
}

/// Success or failure as a worker reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Succeeded,
    Failed,
}

impl Outcome {
    pub fn parse(s: &str) -> Option<Outcome> {
        match s.trim().to_ascii_lowercase().as_str() {
            "succeeded" | "success" | "ok" => Some(Outcome::Succeeded),
            "failed" | "failure" | "error" => Some(Outcome::Failed),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Succeeded => "succeeded",
            Outcome::Failed => "failed",
        }
    }
}

/// Where a result came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultSource {
    Reported,
    ExitCode,
    /// The runtime settled it: cancellation, interruption, or an exit
    /// without a report.
    Runtime,
}

// ── Records ────────────────────────────────────────────────────────────────

/// Exit information of a managed process.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessExit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
}

/// One immutable result of one assignment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskResult {
    pub outcome: Outcome,
    pub source: ResultSource,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub reported_at_ms: i64,
}

/// A vendor configuration change the runtime made so the harness starts
/// unattended. Recorded on the run so the user can see and undo it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustWrite {
    pub path: String,
    pub key: String,
    pub value: String,
}

/// Per-run files the adapter materialized.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunFiles {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings_file: Option<String>,
}

/// Budget and capacity of a root, reported on every root response.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootBudget {
    pub total_starts: u32,
    pub consumed_starts: u32,
    pub remaining_starts: u32,
    pub live_workers: u32,
    pub max_live_workers: u32,
}

/// One run: a root or a worker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    pub id: String,
    #[serde(flatten)]
    pub role: RunRole,
    pub harness: HarnessKind,
    pub project_id: String,
    pub terminal_id: String,
    pub slot_id: String,
    pub name: String,
    pub cwd: String,
    pub executable: String,
    pub delivery: DeliveryMode,
    pub completion: CompletionMode,
    pub created_at_ms: i64,
    pub process_state: ProcessState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<ProcessExit>,
    /// Only meaningful for workers.
    pub worker_state: WorkerAvailability,
    /// Only meaningful for roots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_state: Option<RootState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<RootBudget>,
    /// Only meaningful for roots: what the root reported through `finish`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<TaskResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trust_writes: Vec<TrustWrite>,
    #[serde(default)]
    pub files: RunFiles,
    /// Cleanup that could not be confirmed (process still alive after a
    /// kill). Capacity stays reserved while this is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_error: Option<String>,
}

/// One assignment on one worker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssignmentRecord {
    pub id: String,
    pub worker_id: String,
    pub root_id: String,
    /// Position in the worker's history, starting at 1.
    pub sequence: u64,
    pub name: String,
    pub state: AssignmentState,
    /// The task document as the root wrote it. The bootstrap is not part
    /// of it; adapters prepend that when they materialize the file.
    pub body: String,
    /// Where the adapter materialized the document, empty until launch.
    #[serde(default)]
    pub document_path: String,
    pub created_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<TaskResult>,
    pub request_id: String,
}

/// One message between two runs of the same group.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageRecord {
    pub id: String,
    /// Position in the root-local sequence.
    pub sequence: u64,
    pub sender_id: String,
    pub recipient_id: String,
    pub body: String,
    pub created_at_ms: i64,
    pub acknowledged: bool,
    pub request_id: String,
}

/// What happened, addressed to the root's event stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventKind {
    RootRegistered,
    WorkerAccepted { worker_id: String },
    WorkerStarted { worker_id: String },
    WorkerLaunchFailed { worker_id: String, error: String },
    WorkerExited { worker_id: String, exit: ProcessExit },
    /// The application restarted while the process was live.
    WorkerLost { worker_id: String },
    WorkerStopped { worker_id: String },
    CleanupFailed { worker_id: String, error: String },
    AssignmentQueued { worker_id: String, task_id: String },
    AssignmentStarted { worker_id: String, task_id: String },
    AssignmentFinished { worker_id: String, task_id: String, outcome: Outcome },
    MessageSent { message_id: String, sender_id: String, recipient_id: String },
    MessageAcknowledged { message_id: String },
    RootFinished { state: RootState },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRecord {
    pub sequence: u64,
    pub root_id: String,
    pub at_ms: i64,
    #[serde(flatten)]
    pub kind: EventKind,
}

/// One page of a cursor-paged listing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: u64,
    pub has_more: bool,
}

// ── Requests ───────────────────────────────────────────────────────────────

/// Who is calling. Explicit run ID wins, then the terminal the CLI runs in.
/// A stale or mismatched identity is an error, never a silent fallback.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Caller {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_id: Option<String>,
}

/// Body of `POST /v1/agents`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentEnvelope {
    #[serde(default)]
    pub caller: Caller,
    pub request: AgentRequest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentRequest {
    /// Bind the caller's terminal as a root. Idempotent for a live root.
    Register {
        harness: HarnessKind,
        total_start_budget: u32,
    },
    /// Open a new orchestrator pane in a project and register it.
    Lead {
        request_id: String,
        /// Defaults to the focused project.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project_id: Option<String>,
        harness: HarnessKind,
        total_start_budget: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        /// A specific binary instead of the harness's name on PATH.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        executable: Option<String>,
    },
    /// Root only: start a direct worker.
    Spawn {
        request_id: String,
        name: String,
        harness: HarnessKind,
        task: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        /// A specific binary instead of the harness's name on PATH.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        executable: Option<String>,
        /// Only for `Generic`: the executable and its arguments.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        argv: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        completion: Option<CompletionMode>,
    },
    /// Root only: queue another assignment on an existing worker.
    Assign {
        request_id: String,
        worker_id: String,
        name: String,
        task: String,
    },
    List,
    Get {
        run_id: String,
    },
    Task {
        task_id: String,
    },
    Tasks {
        worker_id: String,
        #[serde(default)]
        after_sequence: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<usize>,
    },
    Message {
        request_id: String,
        recipient_id: String,
        body: String,
    },
    Inbox {
        #[serde(default)]
        after_sequence: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<usize>,
        #[serde(default)]
        unacknowledged_only: bool,
    },
    Ack {
        message_id: String,
    },
    /// Block until newer events exist or the timeout elapses.
    Wait {
        #[serde(default)]
        after_sequence: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
    /// Worker only: claim the next assignment, or receive pending messages.
    Next {
        request_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after_task_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
    /// Report an assignment result, or close the root when `task_id` is
    /// absent and the caller is the root.
    Finish {
        request_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_id: Option<String>,
        outcome: Outcome,
        body: String,
    },
    /// Root: stop one worker or the whole group; worker: stop itself.
    Stop {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_id: Option<String>,
    },
    /// Same termination semantics as `Stop`, kept as the CLI verb for
    /// aborting.
    Cancel {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_id: Option<String>,
    },
}

impl AgentRequest {
    pub fn op(&self) -> &'static str {
        match self {
            AgentRequest::Register { .. } => "register",
            AgentRequest::Lead { .. } => "lead",
            AgentRequest::Spawn { .. } => "spawn",
            AgentRequest::Assign { .. } => "assign",
            AgentRequest::List => "list",
            AgentRequest::Get { .. } => "get",
            AgentRequest::Task { .. } => "task",
            AgentRequest::Tasks { .. } => "tasks",
            AgentRequest::Message { .. } => "message",
            AgentRequest::Inbox { .. } => "inbox",
            AgentRequest::Ack { .. } => "ack",
            AgentRequest::Wait { .. } => "wait",
            AgentRequest::Next { .. } => "next",
            AgentRequest::Finish { .. } => "finish",
            AgentRequest::Stop { .. } => "stop",
            AgentRequest::Cancel { .. } => "cancel",
        }
    }

    /// The request ID of a mutating request, if it carries one.
    pub fn request_id(&self) -> Option<&str> {
        match self {
            AgentRequest::Lead { request_id, .. }
            | AgentRequest::Spawn { request_id, .. }
            | AgentRequest::Assign { request_id, .. }
            | AgentRequest::Message { request_id, .. }
            | AgentRequest::Next { request_id, .. }
            | AgentRequest::Finish { request_id, .. } => Some(request_id),
            _ => None,
        }
    }

    /// Structural validation that needs no state: IDs, sizes, names, and
    /// combinations the runtime would otherwise have to reject later.
    pub fn validate(&self) -> Result<(), AgentError> {
        match self {
            AgentRequest::Register {
                total_start_budget, ..
            } => validate_budget(*total_start_budget),
            AgentRequest::Lead {
                request_id,
                project_id,
                total_start_budget,
                name,
                cwd,
                ..
            } => {
                validate_uuid("request_id", request_id)?;
                if let Some(project_id) = project_id {
                    validate_non_empty("project_id", project_id)?;
                }
                validate_budget(*total_start_budget)?;
                if let Some(name) = name {
                    validate_name(name)?;
                }
                if let Some(cwd) = cwd {
                    validate_non_empty("cwd", cwd)?;
                }
                Ok(())
            }
            AgentRequest::Spawn {
                request_id,
                name,
                harness,
                task,
                cwd,
                executable,
                argv,
                completion,
            } => {
                validate_uuid("request_id", request_id)?;
                validate_name(name)?;
                validate_text("task", task, MAX_TASK_BYTES)?;
                if let Some(cwd) = cwd {
                    validate_non_empty("cwd", cwd)?;
                }
                if let Some(executable) = executable {
                    validate_non_empty("executable", executable)?;
                }
                for arg in argv {
                    if arg.contains('\0') {
                        return Err(AgentError::usage("argv must not contain NUL"));
                    }
                }
                match harness {
                    HarnessKind::Generic => {
                        if argv.is_empty() {
                            return Err(AgentError::usage(
                                "a generic worker needs an executable and arguments after `--`",
                            ));
                        }
                    }
                    _ => {
                        if !argv.is_empty() {
                            return Err(AgentError::usage(
                                "argv is only accepted for the generic harness; the adapter builds the command",
                            ));
                        }
                        if *completion == Some(CompletionMode::ExitCode) {
                            return Err(AgentError::usage(
                                "exit-code completion is only valid for the generic harness",
                            ));
                        }
                    }
                }
                Ok(())
            }
            AgentRequest::Assign {
                request_id,
                worker_id,
                name,
                task,
            } => {
                validate_uuid("request_id", request_id)?;
                validate_uuid("worker_id", worker_id)?;
                validate_name(name)?;
                validate_text("task", task, MAX_TASK_BYTES)
            }
            AgentRequest::List => Ok(()),
            AgentRequest::Get { run_id } => validate_uuid("run_id", run_id),
            AgentRequest::Task { task_id } => validate_uuid("task_id", task_id),
            AgentRequest::Tasks {
                worker_id, limit, ..
            } => {
                validate_uuid("worker_id", worker_id)?;
                validate_limit(*limit)
            }
            AgentRequest::Message {
                request_id,
                recipient_id,
                body,
            } => {
                validate_uuid("request_id", request_id)?;
                validate_uuid("recipient_id", recipient_id)?;
                validate_text("body", body, MAX_MESSAGE_BYTES)
            }
            AgentRequest::Inbox { limit, .. } => validate_limit(*limit),
            AgentRequest::Ack { message_id } => validate_uuid("message_id", message_id),
            AgentRequest::Wait { timeout_ms, .. } => validate_timeout(*timeout_ms),
            AgentRequest::Next {
                request_id,
                after_task_id,
                timeout_ms,
            } => {
                validate_uuid("request_id", request_id)?;
                if let Some(id) = after_task_id {
                    validate_uuid("after_task_id", id)?;
                }
                validate_timeout(*timeout_ms)
            }
            AgentRequest::Finish {
                request_id,
                task_id,
                target_id,
                body,
                ..
            } => {
                validate_uuid("request_id", request_id)?;
                if let Some(id) = task_id {
                    validate_uuid("task_id", id)?;
                }
                if let Some(id) = target_id {
                    validate_uuid("target_id", id)?;
                }
                validate_text("body", body, MAX_RESULT_BYTES)
            }
            AgentRequest::Stop { target_id } | AgentRequest::Cancel { target_id } => {
                if let Some(id) = target_id {
                    validate_uuid("target_id", id)?;
                }
                Ok(())
            }
        }
    }
}

// ── Responses ──────────────────────────────────────────────────────────────

/// Why a `wait` returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitReason {
    Events,
    Timeout,
}

/// What `next` hands to a waiting worker.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NextResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<AssignmentRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<MessageRecord>,
    /// The worker was stopped; do not call again.
    #[serde(default)]
    pub stopped: bool,
    /// Nothing arrived before the timeout; call again with the same
    /// request ID.
    #[serde(default)]
    pub timed_out: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentResponse {
    Run(RunRecord),
    /// A struct variant on purpose: an internally tagged enum cannot carry
    /// a bare sequence.
    Runs { runs: Vec<RunRecord> },
    Assignment(AssignmentRecord),
    Assignments(Page<AssignmentRecord>),
    Message(MessageRecord),
    Messages(Page<MessageRecord>),
    Acknowledged {
        message_id: String,
    },
    Events {
        page: Page<EventRecord>,
        reason: WaitReason,
    },
    Next(NextResponse),
    Finished {
        run_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
    },
    Stopped {
        run_id: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        worker_ids: Vec<String>,
    },
}

// ── Errors ─────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentErrorCode {
    /// Malformed request: bad ID, missing field, over-limit text.
    Usage,
    NotRegistered,
    StaleIdentity,
    /// The target run is not in the caller's group.
    ScopeViolation,
    RootOnly,
    WorkerOnly,
    CapacityExceeded,
    BudgetExhausted,
    QueueFull,
    /// The recipient has too many unacknowledged messages.
    MailboxFull,
    /// Same request ID, different payload.
    RequestConflict,
    /// The run or task is already terminal.
    AlreadyFinished,
    LaunchFailed,
    TrustPreparationFailed,
    HarnessUnavailable,
    StorageFailed,
    Unsupported,
    NotFound,
}

impl AgentErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentErrorCode::Usage => "usage",
            AgentErrorCode::NotRegistered => "not_registered",
            AgentErrorCode::StaleIdentity => "stale_identity",
            AgentErrorCode::ScopeViolation => "scope_violation",
            AgentErrorCode::RootOnly => "root_only",
            AgentErrorCode::WorkerOnly => "worker_only",
            AgentErrorCode::CapacityExceeded => "capacity_exceeded",
            AgentErrorCode::BudgetExhausted => "budget_exhausted",
            AgentErrorCode::QueueFull => "queue_full",
            AgentErrorCode::MailboxFull => "mailbox_full",
            AgentErrorCode::RequestConflict => "request_conflict",
            AgentErrorCode::AlreadyFinished => "already_finished",
            AgentErrorCode::LaunchFailed => "launch_failed",
            AgentErrorCode::TrustPreparationFailed => "trust_preparation_failed",
            AgentErrorCode::HarnessUnavailable => "harness_unavailable",
            AgentErrorCode::StorageFailed => "storage_failed",
            AgentErrorCode::Unsupported => "unsupported",
            AgentErrorCode::NotFound => "not_found",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentError {
    pub code: AgentErrorCode,
    pub message: String,
}

impl AgentError {
    pub fn new(code: AgentErrorCode, message: impl Into<String>) -> Self {
        AgentError {
            code,
            message: message.into(),
        }
    }

    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(AgentErrorCode::Usage, message)
    }
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for AgentError {}

// ── Validation helpers ─────────────────────────────────────────────────────

/// Accepts the canonical lowercase hyphenated UUID form only. IDs are
/// generated by NotMux or by the CLI, never typed, so there is no reason to
/// accept variants.
pub fn is_uuid(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (i, b) in bytes.iter().enumerate() {
        let is_dash = matches!(i, 8 | 13 | 18 | 23);
        if is_dash {
            if *b != b'-' {
                return false;
            }
        } else if !(b.is_ascii_digit() || (b'a'..=b'f').contains(b)) {
            return false;
        }
    }
    true
}

pub fn validate_uuid(field: &str, value: &str) -> Result<(), AgentError> {
    if is_uuid(value) {
        Ok(())
    } else {
        Err(AgentError::usage(format!(
            "{field} must be a lowercase UUID, got {value:?}"
        )))
    }
}

pub fn validate_non_empty(field: &str, value: &str) -> Result<(), AgentError> {
    if value.trim().is_empty() {
        Err(AgentError::usage(format!("{field} must not be empty")))
    } else if value.contains('\0') {
        Err(AgentError::usage(format!("{field} must not contain NUL")))
    } else {
        Ok(())
    }
}

pub fn validate_name(name: &str) -> Result<(), AgentError> {
    validate_non_empty("name", name)?;
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(AgentError::usage(format!(
            "name must be at most {MAX_NAME_CHARS} characters"
        )));
    }
    if name.chars().any(|c| c.is_control()) {
        return Err(AgentError::usage("name must not contain control characters"));
    }
    Ok(())
}

/// Bodies are counted in UTF-8 bytes; a limit is a refusal, never a
/// truncation.
pub fn validate_text(field: &str, value: &str, max_bytes: usize) -> Result<(), AgentError> {
    if value.trim().is_empty() {
        return Err(AgentError::usage(format!("{field} must not be empty")));
    }
    if value.contains('\0') {
        return Err(AgentError::usage(format!("{field} must not contain NUL")));
    }
    if value.len() > max_bytes {
        return Err(AgentError::usage(format!(
            "{field} is {} bytes; the limit is {max_bytes}",
            value.len()
        )));
    }
    Ok(())
}

pub fn validate_budget(total: u32) -> Result<(), AgentError> {
    if total == 0 {
        Err(AgentError::usage("total_start_budget must be positive"))
    } else {
        Ok(())
    }
}

pub fn validate_limit(limit: Option<usize>) -> Result<(), AgentError> {
    match limit {
        Some(0) => Err(AgentError::usage("limit must be positive")),
        Some(n) if n > MAX_PAGE_SIZE => Err(AgentError::usage(format!(
            "limit must be at most {MAX_PAGE_SIZE}"
        ))),
        _ => Ok(()),
    }
}

pub fn validate_timeout(timeout_ms: Option<u64>) -> Result<(), AgentError> {
    match timeout_ms {
        Some(0) => Err(AgentError::usage("timeout_ms must be positive")),
        Some(n) if n > MAX_WAIT_TIMEOUT_MS => Err(AgentError::usage(format!(
            "timeout_ms must be at most {MAX_WAIT_TIMEOUT_MS}"
        ))),
        _ => Ok(()),
    }
}

/// Clamp a page size to the contract.
pub fn page_size(limit: Option<usize>) -> usize {
    limit.unwrap_or(MAX_PAGE_SIZE).clamp(1, MAX_PAGE_SIZE)
}

/// Whether an environment variable name is one the runtime reserves.
pub fn is_reserved_env(name: &str) -> bool {
    RESERVED_ENV.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "7257b427-c815-4936-96c7-932247737171";

    #[test]
    fn uuid_accepts_only_canonical_lowercase() {
        assert!(is_uuid(ID));
        assert!(!is_uuid(&ID.to_uppercase()));
        assert!(!is_uuid("7257b427c81549 6c7932247737171"));
        assert!(!is_uuid(""));
        assert!(!is_uuid("7257b427-c815-4936-96c7-93224773717"));
    }

    #[test]
    fn spawn_round_trips_with_op_tag() {
        let req = AgentRequest::Spawn {
            request_id: ID.into(),
            name: "worker a".into(),
            harness: HarnessKind::Claude,
            task: "do it".into(),
            cwd: None,
            executable: None,
            argv: vec![],
            completion: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["op"], "spawn");
        assert_eq!(json["harness"], "claude");
        assert!(json.get("argv").is_none(), "empty argv is omitted");
        let back: AgentRequest = serde_json::from_value(json).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn envelope_rejects_unknown_fields() {
        let json = serde_json::json!({
            "caller": { "run_id": ID },
            "request": { "op": "list" },
            "extra": 1
        });
        assert!(serde_json::from_value::<AgentEnvelope>(json).is_err());
        let ok = serde_json::json!({ "request": { "op": "list" } });
        let env: AgentEnvelope = serde_json::from_value(ok).unwrap();
        assert_eq!(env.caller, Caller::default());
        assert_eq!(env.request, AgentRequest::List);
    }

    #[test]
    fn run_record_flattens_role() {
        let run = RunRecord {
            id: ID.into(),
            role: RunRole::Worker {
                root_id: ID.into(),
            },
            harness: HarnessKind::Notagent,
            project_id: "p".into(),
            terminal_id: "t".into(),
            slot_id: "s".into(),
            name: "w".into(),
            cwd: "/tmp".into(),
            executable: "/usr/bin/notagent".into(),
            delivery: DeliveryMode::Hook,
            completion: CompletionMode::Reported,
            created_at_ms: 1,
            process_state: ProcessState::Starting,
            exit: None,
            worker_state: WorkerAvailability::Starting,
            root_state: None,
            budget: None,
            result: None,
            current_task_id: None,
            launch_error: None,
            trust_writes: vec![],
            files: RunFiles::default(),
            cleanup_error: None,
        };
        let json = serde_json::to_value(&run).unwrap();
        assert_eq!(json["role"], "worker");
        assert_eq!(json["root_id"], ID);
        let back: RunRecord = serde_json::from_value(json).unwrap();
        assert_eq!(back, run);
    }

    #[test]
    fn event_flattens_kind() {
        let event = EventRecord {
            sequence: 3,
            root_id: ID.into(),
            at_ms: 5,
            kind: EventKind::AssignmentFinished {
                worker_id: ID.into(),
                task_id: ID.into(),
                outcome: Outcome::Failed,
            },
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["kind"], "assignment_finished");
        assert_eq!(json["outcome"], "failed");
        let back: EventRecord = serde_json::from_value(json).unwrap();
        assert_eq!(back, event);
    }

    #[test]
    fn generic_spawn_needs_argv_and_others_refuse_it() {
        let base = AgentRequest::Spawn {
            request_id: ID.into(),
            name: "x".into(),
            harness: HarnessKind::Generic,
            task: "t".into(),
            cwd: None,
            executable: None,
            argv: vec![],
            completion: None,
        };
        assert_eq!(base.validate().unwrap_err().code, AgentErrorCode::Usage);
        let AgentRequest::Spawn { mut argv, .. } = base.clone() else {
            unreachable!()
        };
        argv.push("true".into());
        let ok = AgentRequest::Spawn {
            request_id: ID.into(),
            name: "x".into(),
            harness: HarnessKind::Generic,
            task: "t".into(),
            cwd: None,
            executable: None,
            argv: argv.clone(),
            completion: Some(CompletionMode::ExitCode),
        };
        assert!(ok.validate().is_ok());
        let bad = AgentRequest::Spawn {
            request_id: ID.into(),
            name: "x".into(),
            harness: HarnessKind::Codex,
            task: "t".into(),
            cwd: None,
            executable: None,
            argv,
            completion: None,
        };
        assert!(bad.validate().is_err());
        let bad_completion = AgentRequest::Spawn {
            request_id: ID.into(),
            name: "x".into(),
            harness: HarnessKind::Codex,
            task: "t".into(),
            cwd: None,
            executable: None,
            argv: vec![],
            completion: Some(CompletionMode::ExitCode),
        };
        assert!(bad_completion.validate().is_err());
    }

    #[test]
    fn text_limits_refuse_instead_of_truncating() {
        let big = "x".repeat(MAX_MESSAGE_BYTES + 1);
        let err = validate_text("body", &big, MAX_MESSAGE_BYTES).unwrap_err();
        assert_eq!(err.code, AgentErrorCode::Usage);
        assert!(err.message.contains("65536"));
        assert!(validate_text("body", "   ", MAX_MESSAGE_BYTES).is_err());
        assert!(validate_text("body", "a\0b", MAX_MESSAGE_BYTES).is_err());
        // Multi-byte text counts bytes, not chars.
        let euro = "€".repeat(MAX_MESSAGE_BYTES / 3 + 1);
        assert!(validate_text("body", &euro, MAX_MESSAGE_BYTES).is_err());
    }

    #[test]
    fn budget_limit_and_timeout_bounds() {
        assert!(validate_budget(0).is_err());
        assert!(validate_budget(1).is_ok());
        assert!(validate_limit(Some(0)).is_err());
        assert!(validate_limit(Some(MAX_PAGE_SIZE + 1)).is_err());
        assert_eq!(page_size(None), MAX_PAGE_SIZE);
        assert_eq!(page_size(Some(1000)), MAX_PAGE_SIZE);
        assert!(validate_timeout(Some(0)).is_err());
        assert!(validate_timeout(Some(MAX_WAIT_TIMEOUT_MS + 1)).is_err());
        assert!(validate_timeout(None).is_ok());
    }

    #[test]
    fn request_ids_are_uuids_and_ops_are_named() {
        let bad = AgentRequest::Message {
            request_id: "nope".into(),
            recipient_id: ID.into(),
            body: "hi".into(),
        };
        assert!(bad.validate().is_err());
        assert_eq!(bad.op(), "message");
        assert_eq!(bad.request_id(), Some("nope"));
        assert_eq!(AgentRequest::List.request_id(), None);
    }

    #[test]
    fn harness_and_mode_spellings_round_trip() {
        for kind in HarnessKind::ALL {
            assert_eq!(HarnessKind::parse(kind.as_str()), Some(kind));
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
        }
        assert_eq!(HarnessKind::parse("claude-code"), Some(HarnessKind::Claude));
        assert_eq!(HarnessKind::parse("gemini"), None);
        assert_eq!(CompletionMode::parse("exit-code"), Some(CompletionMode::ExitCode));
        assert_eq!(Outcome::parse("ok"), Some(Outcome::Succeeded));
        assert!(!HarnessKind::Generic.is_cooperating());
    }

    #[test]
    fn reserved_env_names_are_recognized() {
        assert!(is_reserved_env("NOTMUX_RUN_ID"));
        assert!(is_reserved_env("NOTMUX_AGENT_ROLE"));
        assert!(!is_reserved_env("NOTMUX_TERMINAL_ID"));
        assert!(!is_reserved_env("PATH"));
    }

    #[test]
    fn every_response_variant_serializes_with_its_tag() {
        let page = Page {
            items: Vec::<MessageRecord>::new(),
            next_cursor: 0,
            has_more: false,
        };
        let variants = vec![
            AgentResponse::Runs { runs: vec![] },
            AgentResponse::Messages(page),
            AgentResponse::Assignments(Page {
                items: vec![],
                next_cursor: 0,
                has_more: false,
            }),
            AgentResponse::Events {
                page: Page {
                    items: vec![],
                    next_cursor: 0,
                    has_more: false,
                },
                reason: WaitReason::Timeout,
            },
            AgentResponse::Next(NextResponse::default()),
            AgentResponse::Acknowledged {
                message_id: "m".into(),
            },
            AgentResponse::Finished {
                run_id: "r".into(),
                task_id: None,
            },
            AgentResponse::Stopped {
                run_id: "r".into(),
                worker_ids: vec![],
            },
        ];
        for v in variants {
            let json = serde_json::to_value(&v).unwrap_or_else(|e| panic!("{v:?}: {e}"));
            assert!(json.get("kind").is_some(), "{json}");
            let back: AgentResponse = serde_json::from_value(json).unwrap();
            assert_eq!(back, v);
        }
    }

    #[test]
    fn error_has_stable_wire_shape() {
        let err = AgentError::new(AgentErrorCode::BudgetExhausted, "no starts left");
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["code"], "budget_exhausted");
        assert_eq!(err.to_string(), "budget_exhausted: no starts left");
    }
}
