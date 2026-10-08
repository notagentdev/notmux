//! The orchestration state machine.
//!
//! Every method validates first and mutates only on success, so a failed
//! call leaves the model exactly as it was. The store relies on that: it
//! applies a mutation to a copy, persists the copy, and only then replaces
//! the live model.
//!
//! Timestamps are passed in; the model never reads the clock. IDs are
//! generated here through [`super::new_id`].

use notmux_core::orchestration::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const STORE_VERSION: u32 = 3;
/// Remembered mutating requests, oldest evicted first.
pub const MAX_REQUEST_ENTRIES: usize = 4096;

/// A remembered mutating request: same key and digest returns the stored
/// response; same key and another digest is a conflict.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestEntry {
    pub digest: String,
    pub response: AgentResponse,
    pub recorded_at_ms: i64,
}

/// What the runtime must do after a state change: kill these processes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Termination {
    pub run: RunRecord,
    /// Runs whose process is still live and must be killed.
    pub kill_run_ids: Vec<String>,
}

/// What `next` hands to a worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NextClaim {
    Task(AssignmentRecord),
    Messages(Vec<MessageRecord>),
    Stopped,
    /// Nothing queued; the worker is now marked waiting.
    Nothing,
}

/// The IDs a worker launch was planned with; adapters need them before
/// the intent is committed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerIds {
    pub run_id: String,
    pub task_id: String,
    /// The terminal and pane slot the process will get. Recorded with the
    /// intent, before the process exists, so an exit that arrives before
    /// the start is recorded still finds its run.
    pub terminal_id: String,
    pub slot_id: String,
}

#[cfg(test)]
impl WorkerIds {
    pub fn fresh() -> Self {
        WorkerIds {
            run_id: super::new_id(),
            task_id: super::new_id(),
            terminal_id: super::new_id(),
            slot_id: super::new_id(),
        }
    }
}

/// Everything an adapter resolved before the launch intent is committed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchSpec {
    pub harness: HarnessKind,
    pub executable: String,
    pub cwd: String,
    pub delivery: DeliveryMode,
    pub completion: CompletionMode,
    pub files: RunFiles,
    pub trust_writes: Vec<TrustWrite>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Model {
    pub version: u32,
    pub runs: BTreeMap<String, RunRecord>,
    pub assignments: BTreeMap<String, AssignmentRecord>,
    pub messages: BTreeMap<String, MessageRecord>,
    /// Root-local event streams.
    pub events: BTreeMap<String, Vec<EventRecord>>,
    pub requests: BTreeMap<String, RequestEntry>,
    /// Last message sequence handed out per root.
    #[serde(default)]
    pub message_sequences: BTreeMap<String, u64>,
}

impl Default for Model {
    fn default() -> Self {
        Model {
            version: STORE_VERSION,
            runs: BTreeMap::new(),
            assignments: BTreeMap::new(),
            messages: BTreeMap::new(),
            events: BTreeMap::new(),
            requests: BTreeMap::new(),
            message_sequences: BTreeMap::new(),
        }
    }
}

fn err(code: AgentErrorCode, message: impl Into<String>) -> AgentError {
    AgentError::new(code, message)
}

fn runtime_result(outcome: Outcome, body: &str, now: i64) -> TaskResult {
    TaskResult {
        outcome,
        source: ResultSource::Runtime,
        body: body.to_string(),
        request_id: None,
        reported_at_ms: now,
    }
}

impl Model {
    // ── Lookups ────────────────────────────────────────────────────────

    /// The root ID of a run's group.
    pub fn root_id_of(&self, run: &RunRecord) -> String {
        match &run.role {
            RunRole::Root => run.id.clone(),
            RunRole::Worker { root_id } => root_id.clone(),
        }
    }

    pub fn run(&self, run_id: &str) -> Result<&RunRecord, AgentError> {
        self.runs
            .get(run_id)
            .ok_or_else(|| err(AgentErrorCode::NotFound, format!("run {run_id} not found")))
    }

    fn run_mut(&mut self, run_id: &str) -> Result<&mut RunRecord, AgentError> {
        self.runs
            .get_mut(run_id)
            .ok_or_else(|| err(AgentErrorCode::NotFound, format!("run {run_id} not found")))
    }

    /// Resolve who is calling. An explicit run ID wins; a terminal ID must
    /// then match it. A terminal alone resolves to its live run.
    pub fn resolve_caller(&self, caller: &Caller) -> Result<&RunRecord, AgentError> {
        self.resolve_caller_with(caller, false)
    }

    /// Like `resolve_caller`, but with `allow_dead` a run whose process is
    /// gone still resolves by its run ID, so the group's history stays
    /// readable after an exit or a restart. Mutations never use this.
    pub fn resolve_caller_with(&self, caller: &Caller, allow_dead: bool) -> Result<&RunRecord, AgentError> {
        match (&caller.run_id, &caller.terminal_id) {
            (Some(run_id), terminal) => {
                let run = self.runs.get(run_id).ok_or_else(|| {
                    err(
                        AgentErrorCode::StaleIdentity,
                        format!("run {run_id} is unknown to this NotMux instance"),
                    )
                })?;
                // A dead run has no terminal any more: when reads may address
                // it, the pane the CLI happens to run in is irrelevant.
                let terminal_matters = !allow_dead || run.process_state.is_live();
                if terminal_matters
                    && let Some(terminal_id) = terminal
                    && !run.terminal_id.is_empty()
                    && &run.terminal_id != terminal_id
                {
                    return Err(err(
                        AgentErrorCode::StaleIdentity,
                        format!(
                            "run {run_id} belongs to terminal {}, not {terminal_id}",
                            run.terminal_id
                        ),
                    ));
                }
                if !allow_dead && !run.process_state.is_live() {
                    return Err(err(
                        AgentErrorCode::StaleIdentity,
                        format!("run {run_id} is {}", run.process_state.as_str()),
                    ));
                }
                Ok(run)
            }
            (None, Some(terminal_id)) => self
                .runs
                .values()
                .filter(|r| &r.terminal_id == terminal_id && r.process_state.is_live())
                .max_by_key(|r| r.created_at_ms)
                .ok_or_else(|| {
                    err(
                        AgentErrorCode::NotRegistered,
                        "this terminal is not registered; run `notmux agent register` first",
                    )
                }),
            (None, None) => Err(err(
                AgentErrorCode::NotRegistered,
                "no caller identity: neither NOTMUX_RUN_ID nor NOTMUX_TERMINAL_ID is set",
            )),
        }
    }

    fn require_active_root(&self, run_id: &str) -> Result<&RunRecord, AgentError> {
        let run = self.run(run_id)?;
        if !run.role.is_root() {
            return Err(err(
                AgentErrorCode::RootOnly,
                "only the root may do this; workers are leaves and never spawn, assign, or register",
            ));
        }
        if run.root_state != Some(RootState::Active) {
            return Err(err(
                AgentErrorCode::AlreadyFinished,
                format!(
                    "root {run_id} is {}",
                    run.root_state.map(|s| s.as_str()).unwrap_or("not active")
                ),
            ));
        }
        Ok(run)
    }

    fn require_in_group(&self, caller_id: &str, target_id: &str) -> Result<&RunRecord, AgentError> {
        let caller = self.run(caller_id)?;
        let target = self.run(target_id)?;
        if self.root_id_of(caller) != self.root_id_of(target) {
            return Err(err(
                AgentErrorCode::ScopeViolation,
                format!("run {target_id} is not in the caller's group"),
            ));
        }
        Ok(target)
    }

    /// Workers that still hold capacity: live, or not confirmed dead.
    fn holds_capacity(run: &RunRecord) -> bool {
        !run.role.is_root() && (run.process_state.is_live() || run.cleanup_error.is_some())
    }

    pub fn live_workers_of(&self, root_id: &str) -> usize {
        self.runs
            .values()
            .filter(|r| Self::holds_capacity(r))
            .filter(|r| matches!(&r.role, RunRole::Worker { root_id: r } if r == root_id))
            .count()
    }

    pub fn live_workers_total(&self) -> usize {
        self.runs.values().filter(|r| Self::holds_capacity(r)).count()
    }

    fn refresh_budget(&mut self, root_id: &str) {
        let live = self.live_workers_of(root_id) as u32;
        if let Some(root) = self.runs.get_mut(root_id)
            && let Some(budget) = root.budget.as_mut()
        {
            budget.live_workers = live;
            budget.remaining_starts = budget.total_starts.saturating_sub(budget.consumed_starts);
            budget.max_live_workers = MAX_WORKERS_PER_ROOT as u32;
        }
    }

    fn push_event(&mut self, root_id: &str, kind: EventKind, now: i64) {
        let stream = self.events.entry(root_id.to_string()).or_default();
        let sequence = stream.last().map(|e| e.sequence + 1).unwrap_or(1);
        stream.push(EventRecord {
            sequence,
            root_id: root_id.to_string(),
            at_ms: now,
            kind,
        });
    }

    fn worker_assignments(&self, worker_id: &str) -> Vec<&AssignmentRecord> {
        let mut list: Vec<_> = self
            .assignments
            .values()
            .filter(|a| a.worker_id == worker_id)
            .collect();
        list.sort_by_key(|a| a.sequence);
        list
    }

    // ── Roots ──────────────────────────────────────────────────────────

    /// Bind a terminal as a root. Returns the existing root when the
    /// terminal already has a live one (idempotent); a worker terminal is
    /// refused.
    #[allow(clippy::too_many_arguments)]
    pub fn register_root(
        &mut self,
        run_id: &str,
        terminal_id: &str,
        project_id: &str,
        cwd: &str,
        harness: HarnessKind,
        executable: &str,
        name: &str,
        total_start_budget: u32,
        managed: bool,
        now: i64,
    ) -> Result<RunRecord, AgentError> {
        validate_budget(total_start_budget)?;
        if !terminal_id.is_empty()
            && let Some(existing) = self
                .runs
                .values()
                .filter(|r| r.terminal_id == terminal_id && r.process_state.is_live())
                .max_by_key(|r| r.created_at_ms)
        {
            if !existing.role.is_root() {
                return Err(err(
                    AgentErrorCode::RootOnly,
                    "a worker terminal cannot register as a root; workers never spawn workers",
                ));
            }
            if existing.root_state == Some(RootState::Active) {
                return Ok(existing.clone());
            }
        }
        if self.runs.contains_key(run_id) {
            return Err(err(AgentErrorCode::RequestConflict, format!("run {run_id} already exists")));
        }
        let id = run_id.to_string();
        let run = RunRecord {
            id: id.clone(),
            role: RunRole::Root,
            harness,
            project_id: project_id.to_string(),
            terminal_id: terminal_id.to_string(),
            slot_id: String::new(),
            name: if name.trim().is_empty() {
                "Orchestrator".to_string()
            } else {
                name.to_string()
            },
            cwd: cwd.to_string(),
            executable: executable.to_string(),
            delivery: DeliveryMode::Cooperative,
            completion: CompletionMode::Reported,
            created_at_ms: now,
            process_state: if managed {
                ProcessState::Starting
            } else {
                ProcessState::Running
            },
            exit: None,
            worker_state: WorkerAvailability::Idle,
            root_state: Some(RootState::Active),
            budget: Some(RootBudget {
                total_starts: total_start_budget,
                consumed_starts: 0,
                remaining_starts: total_start_budget,
                live_workers: 0,
                max_live_workers: MAX_WORKERS_PER_ROOT as u32,
            }),
            result: None,
            current_task_id: None,
            launch_error: None,
            trust_writes: vec![],
            files: RunFiles::default(),
            cleanup_error: None,
        };
        self.runs.insert(id.clone(), run.clone());
        self.push_event(&id, EventKind::RootRegistered, now);
        Ok(run)
    }

    // ── Workers ────────────────────────────────────────────────────────

    /// Commit a launch intent: the worker exists as `Starting` with its
    /// first assignment queued. Capacity and budget are charged here.
    #[allow(clippy::too_many_arguments)]
    pub fn accept_worker(
        &mut self,
        root_id: &str,
        request_id: &str,
        ids: WorkerIds,
        name: &str,
        task: &str,
        spec: LaunchSpec,
        now: i64,
    ) -> Result<(RunRecord, AssignmentRecord), AgentError> {
        validate_name(name)?;
        validate_text("task", task, MAX_TASK_BYTES)?;
        if self.runs.contains_key(&ids.run_id) || self.assignments.contains_key(&ids.task_id) {
            return Err(err(AgentErrorCode::RequestConflict, "run or task id already exists"));
        }
        let root = self.require_active_root(root_id)?.clone();
        let budget = root.budget.clone().unwrap_or_default();
        if budget.consumed_starts >= budget.total_starts {
            return Err(err(
                AgentErrorCode::BudgetExhausted,
                format!(
                    "all {} worker starts of this root are used",
                    budget.total_starts
                ),
            ));
        }
        if self.live_workers_of(root_id) >= MAX_WORKERS_PER_ROOT {
            return Err(err(
                AgentErrorCode::CapacityExceeded,
                format!("this root already has {MAX_WORKERS_PER_ROOT} live workers; stop one first"),
            ));
        }
        if self.live_workers_total() >= MAX_WORKERS_PER_INSTANCE {
            return Err(err(
                AgentErrorCode::CapacityExceeded,
                format!("NotMux already runs {MAX_WORKERS_PER_INSTANCE} workers"),
            ));
        }
        let worker_id = ids.run_id;
        let run = RunRecord {
            id: worker_id.clone(),
            role: RunRole::Worker {
                root_id: root_id.to_string(),
            },
            harness: spec.harness,
            project_id: root.project_id.clone(),
            terminal_id: ids.terminal_id,
            slot_id: ids.slot_id,
            name: name.to_string(),
            cwd: spec.cwd,
            executable: spec.executable,
            delivery: spec.delivery,
            completion: spec.completion,
            created_at_ms: now,
            process_state: ProcessState::Starting,
            exit: None,
            worker_state: WorkerAvailability::Starting,
            root_state: None,
            budget: None,
            result: None,
            current_task_id: None,
            launch_error: None,
            trust_writes: spec.trust_writes,
            files: spec.files,
            cleanup_error: None,
        };
        let task_id = ids.task_id;
        let assignment = AssignmentRecord {
            id: task_id.clone(),
            worker_id: worker_id.clone(),
            root_id: root_id.to_string(),
            sequence: 1,
            name: name.to_string(),
            state: AssignmentState::Queued,
            body: task.to_string(),
            document_path: String::new(),
            created_at_ms: now,
            started_at_ms: None,
            result: None,
            request_id: request_id.to_string(),
        };
        self.runs.insert(worker_id.clone(), run.clone());
        self.assignments.insert(task_id.clone(), assignment.clone());
        if let Some(b) = self.run_mut(root_id)?.budget.as_mut() {
            b.consumed_starts += 1;
        }
        self.refresh_budget(root_id);
        self.push_event(
            root_id,
            EventKind::WorkerAccepted {
                worker_id: worker_id.clone(),
            },
            now,
        );
        self.push_event(
            root_id,
            EventKind::AssignmentQueued {
                worker_id,
                task_id,
            },
            now,
        );
        Ok((run, assignment))
    }

    /// Record where the first task document was written.
    pub fn set_document_path(&mut self, task_id: &str, path: &str) -> Result<(), AgentError> {
        let a = self
            .assignments
            .get_mut(task_id)
            .ok_or_else(|| err(AgentErrorCode::NotFound, format!("task {task_id} not found")))?;
        a.document_path = path.to_string();
        Ok(())
    }

    /// The process is up: `Starting` becomes `Running`; a worker's first
    /// queued assignment starts.
    pub fn mark_started(
        &mut self,
        run_id: &str,
        terminal_id: &str,
        slot_id: &str,
        now: i64,
    ) -> Result<RunRecord, AgentError> {
        let run = self.run(run_id)?.clone();
        if run.process_state != ProcessState::Starting {
            return Err(err(
                AgentErrorCode::AlreadyFinished,
                format!("run {run_id} is {}", run.process_state.as_str()),
            ));
        }
        // A stop that arrived while the process was being started wins: the
        // run must not become `running` after its stop was confirmed. The
        // caller then kills the process it just started.
        let stopped_worker = !run.role.is_root() && run.worker_state == WorkerAvailability::Stopped;
        let root_closed = self
            .runs
            .get(&self.root_id_of(&run))
            .is_some_and(|root| root.root_state != Some(RootState::Active));
        if stopped_worker || root_closed {
            return Err(err(
                AgentErrorCode::AlreadyFinished,
                format!("run {run_id} was stopped before its process started"),
            ));
        }
        let root_id = self.root_id_of(&run);
        {
            let r = self.run_mut(run_id)?;
            r.process_state = ProcessState::Running;
            r.terminal_id = terminal_id.to_string();
            r.slot_id = slot_id.to_string();
        }
        if run.role.is_root() {
            return Ok(self.run(run_id)?.clone());
        }
        self.push_event(
            &root_id,
            EventKind::WorkerStarted {
                worker_id: run_id.to_string(),
            },
            now,
        );
        let first = self
            .worker_assignments(run_id)
            .into_iter()
            .find(|a| a.state == AssignmentState::Queued)
            .map(|a| a.id.clone());
        match first {
            Some(task_id) => {
                self.start_assignment(run_id, &task_id, now)?;
            }
            None => {
                self.run_mut(run_id)?.worker_state = WorkerAvailability::Idle;
            }
        }
        self.refresh_budget(&root_id);
        Ok(self.run(run_id)?.clone())
    }

    fn start_assignment(&mut self, worker_id: &str, task_id: &str, now: i64) -> Result<(), AgentError> {
        let root_id = {
            let a = self.assignments.get_mut(task_id).ok_or_else(|| {
                err(AgentErrorCode::NotFound, format!("task {task_id} not found"))
            })?;
            a.state = AssignmentState::Running;
            a.started_at_ms = Some(now);
            a.root_id.clone()
        };
        let w = self.run_mut(worker_id)?;
        w.current_task_id = Some(task_id.to_string());
        w.worker_state = WorkerAvailability::Busy;
        self.push_event(
            &root_id,
            EventKind::AssignmentStarted {
                worker_id: worker_id.to_string(),
                task_id: task_id.to_string(),
            },
            now,
        );
        Ok(())
    }

    /// The launch never produced a live process. Budget stays consumed; a
    /// launch attempt is a start.
    pub fn mark_launch_failed(
        &mut self,
        run_id: &str,
        error: &str,
        now: i64,
    ) -> Result<RunRecord, AgentError> {
        let run = self.run(run_id)?.clone();
        if run.process_state != ProcessState::Starting {
            return Err(err(
                AgentErrorCode::AlreadyFinished,
                format!("run {run_id} is {}", run.process_state.as_str()),
            ));
        }
        let root_id = self.root_id_of(&run);
        {
            let r = self.run_mut(run_id)?;
            r.process_state = ProcessState::Lost;
            r.launch_error = Some(error.to_string());
            r.worker_state = WorkerAvailability::Stopped;
            r.current_task_id = None;
            // The launch path killed whatever it started; nothing is left
            // to clean up, so no capacity stays reserved for it.
            r.cleanup_error = None;
            if r.role.is_root() && r.root_state == Some(RootState::Active) {
                r.root_state = Some(RootState::Interrupted);
            }
        }
        self.settle_open_assignments(
            run_id,
            AssignmentState::Interrupted,
            &format!("launch failed: {error}"),
            now,
        );
        if run.role.is_root() {
            // Only a root that was still active finishes here; one closed
            // meanwhile already announced its end.
            if run.root_state == Some(RootState::Active) {
                self.push_event(
                    &root_id,
                    EventKind::RootFinished {
                        state: RootState::Interrupted,
                    },
                    now,
                );
            }
        } else {
            self.push_event(
                &root_id,
                EventKind::WorkerLaunchFailed {
                    worker_id: run_id.to_string(),
                    error: error.to_string(),
                },
                now,
            );
        }
        self.refresh_budget(&root_id);
        Ok(self.run(run_id)?.clone())
    }

    /// Move every non-terminal assignment of a worker to `state` with a
    /// runtime result. Emits `AssignmentFinished` for each.
    fn settle_open_assignments(&mut self, worker_id: &str, state: AssignmentState, body: &str, now: i64) {
        let open: Vec<(String, String)> = self
            .worker_assignments(worker_id)
            .into_iter()
            .filter(|a| !a.state.is_terminal())
            .map(|a| (a.id.clone(), a.root_id.clone()))
            .collect();
        for (task_id, root_id) in open {
            if let Some(a) = self.assignments.get_mut(&task_id) {
                a.state = state;
                a.result = Some(runtime_result(Outcome::Failed, body, now));
            }
            self.push_event(
                &root_id,
                EventKind::AssignmentFinished {
                    worker_id: worker_id.to_string(),
                    task_id,
                    outcome: Outcome::Failed,
                },
                now,
            );
        }
        if let Some(w) = self.runs.get_mut(worker_id) {
            w.current_task_id = None;
        }
    }

    /// The process ended. Idempotent for a run that is already dead.
    pub fn mark_exited(
        &mut self,
        run_id: &str,
        exit: ProcessExit,
        now: i64,
    ) -> Result<Termination, AgentError> {
        let mut run = self.run(run_id)?.clone();
        if !run.process_state.is_live() {
            return Ok(Termination {
                run,
                kill_run_ids: vec![],
            });
        }
        // The process ended before its start was recorded (a very fast
        // program). Its first assignment was running from the process's
        // point of view: start it now, so exit-code completion and the
        // interrupted/failed bookkeeping below see it.
        if run.process_state == ProcessState::Starting && !run.role.is_root() && run.current_task_id.is_none() {
            let first = self
                .worker_assignments(run_id)
                .into_iter()
                .find(|a| a.state == AssignmentState::Queued)
                .map(|a| a.id.clone());
            if let Some(task_id) = first {
                self.start_assignment(run_id, &task_id, now)?;
                run = self.run(run_id)?.clone();
            }
        }
        let root_id = self.root_id_of(&run);
        {
            let r = self.run_mut(run_id)?;
            r.process_state = ProcessState::Exited;
            r.exit = Some(exit.clone());
            r.cleanup_error = None;
            r.worker_state = WorkerAvailability::Stopped;
        }
        let mut kill = vec![];
        if run.role.is_root() {
            if run.root_state == Some(RootState::Active) {
                self.run_mut(run_id)?.root_state = Some(RootState::Interrupted);
                kill = self.stop_workers_of(&root_id, "root exited", now);
                self.push_event(
                    &root_id,
                    EventKind::RootFinished {
                        state: RootState::Interrupted,
                    },
                    now,
                );
            }
        } else {
            // A generic worker's exit code decides its running assignment.
            if run.completion == CompletionMode::ExitCode
                && let Some(task_id) = run.current_task_id.clone()
                && let Some(a) = self.assignments.get_mut(&task_id)
                && a.state == AssignmentState::Running
            {
                let outcome = if exit.code == Some(0) {
                    Outcome::Succeeded
                } else {
                    Outcome::Failed
                };
                a.state = match outcome {
                    Outcome::Succeeded => AssignmentState::Succeeded,
                    Outcome::Failed => AssignmentState::Failed,
                };
                a.result = Some(TaskResult {
                    outcome,
                    source: ResultSource::ExitCode,
                    body: describe_exit(&exit),
                    request_id: None,
                    reported_at_ms: now,
                });
                self.push_event(
                    &root_id,
                    EventKind::AssignmentFinished {
                        worker_id: run_id.to_string(),
                        task_id,
                        outcome,
                    },
                    now,
                );
            }
            self.settle_open_assignments(
                run_id,
                AssignmentState::Interrupted,
                &format!("worker exited before reporting ({})", describe_exit(&exit)),
                now,
            );
            self.push_event(
                &root_id,
                EventKind::WorkerExited {
                    worker_id: run_id.to_string(),
                    exit,
                },
                now,
            );
        }
        self.refresh_budget(&root_id);
        Ok(Termination {
            run: self.run(run_id)?.clone(),
            kill_run_ids: kill,
        })
    }

    /// A kill did not take. Capacity stays reserved until an exit arrives.
    pub fn mark_cleanup_failed(&mut self, run_id: &str, error: &str, now: i64) -> Result<(), AgentError> {
        let run = self.run(run_id)?.clone();
        let root_id = self.root_id_of(&run);
        self.run_mut(run_id)?.cleanup_error = Some(error.to_string());
        self.push_event(
            &root_id,
            EventKind::CleanupFailed {
                worker_id: run_id.to_string(),
                error: error.to_string(),
            },
            now,
        );
        Ok(())
    }

    /// Stop every worker of a root that is not already stopped. Returns the
    /// IDs whose process must be killed.
    fn stop_workers_of(&mut self, root_id: &str, reason: &str, now: i64) -> Vec<String> {
        let ids: Vec<String> = self
            .runs
            .values()
            .filter(|r| matches!(&r.role, RunRole::Worker { root_id: rid } if rid == root_id))
            .map(|r| r.id.clone())
            .collect();
        let mut kill = vec![];
        for id in ids {
            if let Some(k) = self.stop_worker(&id, reason, now) {
                kill.push(k);
            }
        }
        kill
    }

    /// Returns the worker ID when its process is still live.
    fn stop_worker(&mut self, worker_id: &str, reason: &str, now: i64) -> Option<String> {
        let (was_stopped, live, root_id) = {
            let w = self.runs.get(worker_id)?;
            (
                w.worker_state == WorkerAvailability::Stopped,
                w.process_state.is_live() || w.cleanup_error.is_some(),
                self.root_id_of(w),
            )
        };
        if !was_stopped {
            if let Some(w) = self.runs.get_mut(worker_id) {
                w.worker_state = WorkerAvailability::Stopped;
            }
            self.settle_open_assignments(worker_id, AssignmentState::Cancelled, reason, now);
            self.push_event(
                &root_id,
                EventKind::WorkerStopped {
                    worker_id: worker_id.to_string(),
                },
                now,
            );
        }
        live.then(|| worker_id.to_string())
    }

    // ── Assignments ────────────────────────────────────────────────────

    pub fn assign(
        &mut self,
        root_id: &str,
        request_id: &str,
        worker_id: &str,
        name: &str,
        task: &str,
        now: i64,
    ) -> Result<AssignmentRecord, AgentError> {
        validate_name(name)?;
        validate_text("task", task, MAX_TASK_BYTES)?;
        self.require_active_root(root_id)?;
        let worker = self.require_in_group(root_id, worker_id)?.clone();
        if worker.role.is_root() {
            return Err(err(AgentErrorCode::Usage, "a root cannot be assigned tasks"));
        }
        if !worker.worker_state.accepts_work() || !worker.process_state.is_live() {
            return Err(err(
                AgentErrorCode::AlreadyFinished,
                format!("worker {worker_id} is stopped and takes no more work"),
            ));
        }
        let existing = self.worker_assignments(worker_id);
        let queued = existing
            .iter()
            .filter(|a| a.state == AssignmentState::Queued)
            .count();
        if queued >= MAX_QUEUED_ASSIGNMENTS_PER_WORKER {
            return Err(err(
                AgentErrorCode::QueueFull,
                format!("worker {worker_id} already has {MAX_QUEUED_ASSIGNMENTS_PER_WORKER} queued assignments"),
            ));
        }
        let sequence = existing.last().map(|a| a.sequence + 1).unwrap_or(1);
        let task_id = super::new_id();
        let assignment = AssignmentRecord {
            id: task_id.clone(),
            worker_id: worker_id.to_string(),
            root_id: root_id.to_string(),
            sequence,
            name: name.to_string(),
            state: AssignmentState::Queued,
            body: task.to_string(),
            document_path: String::new(),
            created_at_ms: now,
            started_at_ms: None,
            result: None,
            request_id: request_id.to_string(),
        };
        self.assignments.insert(task_id.clone(), assignment.clone());
        self.push_event(
            root_id,
            EventKind::AssignmentQueued {
                worker_id: worker_id.to_string(),
                task_id,
            },
            now,
        );
        Ok(assignment)
    }

    /// A worker asks for work. Its current assignment must be finished.
    /// A worker asks for work. `after_task_id` names the assignment the
    /// worker already holds: without it, an open current assignment is
    /// delivered again (a lost answer must not strand the task as
    /// `running`); with it, only messages are delivered while the worker
    /// waits, and a new assignment is claimed only after the report.
    pub fn claim_next(
        &mut self,
        worker_id: &str,
        after_task_id: Option<&str>,
        now: i64,
    ) -> Result<NextClaim, AgentError> {
        let worker = self.run(worker_id)?.clone();
        if worker.role.is_root() {
            return Err(err(AgentErrorCode::WorkerOnly, "only workers call next; roots use wait"));
        }
        if worker.worker_state == WorkerAvailability::Stopped || !worker.process_state.is_live() {
            return Ok(NextClaim::Stopped);
        }
        let open_current = worker
            .current_task_id
            .as_ref()
            .and_then(|id| self.assignments.get(id))
            .filter(|a| !a.state.is_terminal())
            .cloned();
        if let Some(current) = &open_current
            && after_task_id != Some(current.id.as_str())
        {
            return Ok(NextClaim::Task(current.clone()));
        }
        let busy = open_current.is_some();
        if !busy {
            let next = self
                .worker_assignments(worker_id)
                .into_iter()
                .find(|a| a.state == AssignmentState::Queued)
                .map(|a| a.id.clone());
            if let Some(task_id) = next {
                self.start_assignment(worker_id, &task_id, now)?;
                return Ok(NextClaim::Task(self.assignments[&task_id].clone()));
            }
        }
        let pending: Vec<MessageRecord> = self
            .inbox(worker_id, 0, Some(MAX_PAGE_SIZE), true)
            .items;
        if !pending.is_empty() {
            if !busy {
                self.run_mut(worker_id)?.worker_state = WorkerAvailability::Idle;
            }
            return Ok(NextClaim::Messages(pending));
        }
        if !busy {
            self.run_mut(worker_id)?.worker_state = WorkerAvailability::Waiting;
        }
        Ok(NextClaim::Nothing)
    }

    /// A blocked `next` gave up: the worker is idle, queued work waits.
    pub fn mark_worker_idle(&mut self, worker_id: &str) -> Result<(), AgentError> {
        let w = self.run_mut(worker_id)?;
        if w.worker_state == WorkerAvailability::Waiting {
            w.worker_state = WorkerAvailability::Idle;
        }
        Ok(())
    }

    /// Report a result. The worker reports its own task; the root may
    /// report for any task in its group. Results never change afterwards.
    pub fn finish_task(
        &mut self,
        caller_id: &str,
        task_id: &str,
        outcome: Outcome,
        body: &str,
        request_id: &str,
        now: i64,
    ) -> Result<AssignmentRecord, AgentError> {
        validate_text("body", body, MAX_RESULT_BYTES)?;
        let caller = self.run(caller_id)?.clone();
        let task = self
            .assignments
            .get(task_id)
            .ok_or_else(|| err(AgentErrorCode::NotFound, format!("task {task_id} not found")))?
            .clone();
        let allowed = task.worker_id == caller_id
            || (caller.role.is_root() && task.root_id == caller_id);
        if !allowed {
            return Err(err(
                AgentErrorCode::ScopeViolation,
                format!("task {task_id} belongs to another worker"),
            ));
        }
        if task.state.is_terminal() {
            return Err(err(
                AgentErrorCode::AlreadyFinished,
                format!("task {task_id} is already {}", task.state.as_str()),
            ));
        }
        {
            let a = self.assignments.get_mut(task_id).expect("checked above");
            a.state = match outcome {
                Outcome::Succeeded => AssignmentState::Succeeded,
                Outcome::Failed => AssignmentState::Failed,
            };
            a.result = Some(TaskResult {
                outcome,
                source: ResultSource::Reported,
                body: body.to_string(),
                request_id: Some(request_id.to_string()),
                reported_at_ms: now,
            });
        }
        if let Some(w) = self.runs.get_mut(&task.worker_id)
            && w.current_task_id.as_deref() == Some(task_id)
        {
            w.current_task_id = None;
            if w.worker_state == WorkerAvailability::Busy {
                w.worker_state = WorkerAvailability::Idle;
            }
        }
        self.push_event(
            &task.root_id,
            EventKind::AssignmentFinished {
                worker_id: task.worker_id.clone(),
                task_id: task_id.to_string(),
                outcome,
            },
            now,
        );
        Ok(self.assignments[task_id].clone())
    }

    /// The root is done: record its result, stop every worker.
    pub fn finish_root(
        &mut self,
        root_id: &str,
        outcome: Outcome,
        body: &str,
        request_id: &str,
        now: i64,
    ) -> Result<Termination, AgentError> {
        validate_text("body", body, MAX_RESULT_BYTES)?;
        self.require_active_root(root_id)?;
        {
            let r = self.run_mut(root_id)?;
            r.root_state = Some(RootState::Completed);
            r.result = Some(TaskResult {
                outcome,
                source: ResultSource::Reported,
                body: body.to_string(),
                request_id: Some(request_id.to_string()),
                reported_at_ms: now,
            });
        }
        let kill = self.stop_workers_of(root_id, "root finished", now);
        self.push_event(
            root_id,
            EventKind::RootFinished {
                state: RootState::Completed,
            },
            now,
        );
        self.refresh_budget(root_id);
        Ok(Termination {
            run: self.run(root_id)?.clone(),
            kill_run_ids: kill,
        })
    }

    /// Root: stop one worker, or the whole group without a target. Worker:
    /// stop itself. Idempotent; always returns what still has to be killed.
    pub fn stop(
        &mut self,
        caller_id: &str,
        target_id: Option<&str>,
        now: i64,
    ) -> Result<Termination, AgentError> {
        let caller = self.run(caller_id)?.clone();
        match (&caller.role, target_id) {
            (RunRole::Root, None) => {
                let mut kill = self.stop_workers_of(caller_id, "group cancelled", now);
                if caller.root_state == Some(RootState::Active) {
                    self.run_mut(caller_id)?.root_state = Some(RootState::Cancelled);
                    self.push_event(
                        caller_id,
                        EventKind::RootFinished {
                            state: RootState::Cancelled,
                        },
                        now,
                    );
                }
                kill.sort();
                self.refresh_budget(caller_id);
                Ok(Termination {
                    run: self.run(caller_id)?.clone(),
                    kill_run_ids: kill,
                })
            }
            (RunRole::Root, Some(target)) => {
                let t = self.require_in_group(caller_id, target)?.clone();
                if t.role.is_root() {
                    return self.stop(caller_id, None, now);
                }
                let kill = self.stop_worker(target, "stopped by root", now).into_iter().collect();
                self.refresh_budget(caller_id);
                Ok(Termination {
                    run: self.run(target)?.clone(),
                    kill_run_ids: kill,
                })
            }
            (RunRole::Worker { root_id }, target) => {
                if let Some(t) = target
                    && t != caller_id
                {
                    return Err(err(
                        AgentErrorCode::ScopeViolation,
                        "a worker may only stop itself",
                    ));
                }
                let root_id = root_id.clone();
                let kill = self.stop_worker(caller_id, "stopped itself", now).into_iter().collect();
                self.refresh_budget(&root_id);
                Ok(Termination {
                    run: self.run(caller_id)?.clone(),
                    kill_run_ids: kill,
                })
            }
        }
    }

    // ── Messages ───────────────────────────────────────────────────────

    pub fn send_message(
        &mut self,
        sender_id: &str,
        request_id: &str,
        recipient_id: &str,
        body: &str,
        now: i64,
    ) -> Result<MessageRecord, AgentError> {
        validate_text("body", body, MAX_MESSAGE_BYTES)?;
        if sender_id == recipient_id {
            return Err(err(AgentErrorCode::Usage, "a run cannot message itself"));
        }
        let recipient = self.require_in_group(sender_id, recipient_id)?.clone();
        let dead = match &recipient.role {
            RunRole::Root => recipient.root_state != Some(RootState::Active),
            RunRole::Worker { .. } => recipient.worker_state == WorkerAvailability::Stopped,
        } || !recipient.process_state.is_live();
        if dead {
            return Err(err(
                AgentErrorCode::AlreadyFinished,
                format!("run {recipient_id} no longer receives messages"),
            ));
        }
        let unacked = self
            .messages
            .values()
            .filter(|m| m.recipient_id == recipient_id && !m.acknowledged)
            .count();
        if unacked >= MAX_UNACKNOWLEDGED_MESSAGES {
            return Err(err(
                AgentErrorCode::MailboxFull,
                format!("run {recipient_id} holds {MAX_UNACKNOWLEDGED_MESSAGES} unacknowledged messages"),
            ));
        }
        let root_id = self.root_id_of(&recipient);
        let sequence = {
            let s = self.message_sequences.entry(root_id.clone()).or_insert(0);
            *s += 1;
            *s
        };
        let id = super::new_id();
        let message = MessageRecord {
            id: id.clone(),
            sequence,
            sender_id: sender_id.to_string(),
            recipient_id: recipient_id.to_string(),
            body: body.to_string(),
            created_at_ms: now,
            acknowledged: false,
            request_id: request_id.to_string(),
        };
        self.messages.insert(id.clone(), message.clone());
        self.push_event(
            &root_id,
            EventKind::MessageSent {
                message_id: id,
                sender_id: sender_id.to_string(),
                recipient_id: recipient_id.to_string(),
            },
            now,
        );
        Ok(message)
    }

    pub fn inbox(
        &self,
        run_id: &str,
        after_sequence: u64,
        limit: Option<usize>,
        unacknowledged_only: bool,
    ) -> Page<MessageRecord> {
        let mut all: Vec<&MessageRecord> = self
            .messages
            .values()
            .filter(|m| m.recipient_id == run_id && m.sequence > after_sequence)
            .filter(|m| !unacknowledged_only || !m.acknowledged)
            .collect();
        all.sort_by_key(|m| m.sequence);
        page(all, after_sequence, limit, |m| m.sequence)
    }

    /// Duplicate acks are fine; only the first one emits an event.
    pub fn ack(&mut self, run_id: &str, message_id: &str, now: i64) -> Result<MessageRecord, AgentError> {
        let message = self
            .messages
            .get(message_id)
            .ok_or_else(|| err(AgentErrorCode::NotFound, format!("message {message_id} not found")))?
            .clone();
        if message.recipient_id != run_id {
            return Err(err(
                AgentErrorCode::ScopeViolation,
                "only the recipient acknowledges a message",
            ));
        }
        if message.acknowledged {
            return Ok(message);
        }
        let root_id = self.root_id_of(self.run(run_id)?);
        let m = self.messages.get_mut(message_id).expect("checked above");
        m.acknowledged = true;
        let updated = m.clone();
        self.push_event(
            &root_id,
            EventKind::MessageAcknowledged {
                message_id: message_id.to_string(),
            },
            now,
        );
        Ok(updated)
    }

    // ── Reads ──────────────────────────────────────────────────────────

    pub fn events(&self, root_id: &str, after_sequence: u64, limit: Option<usize>) -> Page<EventRecord> {
        let all: Vec<&EventRecord> = self
            .events
            .get(root_id)
            .map(|s| s.iter().filter(|e| e.sequence > after_sequence).collect())
            .unwrap_or_default();
        page(all, after_sequence, limit, |e| e.sequence)
    }

    /// Every run of the caller's group, root first, then by creation.
    pub fn list_group(&self, caller_id: &str) -> Result<Vec<RunRecord>, AgentError> {
        let root_id = self.root_id_of(self.run(caller_id)?);
        let mut runs: Vec<RunRecord> = self
            .runs
            .values()
            .filter(|r| self.root_id_of(r) == root_id)
            .cloned()
            .collect();
        runs.sort_by_key(|r| (!r.role.is_root(), r.created_at_ms, r.id.clone()));
        Ok(runs)
    }

    pub fn get_in_group(&self, caller_id: &str, run_id: &str) -> Result<RunRecord, AgentError> {
        Ok(self.require_in_group(caller_id, run_id)?.clone())
    }

    pub fn task_in_group(&self, caller_id: &str, task_id: &str) -> Result<AssignmentRecord, AgentError> {
        let task = self
            .assignments
            .get(task_id)
            .ok_or_else(|| err(AgentErrorCode::NotFound, format!("task {task_id} not found")))?;
        let root_id = self.root_id_of(self.run(caller_id)?);
        if task.root_id != root_id {
            return Err(err(
                AgentErrorCode::ScopeViolation,
                format!("task {task_id} is not in the caller's group"),
            ));
        }
        Ok(task.clone())
    }

    pub fn tasks_in_group(
        &self,
        caller_id: &str,
        worker_id: &str,
        after_sequence: u64,
        limit: Option<usize>,
    ) -> Result<Page<AssignmentRecord>, AgentError> {
        self.require_in_group(caller_id, worker_id)?;
        let all: Vec<&AssignmentRecord> = self
            .worker_assignments(worker_id)
            .into_iter()
            .filter(|a| a.sequence > after_sequence)
            .collect();
        Ok(page(all, after_sequence, limit, |a| a.sequence))
    }

    // ── Requests ───────────────────────────────────────────────────────

    /// `Ok(Some)` replays a stored response, `Ok(None)` means run it,
    /// `Err` is a conflicting retry.
    pub fn lookup_request(&self, key: &str, digest: &str) -> Result<Option<AgentResponse>, AgentError> {
        match self.requests.get(key) {
            None => Ok(None),
            Some(entry) if entry.digest == digest => Ok(Some(entry.response.clone())),
            Some(_) => Err(err(
                AgentErrorCode::RequestConflict,
                format!("request {key} was already used with a different payload"),
            )),
        }
    }

    pub fn forget_request(&mut self, key: &str) {
        self.requests.remove(key);
    }

    pub fn record_request(&mut self, key: &str, digest: &str, response: AgentResponse, now: i64) {
        self.requests.insert(
            key.to_string(),
            RequestEntry {
                digest: digest.to_string(),
                response,
                recorded_at_ms: now,
            },
        );
        while self.requests.len() > MAX_REQUEST_ENTRIES {
            let oldest = self
                .requests
                .iter()
                .min_by_key(|(_, e)| e.recorded_at_ms)
                .map(|(k, _)| k.clone());
            match oldest {
                Some(k) => {
                    self.requests.remove(&k);
                }
                None => break,
            }
        }
    }

    // ── Startup ────────────────────────────────────────────────────────

    /// After a restart no managed process is ours any more: every live run
    /// is lost, its open assignments interrupted, its root interrupted.
    /// Returns the affected run IDs.
    pub fn interrupt_live_runs(&mut self, now: i64) -> Vec<String> {
        let live: Vec<String> = self
            .runs
            .values()
            .filter(|r| r.process_state.is_live() || r.cleanup_error.is_some())
            .map(|r| r.id.clone())
            .collect();
        let mut roots: Vec<String> = vec![];
        for id in &live {
            let (is_root, root_id) = {
                let r = &self.runs[id];
                (r.role.is_root(), self.root_id_of(r))
            };
            if let Some(r) = self.runs.get_mut(id) {
                if r.process_state.is_live() {
                    r.process_state = ProcessState::Lost;
                }
                r.cleanup_error = None;
                r.worker_state = WorkerAvailability::Stopped;
            }
            if is_root {
                roots.push(root_id);
            } else {
                self.settle_open_assignments(
                    id,
                    AssignmentState::Interrupted,
                    "NotMux restarted while the worker was running",
                    now,
                );
                self.push_event(
                    &root_id,
                    EventKind::WorkerLost {
                        worker_id: id.clone(),
                    },
                    now,
                );
            }
        }
        // Roots whose process survived (registered panes are not managed)
        // but whose group lost workers stay active; only lost roots end.
        for root_id in roots {
            if self.runs[&root_id].root_state == Some(RootState::Active) {
                self.runs.get_mut(&root_id).unwrap().root_state = Some(RootState::Interrupted);
                self.stop_workers_of(&root_id, "NotMux restarted", now);
                self.push_event(
                    &root_id,
                    EventKind::RootFinished {
                        state: RootState::Interrupted,
                    },
                    now,
                );
            }
        }
        let roots_to_refresh: Vec<String> = self
            .runs
            .values()
            .filter(|r| r.role.is_root())
            .map(|r| r.id.clone())
            .collect();
        for r in roots_to_refresh {
            self.refresh_budget(&r);
        }
        live
    }
}

fn describe_exit(exit: &ProcessExit) -> String {
    match (exit.code, &exit.signal) {
        (Some(code), _) => format!("exit code {code}"),
        (None, Some(sig)) => format!("signal {sig}"),
        (None, None) => "unknown exit".to_string(),
    }
}

fn page<T: Clone>(all: Vec<&T>, after: u64, limit: Option<usize>, seq: impl Fn(&T) -> u64) -> Page<T> {
    let size = page_size(limit);
    let has_more = all.len() > size;
    let items: Vec<T> = all.into_iter().take(size).cloned().collect();
    let next_cursor = items.last().map(&seq).unwrap_or(after);
    Page {
        items,
        next_cursor,
        has_more,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_000;

    fn spec(kind: HarnessKind) -> LaunchSpec {
        LaunchSpec {
            harness: kind,
            executable: "/bin/agent".into(),
            cwd: "/work".into(),
            delivery: DeliveryMode::Cooperative,
            completion: if kind == HarnessKind::Generic {
                CompletionMode::ExitCode
            } else {
                CompletionMode::Reported
            },
            files: RunFiles::default(),
            trust_writes: vec![],
        }
    }

    fn root(m: &mut Model, budget: u32) -> String {
        m.register_root(&super::super::new_id(), "term-root", "proj", "/work", HarnessKind::Notagent, "", "", budget, false, T0)
            .unwrap()
            .id
    }

    fn started_worker(m: &mut Model, root_id: &str, name: &str) -> (String, String) {
        let (w, a) = m
            .accept_worker(root_id, &super::super::new_id(), WorkerIds::fresh(), name, "task", spec(HarnessKind::Claude), T0)
            .unwrap();
        m.mark_started(&w.id, &format!("term-{name}"), "slot", T0).unwrap();
        (w.id, a.id)
    }

    #[test]
    fn register_is_idempotent_for_live_root_and_refused_for_worker() {
        let mut m = Model::default();
        let r1 = root(&mut m, 3);
        let r2 = root(&mut m, 5);
        assert_eq!(r1, r2, "same terminal, same live root");
        let (w, _) = started_worker(&mut m, &r1, "a");
        let term = m.run(&w).unwrap().terminal_id.clone();
        let e = m
            .register_root(&super::super::new_id(), &term, "proj", "/", HarnessKind::Claude, "", "", 1, false, T0)
            .unwrap_err();
        assert_eq!(e.code, AgentErrorCode::RootOnly);
        // registered, accepted, queued, started, assignment started
        assert_eq!(m.events(&r1, 0, None).items.len(), 5);
    }

    #[test]
    fn worker_cannot_spawn_assign_or_register_without_side_effects() {
        let mut m = Model::default();
        let r = root(&mut m, 4);
        let (w, _) = started_worker(&mut m, &r, "a");
        let before = m.clone();
        let e = m
            .accept_worker(&w, &super::super::new_id(), WorkerIds::fresh(), "b", "t", spec(HarnessKind::Codex), T0)
            .unwrap_err();
        assert_eq!(e.code, AgentErrorCode::RootOnly);
        let e = m
            .assign(&w, &super::super::new_id(), &w, "b", "t", T0)
            .unwrap_err();
        assert_eq!(e.code, AgentErrorCode::RootOnly);
        assert_eq!(m, before, "no side effects");
    }

    #[test]
    fn capacity_and_budget_are_enforced_without_eviction() {
        let mut m = Model::default();
        let r = root(&mut m, 10);
        for i in 0..MAX_WORKERS_PER_ROOT {
            started_worker(&mut m, &r, &format!("w{i}"));
        }
        let before = m.clone();
        let e = m
            .accept_worker(&r, &super::super::new_id(), WorkerIds::fresh(), "x", "t", spec(HarnessKind::Claude), T0)
            .unwrap_err();
        assert_eq!(e.code, AgentErrorCode::CapacityExceeded);
        assert_eq!(m, before);
        // Stopping one frees capacity only after the process exit.
        let ids = m.list_group(&r).unwrap();
        let w0 = &ids[1].id;
        let t = m.stop(&r, Some(w0), T0).unwrap();
        assert_eq!(t.kill_run_ids, vec![w0.clone()]);
        assert_eq!(
            m.accept_worker(&r, &super::super::new_id(), WorkerIds::fresh(), "x", "t", spec(HarnessKind::Claude), T0)
                .unwrap_err()
                .code,
            AgentErrorCode::CapacityExceeded
        );
        m.mark_exited(w0, ProcessExit { code: Some(0), signal: None }, T0).unwrap();
        assert!(m
            .accept_worker(&r, &super::super::new_id(), WorkerIds::fresh(), "x", "t", spec(HarnessKind::Claude), T0)
            .is_ok());
        let budget = m.run(&r).unwrap().budget.clone().unwrap();
        assert_eq!(budget.consumed_starts, 5);
        assert_eq!(budget.live_workers, 4);

        let mut m = Model::default();
        let r = root(&mut m, 1);
        started_worker(&mut m, &r, "only");
        let e = m
            .accept_worker(&r, &super::super::new_id(), WorkerIds::fresh(), "x", "t", spec(HarnessKind::Claude), T0)
            .unwrap_err();
        assert_eq!(e.code, AgentErrorCode::BudgetExhausted);
    }

    #[test]
    fn instance_capacity_spans_roots() {
        let mut m = Model::default();
        let r1 = root(&mut m, 10);
        let r2 = m
            .register_root(&super::super::new_id(), "term-2", "p", "/", HarnessKind::Codex, "", "", 10, false, T0)
            .unwrap()
            .id;
        for i in 0..4 {
            started_worker(&mut m, &r1, &format!("a{i}"));
            started_worker(&mut m, &r2, &format!("b{i}"));
        }
        let r3 = m
            .register_root(&super::super::new_id(), "term-3", "p", "/", HarnessKind::Codex, "", "", 10, false, T0)
            .unwrap()
            .id;
        let e = m
            .accept_worker(&r3, &super::super::new_id(), WorkerIds::fresh(), "x", "t", spec(HarnessKind::Claude), T0)
            .unwrap_err();
        assert_eq!(e.code, AgentErrorCode::CapacityExceeded);
        assert_eq!(m.live_workers_total(), MAX_WORKERS_PER_INSTANCE);
    }

    #[test]
    fn assignment_queue_next_and_immutable_results() {
        let mut m = Model::default();
        let r = root(&mut m, 2);
        let (w, first) = started_worker(&mut m, &r, "a");
        assert_eq!(m.run(&w).unwrap().current_task_id.as_deref(), Some(first.as_str()));
        // next without naming the held task delivers it again (a lost answer
        // must not strand it); naming it delivers nothing and keeps busy.
        match m.claim_next(&w, None, T0).unwrap() {
            NextClaim::Task(t) => assert_eq!(t.id, first),
            other => panic!("{other:?}"),
        }
        assert_eq!(m.claim_next(&w, Some(&first), T0).unwrap(), NextClaim::Nothing);
        assert_eq!(m.run(&w).unwrap().worker_state, WorkerAvailability::Busy);
        let second = m.assign(&r, &super::super::new_id(), &w, "second", "more", T0).unwrap();
        assert_eq!(second.sequence, 2);
        let done = m
            .finish_task(&w, &first, Outcome::Succeeded, "ok", "req", T0)
            .unwrap();
        assert_eq!(done.state, AssignmentState::Succeeded);
        assert_eq!(
            m.finish_task(&w, &first, Outcome::Failed, "again", "req2", T0)
                .unwrap_err()
                .code,
            AgentErrorCode::AlreadyFinished
        );
        assert_eq!(m.assignments[&first].result.as_ref().unwrap().body, "ok");
        match m.claim_next(&w, None, T0).unwrap() {
            NextClaim::Task(t) => assert_eq!(t.id, second.id),
            other => panic!("{other:?}"),
        }
        assert_eq!(m.run(&w).unwrap().worker_state, WorkerAvailability::Busy);
        // The answer was lost: the same call delivers the same task again,
        // without touching its state.
        let before = m.clone();
        match m.claim_next(&w, None, T0).unwrap() {
            NextClaim::Task(t) => assert_eq!(t.id, second.id),
            other => panic!("{other:?}"),
        }
        assert_eq!(m, before, "redelivery mutates nothing");
        m.finish_task(&r, &second.id, Outcome::Failed, "root says no", "r", T0)
            .unwrap();
        assert_eq!(m.claim_next(&w, None, T0).unwrap(), NextClaim::Nothing);
        assert_eq!(m.run(&w).unwrap().worker_state, WorkerAvailability::Waiting);
        m.mark_worker_idle(&w).unwrap();
        assert_eq!(m.run(&w).unwrap().worker_state, WorkerAvailability::Idle);
        // Queue bound.
        for i in 0..MAX_QUEUED_ASSIGNMENTS_PER_WORKER {
            m.assign(&r, &super::super::new_id(), &w, &format!("q{i}"), "t", T0).unwrap();
        }
        assert_eq!(
            m.assign(&r, &super::super::new_id(), &w, "over", "t", T0)
                .unwrap_err()
                .code,
            AgentErrorCode::QueueFull
        );
    }

    #[test]
    fn two_workers_finish_independently_and_root_finish_stops_all() {
        let mut m = Model::default();
        let r = root(&mut m, 4);
        let (a, ta) = started_worker(&mut m, &r, "a");
        let (b, tb) = started_worker(&mut m, &r, "b");
        m.finish_task(&a, &ta, Outcome::Succeeded, "A", "1", T0).unwrap();
        // b may not finish a's task, and a may not finish b's.
        assert_eq!(
            m.finish_task(&a, &tb, Outcome::Succeeded, "x", "2", T0).unwrap_err().code,
            AgentErrorCode::ScopeViolation
        );
        m.finish_task(&b, &tb, Outcome::Failed, "B", "3", T0).unwrap();
        let t = m.finish_root(&r, Outcome::Succeeded, "all done", "4", T0).unwrap();
        assert_eq!(t.kill_run_ids.len(), 2);
        assert_eq!(m.run(&r).unwrap().root_state, Some(RootState::Completed));
        assert!(m.runs.values().filter(|x| !x.role.is_root()).all(|x| x.worker_state == WorkerAvailability::Stopped));
        assert_eq!(
            m.accept_worker(&r, &super::super::new_id(), WorkerIds::fresh(), "x", "t", spec(HarnessKind::Claude), T0)
                .unwrap_err()
                .code,
            AgentErrorCode::AlreadyFinished
        );
    }

    #[test]
    fn messages_are_group_scoped_and_acks_are_idempotent() {
        let mut m = Model::default();
        let r = root(&mut m, 4);
        let (a, _) = started_worker(&mut m, &r, "a");
        let r2 = m
            .register_root(&super::super::new_id(), "term-2", "p", "/", HarnessKind::Codex, "", "", 2, false, T0)
            .unwrap()
            .id;
        let (b, _) = {
            let (w, t) = m
                .accept_worker(&r2, &super::super::new_id(), WorkerIds::fresh(), "b", "t", spec(HarnessKind::Codex), T0)
                .unwrap();
            m.mark_started(&w.id, "term-b", "s", T0).unwrap();
            (w.id, t.id)
        };
        assert_eq!(
            m.send_message(&a, "req", &b, "hi", T0).unwrap_err().code,
            AgentErrorCode::ScopeViolation
        );
        let m1 = m.send_message(&a, "req", &r, "hello root", T0).unwrap();
        let m2 = m.send_message(&r, "req", &a, "hello worker", T0).unwrap();
        assert_eq!((m1.sequence, m2.sequence), (1, 2));
        assert_eq!(m.inbox(&r, 0, None, true).items.len(), 1);
        assert_eq!(
            m.ack(&r, &m2.id, T0).unwrap_err().code,
            AgentErrorCode::ScopeViolation
        );
        let events_before = m.events(&r, 0, None).items.len();
        m.ack(&r, &m1.id, T0).unwrap();
        m.ack(&r, &m1.id, T0).unwrap();
        assert_eq!(m.events(&r, 0, None).items.len(), events_before + 1);
        assert!(m.inbox(&r, 0, None, true).items.is_empty());
        // Worker next surfaces unacked messages when nothing is queued.
        let (_, ta) = (a.clone(), m.run(&a).unwrap().current_task_id.clone().unwrap());
        m.finish_task(&a, &ta, Outcome::Succeeded, "ok", "x", T0).unwrap();
        match m.claim_next(&a, None, T0).unwrap() {
            NextClaim::Messages(list) => assert_eq!(list[0].id, m2.id),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn mailbox_bound_refuses_without_eviction() {
        let mut m = Model::default();
        let r = root(&mut m, 4);
        let (a, _) = started_worker(&mut m, &r, "a");
        for _ in 0..MAX_UNACKNOWLEDGED_MESSAGES {
            m.send_message(&a, "req", &r, "x", T0).unwrap();
        }
        let before = m.messages.len();
        assert_eq!(
            m.send_message(&a, "req", &r, "one more", T0).unwrap_err().code,
            AgentErrorCode::MailboxFull
        );
        assert_eq!(m.messages.len(), before);
    }

    #[test]
    fn cursor_paging_has_no_gaps() {
        let mut m = Model::default();
        let r = root(&mut m, 4);
        let (a, _) = started_worker(&mut m, &r, "a");
        for i in 0..150 {
            m.send_message(&a, "req", &r, &format!("m{i}"), T0).unwrap();
        }
        let mut cursor = 0;
        let mut seen = vec![];
        loop {
            let p = m.inbox(&r, cursor, Some(MAX_PAGE_SIZE), false);
            seen.extend(p.items.iter().map(|x| x.sequence));
            cursor = p.next_cursor;
            if !p.has_more {
                break;
            }
        }
        assert_eq!(seen, (1..=150).collect::<Vec<u64>>());
        let mut cursor = 0;
        let mut count = 0;
        loop {
            let p = m.events(&r, cursor, Some(7));
            count += p.items.len();
            cursor = p.next_cursor;
            if !p.has_more {
                break;
            }
        }
        assert_eq!(count, m.events[&r].len());
    }

    #[test]
    fn exit_code_completion_and_interrupted_reported_task() {
        let mut m = Model::default();
        let r = root(&mut m, 4);
        let (g, tg) = {
            let (w, t) = m
                .accept_worker(&r, &super::super::new_id(), WorkerIds::fresh(), "gen", "t", spec(HarnessKind::Generic), T0)
                .unwrap();
            m.mark_started(&w.id, "term-g", "s", T0).unwrap();
            (w.id, t.id)
        };
        m.mark_exited(&g, ProcessExit { code: Some(3), signal: None }, T0).unwrap();
        let a = &m.assignments[&tg];
        assert_eq!(a.state, AssignmentState::Failed);
        assert_eq!(a.result.as_ref().unwrap().source, ResultSource::ExitCode);
        let (c, tc) = started_worker(&mut m, &r, "claude");
        m.mark_exited(&c, ProcessExit { code: Some(0), signal: None }, T0).unwrap();
        assert_eq!(m.assignments[&tc].state, AssignmentState::Interrupted);
        assert_eq!(m.run(&c).unwrap().worker_state, WorkerAvailability::Stopped);
        // Exit is idempotent.
        let t = m.mark_exited(&c, ProcessExit::default(), T0).unwrap();
        assert_eq!(t.run.exit.unwrap().code, Some(0));
    }

    #[test]
    fn launch_failure_keeps_budget_and_frees_capacity() {
        let mut m = Model::default();
        let r = root(&mut m, 2);
        let (w, t) = m
            .accept_worker(&r, &super::super::new_id(), WorkerIds::fresh(), "a", "t", spec(HarnessKind::Codex), T0)
            .unwrap();
        m.mark_launch_failed(&w.id, "codex not found", T0).unwrap();
        assert_eq!(m.assignments[&t.id].state, AssignmentState::Interrupted);
        let b = m.run(&r).unwrap().budget.clone().unwrap();
        assert_eq!((b.consumed_starts, b.live_workers), (1, 0));
        assert_eq!(
            m.mark_started(&w.id, "x", "y", T0).unwrap_err().code,
            AgentErrorCode::AlreadyFinished
        );
    }

    #[test]
    fn root_exit_interrupts_group_and_restart_interrupts_everything() {
        let mut m = Model::default();
        let r = m
            .register_root(&super::super::new_id(), "t", "p", "/", HarnessKind::Notagent, "", "lead", 4, true, T0)
            .unwrap()
            .id;
        m.mark_started(&r, "t", "slot", T0).unwrap();
        let (a, ta) = started_worker(&mut m, &r, "a");
        let term = m.mark_exited(&r, ProcessExit { code: Some(1), signal: None }, T0).unwrap();
        assert_eq!(term.kill_run_ids, vec![a.clone()]);
        assert_eq!(m.run(&r).unwrap().root_state, Some(RootState::Interrupted));
        assert_eq!(m.assignments[&ta].state, AssignmentState::Cancelled);

        let mut m = Model::default();
        let r = root(&mut m, 4);
        let (a, ta) = started_worker(&mut m, &r, "a");
        let affected = m.interrupt_live_runs(T0 + 1);
        assert_eq!(affected.len(), 2);
        assert_eq!(m.run(&a).unwrap().process_state, ProcessState::Lost);
        assert_eq!(m.assignments[&ta].state, AssignmentState::Interrupted);
        assert_eq!(m.run(&r).unwrap().root_state, Some(RootState::Interrupted));
        assert_eq!(m.live_workers_total(), 0);
        assert!(m.interrupt_live_runs(T0 + 2).is_empty());
    }

    #[test]
    fn request_replay_and_conflict() {
        let mut m = Model::default();
        let resp = AgentResponse::Acknowledged {
            message_id: "x".into(),
        };
        assert_eq!(m.lookup_request("k", "d1").unwrap(), None);
        m.record_request("k", "d1", resp.clone(), T0);
        assert_eq!(m.lookup_request("k", "d1").unwrap(), Some(resp));
        assert_eq!(
            m.lookup_request("k", "d2").unwrap_err().code,
            AgentErrorCode::RequestConflict
        );
        for i in 0..MAX_REQUEST_ENTRIES + 5 {
            m.record_request(&format!("r{i}"), "d", AgentResponse::Runs { runs: vec![] }, T0 + i as i64);
        }
        assert_eq!(m.requests.len(), MAX_REQUEST_ENTRIES);
        assert!(!m.requests.contains_key("k"), "oldest evicted first");
    }

    #[test]
    fn caller_resolution_rejects_stale_identity() {
        let mut m = Model::default();
        let r = root(&mut m, 4);
        let (a, _) = started_worker(&mut m, &r, "a");
        let by_term = m
            .resolve_caller(&Caller {
                run_id: None,
                terminal_id: Some("term-root".into()),
            })
            .unwrap();
        assert_eq!(by_term.id, r);
        let e = m
            .resolve_caller(&Caller {
                run_id: Some(a.clone()),
                terminal_id: Some("term-root".into()),
            })
            .unwrap_err();
        assert_eq!(e.code, AgentErrorCode::StaleIdentity);
        m.mark_exited(&a, ProcessExit::default(), T0).unwrap();
        let dead = Caller {
            run_id: Some(a.clone()),
            terminal_id: None,
        };
        let e = m.resolve_caller(&dead).unwrap_err();
        assert_eq!(e.code, AgentErrorCode::StaleIdentity);
        // Reads may still address the dead run, so history stays reachable.
        assert_eq!(m.resolve_caller_with(&dead, true).unwrap().id, a);
        assert!(m.list_group(&a).unwrap().len() == 2);
        // Reading a dead run from another pane (`--run <old id>` in a new
        // terminal) is fine: the dead run has no pane any more. An unknown
        // id stays unknown even for reads.
        assert_eq!(
            m.resolve_caller_with(
                &Caller {
                    run_id: Some(a.clone()),
                    terminal_id: Some("term-root".into()),
                },
                true,
            )
            .unwrap()
            .id,
            a
        );
        assert_eq!(
            m.resolve_caller_with(
                &Caller {
                    run_id: Some("nope".into()),
                    terminal_id: None,
                },
                true,
            )
            .unwrap_err()
            .code,
            AgentErrorCode::StaleIdentity
        );
        assert_eq!(
            m.resolve_caller(&Caller::default()).unwrap_err().code,
            AgentErrorCode::NotRegistered
        );
    }

    #[test]
    fn worker_stop_scope() {
        let mut m = Model::default();
        let r = root(&mut m, 4);
        let (a, _) = started_worker(&mut m, &r, "a");
        let (b, _) = started_worker(&mut m, &r, "b");
        assert_eq!(
            m.stop(&a, Some(&b), T0).unwrap_err().code,
            AgentErrorCode::ScopeViolation
        );
        let t = m.stop(&a, None, T0).unwrap();
        assert_eq!(t.kill_run_ids, vec![a.clone()]);
        assert_eq!(m.stop(&a, None, T0).unwrap().kill_run_ids, vec![a.clone()], "still live: kill again");
        m.mark_exited(&a, ProcessExit::default(), T0).unwrap();
        assert!(m.stop(&a, None, T0).unwrap().kill_run_ids.is_empty());
        let t = m.stop(&r, None, T0).unwrap();
        assert_eq!(t.kill_run_ids, vec![b]);
        assert_eq!(m.run(&r).unwrap().root_state, Some(RootState::Cancelled));
    }
}
