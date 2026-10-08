//! The coordinator: turns agent requests into store mutations, launches,
//! pane insertions, and kills. It never blocks on a client: `wait` and
//! `next` return at once, and the HTTP layer re-asks whenever
//! [`Runtime::subscribe`] reports a change.
//!
//! Everything that touches GPUI state (panes, PTYs, settings) goes through
//! the [`Host`] trait, implemented by the desktop and headless apps and by
//! a fake in tests.
//!
//! A worker launch has three phases so the slow part never runs on the UI
//! thread: [`Runtime::plan`] (host, fast), [`Runtime::prepare`] (no host,
//! may block on trust files and a `--help` probe), [`Runtime::commit`]
//! (host, fast). [`Runtime::handle`] runs all three for callers that do not
//! care.

use super::bootstrap::{self, RunFacts};
use super::harness::{self, AgentRole, LaunchRequest, PreparedLaunch, VendorPaths};
#[cfg(test)]
use super::model::Model;
use super::model::{LaunchSpec, NextClaim, WorkerIds};

/// Test-only: how long `prepare` sleeps before the adapter runs, so a
/// deadline can be exercised without a slow file system.
#[cfg(test)]
static PREPARE_DELAY_MS: AtomicU64 = AtomicU64::new(0);
use super::store::{Opened, Store};
use super::{new_id, now_ms, request_digest, request_key};
use notmux_core::orchestration::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::watch;

/// What the runtime needs from the application.
pub trait Host {
    /// The project owning a terminal: `(project_id, project_path)`.
    fn project_for_terminal(&self, terminal_id: &str) -> Option<(String, String)>;
    fn project_path(&self, project_id: &str) -> Option<String>;
    fn focused_project(&self) -> Option<String>;
    /// The user's per-terminal environment from settings.
    fn terminal_env(&self) -> HashMap<String, String>;
    /// Register the terminal model and start the process.
    fn launch(
        &mut self,
        terminal_id: &str,
        cwd: &str,
        launch: &PreparedLaunch,
        env: &HashMap<String, String>,
    ) -> Result<(), String>;
    /// Insert the pane for an already running terminal.
    #[allow(clippy::too_many_arguments)]
    fn insert_pane(
        &mut self,
        project_id: &str,
        slot_id: &str,
        terminal_id: &str,
        run_id: &str,
        name: &str,
        focus: bool,
    ) -> Result<(), String>;
    fn remove_pane(&mut self, project_id: &str, slot_id: &str);
    fn kill_terminal(&mut self, terminal_id: &str);
    /// The run's state changed: show it on its pane (header, sidebar, API).
    /// `task_state` is the state of the worker's newest assignment, so the
    /// outcome of the last task is visible beside availability and process.
    fn publish_run(&mut self, run: &RunRecord, task_state: Option<AssignmentState>);
}

/// A planned launch, between `plan` and `commit`.
#[derive(Clone, Debug)]
pub struct LaunchPlan {
    pub kind: PlanKind,
    pub harness: HarnessKind,
    pub project_id: String,
    pub cwd: String,
    pub run_id: String,
    pub run_dir: PathBuf,
    pub name: String,
    pub executable: Option<String>,
    pub request_key: Option<String>,
    pub digest: String,
    pub planned_at_ms: i64,
}

#[derive(Clone, Debug)]
pub enum PlanKind {
    Root {
        total_start_budget: u32,
    },
    Worker {
        root_id: String,
        request_id: String,
        task_id: String,
        task: String,
        argv: Vec<String>,
        completion: Option<CompletionMode>,
    },
}

/// Outcome of `plan`: either a replayed response or a launch to prepare.
#[derive(Clone, Debug)]
pub enum Plan {
    Replay(AgentResponse),
    Launch(LaunchPlan),
}

#[derive(Clone, Debug)]
pub struct Prepared {
    pub plan: LaunchPlan,
    pub launch: PreparedLaunch,
}

/// A recorded launch intent, between `commit_intent` and `finish_start`.
#[derive(Clone, Debug)]
pub struct Intent {
    pub plan: LaunchPlan,
    pub launch: PreparedLaunch,
    pub terminal_id: String,
    pub slot_id: String,
    pub focus: bool,
}

#[derive(Clone, Debug)]
pub enum IntentOutcome {
    /// The request id was already used: its stored answer.
    Replay(AgentResponse),
    Proceed(Box<Intent>),
}

/// Result of `finish_start`, plus the pane the UI must remove when the
/// start could not be recorded.
#[derive(Debug)]
pub struct FinishOutcome {
    pub response: Result<AgentResponse, AgentError>,
    pub remove_pane: Option<(String, String)>,
}

type ProcessKiller = Arc<dyn Fn(&str) + Send + Sync>;

struct Inner {
    store: Store,
    dir: PathBuf,
    vendor: VendorPaths,
    search_path: String,
    changes: watch::Sender<u64>,
    generation: AtomicU64,
    /// Kills a terminal's process at shutdown, installed by the app.
    killer: std::sync::Mutex<Option<ProcessKiller>>,
}

#[derive(Clone)]
pub struct Runtime {
    inner: Arc<Inner>,
}

fn err(code: AgentErrorCode, message: impl Into<String>) -> AgentError {
    AgentError::new(code, message)
}

impl Runtime {
    /// Open the store under `dir`, interrupt what was live, and start.
    pub fn open(dir: &Path, vendor: VendorPaths, search_path: String) -> Result<(Runtime, Opened), AgentError> {
        let (store, opened) = Store::open(dir, now_ms())?;
        let (changes, _) = watch::channel(0u64);
        let runtime = Runtime {
            inner: Arc::new(Inner {
                store,
                dir: dir.to_path_buf(),
                vendor,
                search_path,
                changes,
                generation: AtomicU64::new(0),
                killer: std::sync::Mutex::new(None),
            }),
        };
        Ok((runtime, opened))
    }

    /// Install the function that kills a managed terminal's process; used
    /// by [`Runtime::shutdown`] where no host is available.
    pub fn set_process_killer(&self, killer: ProcessKiller) {
        *self.inner.killer.lock().unwrap_or_else(|p| p.into_inner()) = Some(killer);
    }

    /// Kill every launched process before the application exits. The
    /// records are settled on the next open.
    pub fn shutdown(&self) {
        let killer = self.inner.killer.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let Some(killer) = killer else {
            return;
        };
        for terminal_id in self.live_terminal_ids() {
            killer(&terminal_id);
        }
    }

    pub fn store(&self) -> &Store {
        &self.inner.store
    }

    /// Changes since open; waiters compare against the value they saw.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.inner.changes.subscribe()
    }

    fn bump(&self) {
        let next = self.inner.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.inner.changes.send(next);
    }

    /// Publish every run of the group the caller belongs to, each with the
    /// state of its newest assignment.
    fn publish_group(&self, member_id: &str, host: &mut dyn Host) {
        let runs: Vec<(RunRecord, Option<AssignmentState>)> = self.inner.store.read(|m| {
            m.list_group(member_id)
                .unwrap_or_default()
                .into_iter()
                .map(|run| {
                    // The assignment being worked on, if any; otherwise the
                    // newest one, whose outcome is what the worker last did.
                    let current = run
                        .current_task_id
                        .as_ref()
                        .and_then(|id| m.assignments.get(id))
                        .filter(|a| !a.state.is_terminal())
                        .map(|a| a.state);
                    let shown = current.or_else(|| {
                        m.assignments
                            .values()
                            .filter(|a| a.worker_id == run.id)
                            .max_by_key(|a| a.sequence)
                            .map(|a| a.state)
                    });
                    (run, shown)
                })
                .collect()
        });
        for (run, task_state) in &runs {
            if !run.terminal_id.is_empty() {
                host.publish_run(run, *task_state);
            }
        }
    }

    fn run_dir(&self, run_id: &str) -> PathBuf {
        self.inner.dir.join("runs").join(run_id)
    }

    fn resolve_caller(&self, caller: &Caller) -> Result<RunRecord, AgentError> {
        self.inner.store.read(|m| m.resolve_caller(caller).cloned())
    }

    // ── Requests ───────────────────────────────────────────────────────

    /// Answer one request completely. `Spawn` and `Lead` run all three
    /// launch phases here.
    pub fn handle(
        &self,
        caller: &Caller,
        request: AgentRequest,
        host: &mut dyn Host,
    ) -> Result<AgentResponse, AgentError> {
        request.validate()?;
        match request {
            AgentRequest::Spawn { .. } | AgentRequest::Lead { .. } => match self.plan(caller, &request, host)? {
                Plan::Replay(response) => Ok(response),
                Plan::Launch(plan) => {
                    let prepared = self.prepare(plan)?;
                    self.commit(prepared, host)
                }
            },
            AgentRequest::Register {
                harness,
                total_start_budget,
            } => self.register(caller, harness, total_start_budget, host),
            other => self.handle_registered(caller, other, host),
        }
    }

    fn register(
        &self,
        caller: &Caller,
        harness: HarnessKind,
        total_start_budget: u32,
        host: &mut dyn Host,
    ) -> Result<AgentResponse, AgentError> {
        let terminal_id = caller.terminal_id.clone().ok_or_else(|| {
            err(
                AgentErrorCode::NotRegistered,
                "register needs the pane's terminal; run it inside a NotMux terminal",
            )
        })?;
        let (project_id, cwd) = host.project_for_terminal(&terminal_id).ok_or_else(|| {
            err(
                AgentErrorCode::NotRegistered,
                format!("terminal {terminal_id} belongs to no project"),
            )
        })?;
        let run_id = new_id();
        let run = self.inner.store.mutate(move |m| {
            m.register_root(
                &run_id,
                &terminal_id,
                &project_id,
                &cwd,
                harness,
                harness.as_str(),
                "Orchestrator",
                total_start_budget,
                false,
                now_ms(),
            )
        })?;
        self.bump();
        host.publish_run(&run, None);
        Ok(AgentResponse::Run(run))
    }

    fn handle_registered(
        &self,
        caller: &Caller,
        request: AgentRequest,
        host: &mut dyn Host,
    ) -> Result<AgentResponse, AgentError> {
        // Reads may come from a run whose process is gone (history after an
        // exit or a restart); mutations need a live identity.
        let is_read = matches!(
            request,
            AgentRequest::List
                | AgentRequest::Get { .. }
                | AgentRequest::Task { .. }
                | AgentRequest::Tasks { .. }
                | AgentRequest::Inbox { .. }
                | AgentRequest::Wait { .. }
        );
        let me = self
            .inner
            .store
            .read(|m| m.resolve_caller_with(caller, is_read).cloned())?;
        let key = request_key(&me.id, &request);
        let digest = request_digest(&request);
        if let Some(key) = &key
            && let Some(replay) = self.inner.store.read(|m| m.lookup_request(key, &digest))?
        {
            return Ok(replay);
        }
        let store = self.inner.store.clone();
        let me_id = me.id.clone();

        // Reads never write.
        match &request {
            AgentRequest::List => {
                return Ok(AgentResponse::Runs {
                    runs: store.read(|m| m.list_group(&me_id))?,
                });
            }
            AgentRequest::Get { run_id } => {
                return Ok(AgentResponse::Run(store.read(|m| m.get_in_group(&me_id, run_id))?));
            }
            AgentRequest::Task { task_id } => {
                return Ok(AgentResponse::Assignment(store.read(|m| m.task_in_group(&me_id, task_id))?));
            }
            AgentRequest::Tasks {
                worker_id,
                after_sequence,
                limit,
            } => {
                return Ok(AgentResponse::Assignments(store.read(|m| {
                    m.tasks_in_group(&me_id, worker_id, *after_sequence, *limit)
                })?));
            }
            AgentRequest::Inbox {
                after_sequence,
                limit,
                unacknowledged_only,
            } => {
                return Ok(AgentResponse::Messages(store.read(|m| {
                    m.inbox(&me_id, *after_sequence, *limit, *unacknowledged_only)
                })));
            }
            AgentRequest::Wait { after_sequence, .. } => {
                let root_id = store.read(|m| m.run(&me_id).map(|r| m.root_id_of(r)))?;
                let page = store.read(|m| m.events(&root_id, *after_sequence, None));
                let reason = if page.items.is_empty() {
                    WaitReason::Timeout
                } else {
                    WaitReason::Events
                };
                return Ok(AgentResponse::Events { page, reason });
            }
            _ => {}
        }

        // Every mutation and the record that makes its retry idempotent are
        // one store write: either both are on disk or neither is.
        type Op = Box<dyn FnOnce(&mut super::model::Model) -> Result<(AgentResponse, Vec<String>), AgentError> + Send>;
        let op: Op = match request {
            AgentRequest::Assign {
                request_id,
                worker_id,
                name,
                task,
            } => Box::new(move |m| {
                let a = m.assign(&me_id, &request_id, &worker_id, &name, &task, now_ms())?;
                let t: Vec<String> = vec![];
                Ok((AgentResponse::Assignment(a), t))
            }),
            AgentRequest::Message {
                request_id,
                recipient_id,
                body,
            } => Box::new(move |m| {
                let msg = m.send_message(&me_id, &request_id, &recipient_id, &body, now_ms())?;
                let t: Vec<String> = vec![];
                Ok((AgentResponse::Message(msg), t))
            }),
            AgentRequest::Ack { message_id } => Box::new(move |m| {
                let msg = m.ack(&me_id, &message_id, now_ms())?;
                let t: Vec<String> = vec![];
                Ok((AgentResponse::Acknowledged { message_id: msg.id }, t))
            }),
            AgentRequest::Next { after_task_id, .. } => Box::new(move |m| {
                let claim = m.claim_next(&me_id, after_task_id.as_deref(), now_ms())?;
                let response = match claim {
                    NextClaim::Task(task) => NextResponse {
                        task: Some(task),
                        ..Default::default()
                    },
                    NextClaim::Messages(messages) => NextResponse {
                        messages,
                        ..Default::default()
                    },
                    NextClaim::Stopped => NextResponse {
                        stopped: true,
                        ..Default::default()
                    },
                    NextClaim::Nothing => NextResponse {
                        timed_out: true,
                        ..Default::default()
                    },
                };
                let t: Vec<String> = vec![];
                Ok((AgentResponse::Next(response), t))
            }),
            AgentRequest::Finish {
                request_id,
                task_id,
                target_id,
                outcome,
                body,
            } => {
                let is_root = me.role.is_root();
                let current = me.current_task_id.clone();
                let my_id = me.id.clone();
                Box::new(move |m| match (task_id, is_root) {
                    (Some(task_id), _) => {
                        let a = m.finish_task(&me_id, &task_id, outcome, &body, &request_id, now_ms())?;
                        let t: Vec<String> = vec![];
                        Ok((
                            AgentResponse::Finished {
                                run_id: a.worker_id.clone(),
                                task_id: Some(a.id),
                            },
                            t,
                        ))
                    }
                    (None, true) => {
                        if target_id.is_some_and(|t| t != my_id) {
                            return Err(err(AgentErrorCode::Usage, "finish takes --task for a worker's result"));
                        }
                        let t = m.finish_root(&me_id, outcome, &body, &request_id, now_ms())?;
                        Ok((
                            AgentResponse::Finished {
                                run_id: t.run.id.clone(),
                                task_id: None,
                            },
                            t.kill_run_ids,
                        ))
                    }
                    (None, false) => {
                        // A worker without --task reports its current task.
                        let current = current
                            .ok_or_else(|| err(AgentErrorCode::Usage, "no current task; pass --task <id>"))?;
                        let a = m.finish_task(&me_id, &current, outcome, &body, &request_id, now_ms())?;
                        let t: Vec<String> = vec![];
                        Ok((
                            AgentResponse::Finished {
                                run_id: a.worker_id.clone(),
                                task_id: Some(a.id),
                            },
                            t,
                        ))
                    }
                })
            }
            AgentRequest::Stop { target_id } | AgentRequest::Cancel { target_id } => Box::new(move |m| {
                let t = m.stop(&me_id, target_id.as_deref(), now_ms())?;
                let worker_ids = t.kill_run_ids.clone();
                Ok((
                    AgentResponse::Stopped {
                        run_id: t.run.id.clone(),
                        worker_ids,
                    },
                    t.kill_run_ids,
                ))
            }),
            AgentRequest::Register { .. }
            | AgentRequest::Spawn { .. }
            | AgentRequest::Lead { .. }
            | AgentRequest::List
            | AgentRequest::Get { .. }
            | AgentRequest::Task { .. }
            | AgentRequest::Tasks { .. }
            | AgentRequest::Inbox { .. }
            | AgentRequest::Wait { .. } => unreachable!("handled above"),
        };
        let ((response, kill_run_ids), changed) = store.mutate_tracked(move |m| {
            // The authoritative replay check runs here, inside the single
            // writer, so two concurrent requests with the same id cannot
            // both see "unknown": the second finds the first's record and
            // replays it (or conflicts on a different payload).
            if let Some(key) = &key
                && let Some(replay) = m.lookup_request(key, &digest)?
            {
                return Ok((replay, vec![]));
            }
            let (response, kill_run_ids) = op(m)?;
            // Remember the request; `next` only once it handed out a task,
            // so a retry after an empty answer still claims work.
            let remember = match &response {
                AgentResponse::Next(n) => n.task.is_some(),
                _ => true,
            };
            if remember && let Some(key) = &key {
                m.record_request(key, &digest, response.clone(), now_ms());
            }
            Ok((response, kill_run_ids))
        })?;
        self.kill_runs(&kill_run_ids, host);
        // Waiters are woken only by real changes: an empty `next` or `wait`
        // must not wake the very caller that asked, or it spins until its
        // timeout instead of sleeping.
        if changed {
            self.bump();
            self.publish_group(&me.id, host);
        }
        Ok(response)
    }

    /// A blocked `next` or `wait` gave up: the worker is idle again.
    pub fn wait_ended(&self, run_id: &str) {
        let run_id = run_id.to_string();
        let _ = self.inner.store.mutate(move |m| m.mark_worker_idle(&run_id));
    }

    fn kill_runs(&self, kill_run_ids: &[String], host: &mut dyn Host) {
        for run_id in kill_run_ids {
            let run = self.inner.store.read(|m| m.run(run_id).ok().cloned());
            match run {
                Some(r) if !r.terminal_id.is_empty() => host.kill_terminal(&r.terminal_id),
                // Still starting: there is no process yet. The launch path
                // sees the stop when it records the start, and kills what it
                // started; nothing to do here, nothing left reserved.
                Some(r) if r.process_state == ProcessState::Starting => {}
                _ => {
                    let id = run_id.clone();
                    let _ = self
                        .inner
                        .store
                        .mutate(move |m| m.mark_cleanup_failed(&id, "no terminal to kill", now_ms()));
                }
            }
        }
    }

    // ── Launch phases ──────────────────────────────────────────────────

    /// Phase 1: validate the caller and the request, resolve the project
    /// and directory, allocate IDs. No side effects.
    pub fn plan(&self, caller: &Caller, request: &AgentRequest, host: &mut dyn Host) -> Result<Plan, AgentError> {
        request.validate()?;
        match request {
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
                let root = self.resolve_caller(caller)?;
                if !root.role.is_root() {
                    return Err(err(
                        AgentErrorCode::RootOnly,
                        "only the root spawns workers; workers never spawn workers",
                    ));
                }
                if root.root_state != Some(RootState::Active) {
                    return Err(err(AgentErrorCode::AlreadyFinished, "this root is finished"));
                }
                let key = request_key(&root.id, request);
                let digest = request_digest(request);
                if let Some(key) = &key
                    && let Some(replay) = self.inner.store.read(|m| m.lookup_request(key, &digest))?
                {
                    return Ok(Plan::Replay(replay));
                }
                self.inner.store.read(|m| {
                    if m.live_workers_of(&root.id) >= MAX_WORKERS_PER_ROOT {
                        return Err(err(AgentErrorCode::CapacityExceeded, format!("{MAX_WORKERS_PER_ROOT} workers are already live")));
                    }
                    if m.live_workers_total() >= MAX_WORKERS_PER_INSTANCE {
                        return Err(err(AgentErrorCode::CapacityExceeded, format!("{MAX_WORKERS_PER_INSTANCE} workers are already live in NotMux")));
                    }
                    let budget = m.run(&root.id)?.budget.clone().unwrap_or_default();
                    if budget.consumed_starts >= budget.total_starts {
                        return Err(err(AgentErrorCode::BudgetExhausted, "no worker starts left"));
                    }
                    Ok(())
                })?;
                let cwd = resolve_cwd(cwd.as_deref(), &root.cwd)?;
                let run_id = new_id();
                Ok(Plan::Launch(LaunchPlan {
                    kind: PlanKind::Worker {
                        root_id: root.id.clone(),
                        request_id: request_id.clone(),
                        task_id: new_id(),
                        task: task.clone(),
                        argv: argv.clone(),
                        completion: *completion,
                    },
                    harness: *harness,
                    project_id: root.project_id.clone(),
                    cwd,
                    run_dir: self.run_dir(&run_id),
                    run_id,
                    name: name.clone(),
                    executable: executable.clone(),
                    request_key: key,
                    digest,
                    planned_at_ms: now_ms(),
                }))
            }
            AgentRequest::Lead {
                request_id,
                project_id,
                harness,
                total_start_budget,
                name,
                cwd,
                executable,
            } => {
                // A worker process carries its run identity; it may not open
                // a new root, or the flat topology would grow a second level
                // through the back door. Unregistered shells may lead.
                match self.inner.store.read(|m| m.resolve_caller(caller).cloned()) {
                    Ok(run) if !run.role.is_root() => {
                        return Err(err(
                            AgentErrorCode::RootOnly,
                            "a worker cannot open an orchestrator; workers never spawn agents",
                        ));
                    }
                    Ok(_) => {}
                    // An explicit run identity that does not resolve is an
                    // identity error, not a licence; only a pane with no
                    // run identity at all may lead.
                    Err(e) if caller.run_id.is_some() => return Err(e),
                    Err(_) => {}
                }
                let key = Some(format!("lead/{request_id}"));
                let digest = request_digest(request);
                if let Some(key) = &key
                    && let Some(replay) = self.inner.store.read(|m| m.lookup_request(key, &digest))?
                {
                    return Ok(Plan::Replay(replay));
                }
                let project_id = match project_id {
                    Some(id) => id.clone(),
                    None => host
                        .focused_project()
                        .ok_or_else(|| err(AgentErrorCode::Usage, "no focused project; pass --project"))?,
                };
                let project_path = host
                    .project_path(&project_id)
                    .ok_or_else(|| err(AgentErrorCode::NotFound, format!("project {project_id} not found")))?;
                let cwd = resolve_cwd(cwd.as_deref(), &project_path)?;
                let run_id = new_id();
                Ok(Plan::Launch(LaunchPlan {
                    kind: PlanKind::Root {
                        total_start_budget: *total_start_budget,
                    },
                    harness: *harness,
                    project_id,
                    cwd,
                    run_dir: self.run_dir(&run_id),
                    run_id,
                    name: name.clone().unwrap_or_else(|| "Orchestrator".to_string()),
                    executable: executable.clone(),
                    request_key: key,
                    digest,
                    planned_at_ms: now_ms(),
                }))
            }
            _ => Err(err(AgentErrorCode::Usage, "only spawn and lead are planned")),
        }
    }

    /// Phase 2: trust, files, executable. May block; needs no host.
    pub fn prepare(&self, plan: LaunchPlan) -> Result<Prepared, AgentError> {
        self.prepare_with(plan, &harness::NEVER_CANCELLED)
    }

    /// Phase 2 with a cancel flag the adapters check before every write.
    fn prepare_with(&self, plan: LaunchPlan, cancel: &std::sync::atomic::AtomicBool) -> Result<Prepared, AgentError> {
        let (role, parent, task, task_id, budget, argv): (AgentRole, Option<&str>, Option<&str>, Option<&str>, Option<u32>, &[String]) =
            match &plan.kind {
                PlanKind::Root { total_start_budget } => (AgentRole::Root, None, None, None, Some(*total_start_budget), &[]),
                PlanKind::Worker {
                    root_id,
                    task,
                    task_id,
                    argv,
                    ..
                } => (AgentRole::Worker, Some(root_id), Some(task), Some(task_id), None, argv),
            };
        let task_file_path = plan.run_dir.join(harness::TASK_FILE);
        let task_file_str = task_file_path.to_string_lossy().into_owned();
        let facts = RunFacts {
            run_id: &plan.run_id,
            root_id: parent,
            name: &plan.name,
            cwd: &plan.cwd,
            harness: plan.harness.as_str(),
            total_start_budget: budget,
            task_id,
            task_file: task.map(|_| task_file_str.as_str()),
        };
        let prompt = bootstrap::system_prompt(role, &facts);
        let request = LaunchRequest {
            run_id: &plan.run_id,
            parent_run_id: parent,
            role,
            name: &plan.name,
            cwd: Path::new(&plan.cwd),
            run_dir: &plan.run_dir,
            task,
            bootstrap: &prompt,
            executable: plan.executable.as_deref(),
            argv,
            search_path: &self.inner.search_path,
            vendor: &self.inner.vendor,
            cancel,
        };
        let adapter = harness::adapter_for(plan.harness);
        #[cfg(test)]
        std::thread::sleep(std::time::Duration::from_millis(
            PREPARE_DELAY_MS.load(Ordering::SeqCst),
        ));
        // Trust files are edited under a per-file lock that refuses a
        // concurrent editor ("… is busy"). Two spawns prepared at once must
        // not fail each other, and one slow preparation must not hold every
        // other start: a busy file is retried until the caller gives up.
        loop {
            match adapter.prepare(&request) {
                Err(e) if is_busy(&e) && !cancel.load(Ordering::SeqCst) => {
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                result => return result.map(|launch| Prepared { plan, launch }),
            }
        }
    }

    /// Phase 2 with a real time limit: preparation runs on its own thread
    /// and the caller gets `launch_failed` when the deadline passes. A
    /// preparation that finishes late is discarded; its files are removed
    /// and nothing was committed, so nothing leaks into the store.
    pub fn prepare_bounded(&self, plan: LaunchPlan, deadline: std::time::Duration) -> Result<Prepared, AgentError> {
        let (tx, rx) = std::sync::mpsc::channel();
        let runtime = self.clone();
        let run_dir = plan.run_dir.clone();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel_for_thread = Arc::clone(&cancel);
        std::thread::Builder::new()
            .name("orchestration-prepare".into())
            .spawn(move || {
                let result = runtime.prepare_with(plan, &cancel_for_thread);
                if tx.send(result).is_err() {
                    // Nobody waits any more: the deadline passed. Whatever
                    // was written before the cancel flag was seen goes.
                    let _ = std::fs::remove_dir_all(&run_dir);
                }
            })
            .map_err(|e| err(AgentErrorCode::LaunchFailed, format!("spawn preparation thread: {e}")))?;
        match rx.recv_timeout(deadline) {
            Ok(result) => result,
            Err(_) => {
                // The adapters check this flag before every write, so a
                // late preparation touches neither trust files nor run
                // files and releases the preparation lock at its next check.
                cancel.store(true, Ordering::SeqCst);
                Err(err(
                    AgentErrorCode::LaunchFailed,
                    format!(
                        "launch preparation exceeded {} ms; nothing was started",
                        deadline.as_millis()
                    ),
                ))
            }
        }
    }

    /// Phase 3, all steps with one host (tests, and any caller that is on
    /// the UI thread anyway): record the intent, start the process and its
    /// pane, record the start.
    pub fn commit(&self, prepared: Prepared, host: &mut dyn Host) -> Result<AgentResponse, AgentError> {
        let intent = match self.commit_intent(prepared)? {
            IntentOutcome::Replay(response) => return Ok(response),
            IntentOutcome::Proceed(intent) => intent,
        };
        let started = self.start_process(&intent, host);
        let finished = self.finish_start(*intent, started, host);
        if let Some((project_id, slot_id)) = &finished.remove_pane {
            host.remove_pane(project_id, slot_id);
        }
        finished.response
    }

    /// Phase 3a (store only, any thread): record the launch intent. Capacity,
    /// budget and the request record are checked and written in one store
    /// write, so two concurrent spawns with the same request id start at
    /// most one worker: the second one replays the first.
    pub fn commit_intent(&self, prepared: Prepared) -> Result<IntentOutcome, AgentError> {
        let Prepared { plan, launch } = prepared;
        let now = now_ms();
        if now.saturating_sub(plan.planned_at_ms) > LAUNCH_DEADLINE_MS as i64 {
            let _ = std::fs::remove_dir_all(&plan.run_dir);
            return Err(err(
                AgentErrorCode::LaunchFailed,
                format!("launch preparation exceeded {LAUNCH_DEADLINE_MS} ms; nothing was started"),
            ));
        }
        let spec = LaunchSpec {
            harness: plan.harness,
            executable: launch.executable.to_string_lossy().into_owned(),
            cwd: plan.cwd.clone(),
            delivery: launch.delivery,
            completion: match &plan.kind {
                PlanKind::Worker { completion: Some(c), .. } => *c,
                _ => launch.completion,
            },
            files: launch.files.clone(),
            trust_writes: launch.trust_writes.clone(),
        };

        let run_id = plan.run_id.clone();
        let key = plan.request_key.clone();
        let digest = plan.digest.clone();
        // Allocated now and stored with the intent, so the run is found by
        // its terminal from the first moment the process can exit.
        let terminal_id = new_id();
        let slot_id = new_id();
        let replay = match &plan.kind {
            PlanKind::Worker {
                root_id,
                request_id,
                task_id,
                task,
                ..
            } => {
                let (root_id, request_id, task_id, task, name) = (
                    root_id.clone(),
                    request_id.clone(),
                    task_id.clone(),
                    task.clone(),
                    plan.name.clone(),
                );
                let ids = WorkerIds {
                    run_id: run_id.clone(),
                    task_id: task_id.clone(),
                    terminal_id: terminal_id.clone(),
                    slot_id: slot_id.clone(),
                };
                let task_file = spec.files.task_file.clone();
                self.inner.store.mutate(move |m| {
                    if let Some(key) = &key
                        && let Some(replay) = m.lookup_request(key, &digest)?
                    {
                        return Ok(Some(replay));
                    }
                    let (run, _) = m.accept_worker(&root_id, &request_id, ids, &name, &task, spec, now)?;
                    if let Some(path) = task_file {
                        m.set_document_path(&task_id, &path)?;
                    }
                    // Reserve the request id now; the start overwrites it with
                    // the running run, a failed start removes it.
                    if let Some(key) = &key {
                        m.record_request(key, &digest, AgentResponse::Run(run), now);
                    }
                    Ok(None)
                })?
            }
            PlanKind::Root { total_start_budget } => {
                let (project_id, cwd, harness, exe, name, budget) = (
                    plan.project_id.clone(),
                    plan.cwd.clone(),
                    plan.harness,
                    spec.executable.clone(),
                    plan.name.clone(),
                    *total_start_budget,
                );
                let rid = run_id.clone();
                let files = spec.files.clone();
                let trust = spec.trust_writes.clone();
                let (tid, sid) = (terminal_id.clone(), slot_id.clone());
                self.inner.store.mutate(move |m| {
                    if let Some(key) = &key
                        && let Some(replay) = m.lookup_request(key, &digest)?
                    {
                        return Ok(Some(replay));
                    }
                    let mut run = m.register_root(&rid, &tid, &project_id, &cwd, harness, &exe, &name, budget, true, now)?;
                    if let Some(r) = m.runs.get_mut(&rid) {
                        r.files = files;
                        r.trust_writes = trust;
                        r.slot_id = sid;
                        run = r.clone();
                    }
                    if let Some(key) = &key {
                        m.record_request(key, &digest, AgentResponse::Run(run), now);
                    }
                    Ok(None)
                })?
            }
        };
        if let Some(replay) = replay {
            let _ = std::fs::remove_dir_all(&plan.run_dir);
            return Ok(IntentOutcome::Replay(replay));
        }
        self.bump();
        let focus = matches!(plan.kind, PlanKind::Root { .. });
        Ok(IntentOutcome::Proceed(Box::new(Intent {
            plan,
            launch,
            terminal_id,
            slot_id,
            focus,
        })))
    }

    /// Phase 3b (UI thread, no store access): start the process and insert
    /// its pane. On failure nothing of it is left running.
    pub fn start_process(&self, intent: &Intent, host: &mut dyn Host) -> Result<(), String> {
        // Nothing runs once the launch was stopped or the deadline passed.
        // Checked before the start and again after it: a stop that commits
        // in between either finds the terminal registered (its kill works)
        // or is seen by the second check (the process is killed here). No
        // store write happens on this thread; these are reads.
        self.may_start(intent, false)?;
        let mut env = host.terminal_env();
        env.insert("NOTMUX_SLOT_ID".to_string(), intent.slot_id.clone());
        host.launch(&intent.terminal_id, &intent.plan.cwd, &intent.launch, &env)?;
        if let Err(e) = self.may_start(intent, true) {
            host.kill_terminal(&intent.terminal_id);
            return Err(e);
        }
        if let Err(e) = host.insert_pane(
            &intent.plan.project_id,
            &intent.slot_id,
            &intent.terminal_id,
            &intent.plan.run_id,
            &intent.plan.name,
            intent.focus,
        ) {
            host.kill_terminal(&intent.terminal_id);
            return Err(e);
        }
        Ok(())
    }

    /// Whether the intent may still turn into a running process: within the
    /// launch deadline, its run not stopped, its root still active. Before
    /// the launch the run must still be starting; after it, a process that
    /// already exited on its own (and was recorded) is fine.
    fn may_start(&self, intent: &Intent, after_launch: bool) -> Result<(), String> {
        if !after_launch {
            let elapsed = now_ms().saturating_sub(intent.plan.planned_at_ms);
            if elapsed > LAUNCH_DEADLINE_MS as i64 {
                return Err(format!("launch exceeded {LAUNCH_DEADLINE_MS} ms before the process started"));
            }
        }
        let run_id = &intent.plan.run_id;
        self.inner.store.read(|m| {
            let run = m.run(run_id).map_err(|e| e.message)?;
            if after_launch && run.process_state == ProcessState::Exited && run.launch_error.is_none() {
                return Ok(());
            }
            let root_active = m
                .runs
                .get(&m.root_id_of(run))
                .is_some_and(|root| root.root_state == Some(RootState::Active));
            let stopped = !run.role.is_root() && run.worker_state == WorkerAvailability::Stopped;
            if run.process_state != ProcessState::Starting || stopped || !root_active {
                Err(format!("run {run_id} was stopped before its process started"))
            } else {
                Ok(())
            }
        })
    }

    /// Phase 3c (store, any thread): record the start together with the
    /// request record, in one write. If that is impossible (write failure,
    /// or a stop arrived while the process was starting), the process is
    /// killed here and the caller removes the pane on the UI thread.
    pub fn finish_start(&self, intent: Intent, started: Result<(), String>, host: &mut dyn Host) -> FinishOutcome {
        let Intent {
            plan,
            terminal_id,
            slot_id,
            ..
        } = intent;
        let run_id = plan.run_id.clone();
        let pane_was_inserted = started.is_ok();
        let recorded = started.and_then(|()| {
            let (rid, tid, sid) = (run_id.clone(), terminal_id.clone(), slot_id.clone());
            let key = plan.request_key.clone();
            let digest = plan.digest.clone();
            self.inner
                .store
                .mutate(move |m| {
                    // A fast process may have exited, and its exit been
                    // recorded, before this runs: then the run stays as the
                    // exit left it and is never set back to `running`.
                    let current = m.run(&rid)?.clone();
                    let run = if current.process_state == ProcessState::Exited && current.launch_error.is_none() {
                        current
                    } else {
                        m.mark_started(&rid, &tid, &sid, now_ms())?
                    };
                    if let Some(key) = &key {
                        m.record_request(key, &digest, AgentResponse::Run(run.clone()), now_ms());
                    }
                    Ok(run)
                })
                .map_err(|e| format!("run could not be recorded as started: {e}"))
        });
        match recorded {
            Ok(run) => {
                self.bump();
                self.publish_group(&run.id, host);
                FinishOutcome {
                    response: Ok(AgentResponse::Run(run)),
                    remove_pane: None,
                }
            }
            Err(e) => {
                if pane_was_inserted {
                    host.kill_terminal(&terminal_id);
                }
                let (rid, error, key) = (run_id.clone(), e.clone(), plan.request_key.clone());
                let _ = self.inner.store.mutate(move |m| {
                    m.mark_launch_failed(&rid, &error, now_ms())?;
                    // The reservation made by the intent goes: a retry is a
                    // new attempt, not a replay of a run that never ran.
                    if let Some(key) = &key {
                        m.forget_request(key);
                    }
                    Ok(())
                });
                self.bump();
                FinishOutcome {
                    response: Err(err(AgentErrorCode::LaunchFailed, e)),
                    remove_pane: pane_was_inserted.then(|| (plan.project_id.clone(), slot_id.clone())),
                }
            }
        }
    }

    // ── Process events ─────────────────────────────────────────────────

    /// A PTY exited. Returns `true` when it belonged to a managed run.
    pub fn on_terminal_exit(&self, terminal_id: &str, exit_code: Option<u32>, host: &mut dyn Host) -> bool {
        let run_id = self.inner.store.read(|m| {
            m.runs
                .values()
                .filter(|r| r.terminal_id == terminal_id && r.process_state.is_live())
                .map(|r| r.id.clone())
                .next()
        });
        let Some(run_id) = run_id else {
            return false;
        };
        let exit = ProcessExit {
            code: exit_code.map(|c| c as i32),
            signal: None,
        };
        match self.inner.store.mutate(move |m| m.mark_exited(&run_id, exit, now_ms())) {
            Ok(termination) => {
                self.kill_runs(&termination.kill_run_ids, host);
                self.bump();
                self.publish_group(&termination.run.id, host);
            }
            Err(e) => log::error!("orchestration: exit of terminal {terminal_id} not recorded: {e}"),
        }
        true
    }

    /// Terminal IDs of every live managed run, for shutdown.
    pub fn live_terminal_ids(&self) -> Vec<String> {
        self.inner.store.read(|m| {
            m.runs
                .values()
                .filter(|r| r.process_state.is_live() && !r.terminal_id.is_empty())
                // Registered roots are the user's own panes; only launched
                // runs (workers, lead roots with a slot) are ours to kill.
                .filter(|r| !r.role.is_root() || !r.slot_id.is_empty())
                .map(|r| r.terminal_id.clone())
                .collect()
        })
    }

    #[cfg(test)]
    pub(crate) fn model_snapshot(&self) -> Model {
        self.inner.store.read(|m| m.clone())
    }
}

/// A trust file held by another editor right now (see the lock in
/// `notmux_hooks::agent_hooks::config`).
fn is_busy(e: &AgentError) -> bool {
    e.code == AgentErrorCode::TrustPreparationFailed && e.message.contains(" is busy")
}

fn resolve_cwd(requested: Option<&str>, fallback: &str) -> Result<String, AgentError> {
    let cwd = requested.filter(|c| !c.trim().is_empty()).unwrap_or(fallback);
    let path = Path::new(cwd);
    if !path.is_absolute() {
        return Err(err(AgentErrorCode::Usage, format!("cwd must be absolute: {cwd}")));
    }
    if !path.is_dir() {
        return Err(err(AgentErrorCode::Usage, format!("cwd is not a directory: {cwd}")));
    }
    Ok(cwd.to_string())
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// A host that records what the runtime asked for.
    #[derive(Default)]
    pub struct FakeHost {
        pub projects: HashMap<String, String>,
        pub terminals: HashMap<String, String>,
        pub focused: Option<String>,
        pub launches: Vec<(String, PreparedLaunch)>,
        pub panes: Vec<(String, String, String, String, bool)>,
        pub removed: Vec<String>,
        pub killed: Vec<String>,
        pub published: Vec<(String, ProcessState, WorkerAvailability, Option<AssignmentState>)>,
        pub fail_launch: bool,
        pub fail_insert: bool,
        /// Runs inside `launch`, after the process "exists": lets a test
        /// land a stop or an exit exactly while the process is starting.
        pub on_launch: Option<Box<dyn FnMut(&str) + Send>>,
    }

    impl Host for FakeHost {
        fn project_for_terminal(&self, terminal_id: &str) -> Option<(String, String)> {
            let p = self.terminals.get(terminal_id)?;
            Some((p.clone(), self.projects.get(p)?.clone()))
        }
        fn project_path(&self, project_id: &str) -> Option<String> {
            self.projects.get(project_id).cloned()
        }
        fn focused_project(&self) -> Option<String> {
            self.focused.clone()
        }
        fn terminal_env(&self) -> HashMap<String, String> {
            HashMap::from([("USER_VAR".to_string(), "1".to_string())])
        }
        fn launch(&mut self, terminal_id: &str, _cwd: &str, launch: &PreparedLaunch, env: &HashMap<String, String>) -> Result<(), String> {
            if self.fail_launch {
                return Err("spawn failed".into());
            }
            assert!(env.contains_key("NOTMUX_SLOT_ID"));
            self.launches.push((terminal_id.to_string(), launch.clone()));
            if let Some(hook) = self.on_launch.as_mut() {
                hook(terminal_id);
            }
            Ok(())
        }
        fn insert_pane(&mut self, project_id: &str, slot_id: &str, terminal_id: &str, run_id: &str, _name: &str, focus: bool) -> Result<(), String> {
            if self.fail_insert {
                return Err("no such project".into());
            }
            self.terminals.insert(terminal_id.to_string(), project_id.to_string());
            self.panes.push((project_id.into(), slot_id.into(), terminal_id.into(), run_id.into(), focus));
            Ok(())
        }
        fn remove_pane(&mut self, _project_id: &str, slot_id: &str) {
            self.removed.push(slot_id.to_string());
        }
        fn kill_terminal(&mut self, terminal_id: &str) {
            self.killed.push(terminal_id.to_string());
        }
        fn publish_run(&mut self, run: &RunRecord, task_state: Option<AssignmentState>) {
            self.published
                .push((run.id.clone(), run.process_state, run.worker_state, task_state));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::FakeHost;
    use super::*;
    use crate::orchestration::harness::test_support::{fake_exe, vendor_paths};

    struct Fixture {
        _dir: tempfile::TempDir,
        runtime: Runtime,
        host: FakeHost,
        work: String,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        for name in ["notagent", "claude", "codex", "runner"] {
            fake_exe(&bin, name);
        }
        let work = dir.path().join("work");
        std::fs::create_dir(&work).unwrap();
        let (runtime, _) = Runtime::open(&dir.path().join("orch"), vendor_paths(dir.path()), bin.to_string_lossy().into_owned()).unwrap();
        let mut host = FakeHost::default();
        host.projects.insert("p1".into(), work.to_string_lossy().into_owned());
        host.terminals.insert("term-root".into(), "p1".into());
        host.focused = Some("p1".into());
        Fixture {
            _dir: dir,
            runtime,
            host,
            work: work.to_string_lossy().into_owned(),
        }
    }

    fn root_caller() -> Caller {
        Caller {
            run_id: None,
            terminal_id: Some("term-root".into()),
        }
    }

    fn register(f: &mut Fixture, budget: u32) -> RunRecord {
        match f
            .runtime
            .handle(&root_caller(), AgentRequest::Register { harness: HarnessKind::Notagent, total_start_budget: budget }, &mut f.host)
            .unwrap()
        {
            AgentResponse::Run(r) => r,
            other => panic!("{other:?}"),
        }
    }

    fn spawn_req(name: &str, harness: HarnessKind) -> AgentRequest {
        AgentRequest::Spawn {
            request_id: new_id(),
            name: name.into(),
            harness,
            task: "do the thing".into(),
            cwd: None,
            executable: None,
            argv: if harness == HarnessKind::Generic { vec!["runner".into(), "--x".into()] } else { vec![] },
            completion: None,
        }
    }

    fn spawn(f: &mut Fixture, caller: &Caller, name: &str, harness: HarnessKind) -> Result<RunRecord, AgentError> {
        match f.runtime.handle(caller, spawn_req(name, harness), &mut f.host)? {
            AgentResponse::Run(r) => Ok(r),
            other => panic!("{other:?}"),
        }
    }

    fn by_run(run_id: &str) -> Caller {
        Caller {
            run_id: Some(run_id.into()),
            terminal_id: None,
        }
    }

    #[test]
    fn spawn_launches_inserts_pane_and_starts_the_run() {
        let mut f = fixture();
        let root = register(&mut f, 4);
        assert_eq!(root.terminal_id, "term-root");
        let w = spawn(&mut f, &root_caller(), "Worker A", HarnessKind::Claude).unwrap();
        assert_eq!(w.process_state, ProcessState::Running);
        assert_eq!(w.worker_state, WorkerAvailability::Busy);
        assert!(w.current_task_id.is_some());
        assert_eq!(f.host.launches.len(), 1);
        let (tid, launch) = &f.host.launches[0];
        assert_eq!(&w.terminal_id, tid);
        assert!(launch.args.contains(&"--dangerously-skip-permissions".to_string()));
        assert!(launch.env.iter().any(|(k, v)| k == "NOTMUX_RUN_ID" && *v == w.id));
        assert!(launch.env.iter().any(|(k, v)| k == "NOTMUX_PARENT_RUN_ID" && *v == root.id));
        let pane = &f.host.panes[0];
        assert_eq!((pane.0.as_str(), pane.3.as_str(), pane.4), ("p1", w.id.as_str(), false));
        assert_eq!(w.slot_id, pane.1);
        let task = f.runtime.model_snapshot().assignments.values().next().unwrap().clone();
        assert!(task.document_path.ends_with("task.md"));
        assert!(std::fs::read_to_string(&task.document_path).unwrap().contains("do the thing"));
        assert!(w.files.system_prompt_file.is_some());
        // The system prompt names the run.
        let prompt = std::fs::read_to_string(w.files.system_prompt_file.as_ref().unwrap()).unwrap();
        assert!(prompt.contains(&format!("Run ID: {}", w.id)));
        assert!(prompt.contains(&format!("Current task ID: {}", task.id)));
    }

    #[test]
    fn worker_may_not_spawn_and_nothing_is_launched() {
        let mut f = fixture();
        register(&mut f, 4);
        let w = spawn(&mut f, &root_caller(), "a", HarnessKind::Notagent).unwrap();
        let e = spawn(&mut f, &by_run(&w.id), "b", HarnessKind::Notagent).unwrap_err();
        assert_eq!(e.code, AgentErrorCode::RootOnly);
        assert_eq!(f.host.launches.len(), 1);
        assert_eq!(f.runtime.model_snapshot().runs.len(), 2);
    }

    #[test]
    fn identical_retry_replays_without_a_second_worker() {
        let mut f = fixture();
        register(&mut f, 4);
        let req = spawn_req("a", HarnessKind::Codex);
        let first = f.runtime.handle(&root_caller(), req.clone(), &mut f.host).unwrap();
        let again = f.runtime.handle(&root_caller(), req.clone(), &mut f.host).unwrap();
        assert_eq!(first, again);
        assert_eq!(f.host.launches.len(), 1);
        let AgentRequest::Spawn { request_id, .. } = req else { unreachable!() };
        let conflict = AgentRequest::Spawn {
            request_id,
            name: "different".into(),
            harness: HarnessKind::Codex,
            task: "t".into(),
            cwd: None,
            executable: None,
            argv: vec![],
            completion: None,
        };
        assert_eq!(
            f.runtime.handle(&root_caller(), conflict, &mut f.host).unwrap_err().code,
            AgentErrorCode::RequestConflict
        );
    }

    #[test]
    fn launch_failure_rolls_back_pane_and_keeps_the_record() {
        let mut f = fixture();
        register(&mut f, 2);
        f.host.fail_insert = true;
        let e = spawn(&mut f, &root_caller(), "a", HarnessKind::Notagent).unwrap_err();
        assert_eq!(e.code, AgentErrorCode::LaunchFailed);
        assert_eq!(f.host.killed.len(), 1, "the started process is killed");
        assert!(f.host.removed.is_empty(), "no pane was inserted, none to remove");
        let m = f.runtime.model_snapshot();
        let w = m.runs.values().find(|r| !r.role.is_root()).unwrap();
        assert_eq!(w.process_state, ProcessState::Lost);
        assert!(w.launch_error.as_deref().unwrap().contains("no such project"));
        assert_eq!(m.live_workers_total(), 0);
        // Budget consumed by the attempt, capacity free again.
        f.host.fail_insert = false;
        spawn(&mut f, &root_caller(), "b", HarnessKind::Notagent).unwrap();
        assert_eq!(
            spawn(&mut f, &root_caller(), "c", HarnessKind::Notagent).unwrap_err().code,
            AgentErrorCode::BudgetExhausted
        );
    }

    #[test]
    fn worker_cancel_leaves_siblings_and_root_cancel_closes_admission() {
        let mut f = fixture();
        let root = register(&mut f, 8);
        let a = spawn(&mut f, &root_caller(), "a", HarnessKind::Notagent).unwrap();
        let b = spawn(&mut f, &root_caller(), "b", HarnessKind::Claude).unwrap();
        let resp = f
            .runtime
            .handle(&root_caller(), AgentRequest::Cancel { target_id: Some(a.id.clone()) }, &mut f.host)
            .unwrap();
        assert!(matches!(resp, AgentResponse::Stopped { ref worker_ids, .. } if worker_ids == &vec![a.id.clone()]));
        assert_eq!(f.host.killed, vec![a.terminal_id.clone()]);
        let m = f.runtime.model_snapshot();
        assert_eq!(m.runs[&b.id].worker_state, WorkerAvailability::Busy);
        assert_eq!(m.runs[&a.id].worker_state, WorkerAvailability::Stopped);
        // Exit arrives for a: no double finalization, result stays cancelled.
        assert!(f.runtime.on_terminal_exit(&a.terminal_id, Some(0), &mut f.host));
        assert!(!f.runtime.on_terminal_exit(&a.terminal_id, Some(0), &mut f.host), "second exit is unknown");
        let task_a = f.runtime.model_snapshot().assignments.values().find(|t| t.worker_id == a.id).unwrap().clone();
        assert_eq!(task_a.state, AssignmentState::Cancelled);

        // Root cancel: b is killed, later spawns rejected.
        f.runtime.handle(&root_caller(), AgentRequest::Stop { target_id: None }, &mut f.host).unwrap();
        assert!(f.host.killed.contains(&b.terminal_id));
        assert_eq!(f.runtime.model_snapshot().runs[&root.id].root_state, Some(RootState::Cancelled));
        assert_eq!(
            spawn(&mut f, &root_caller(), "c", HarnessKind::Notagent).unwrap_err().code,
            AgentErrorCode::AlreadyFinished
        );
    }

    #[test]
    fn cancellation_before_commit_and_deadline_prevent_the_launch() {
        let mut f = fixture();
        register(&mut f, 4);
        let Plan::Launch(plan) = f.runtime.plan(&root_caller(), &spawn_req("a", HarnessKind::Notagent), &mut f.host).unwrap() else {
            panic!()
        };
        let prepared = f.runtime.prepare(plan).unwrap();
        f.runtime.handle(&root_caller(), AgentRequest::Stop { target_id: None }, &mut f.host).unwrap();
        let e = f.runtime.commit(prepared, &mut f.host).unwrap_err();
        assert_eq!(e.code, AgentErrorCode::AlreadyFinished);
        assert!(f.host.launches.is_empty());
        assert_eq!(f.runtime.model_snapshot().runs.len(), 1);

        let mut f = fixture();
        register(&mut f, 4);
        let Plan::Launch(mut plan) = f.runtime.plan(&root_caller(), &spawn_req("a", HarnessKind::Notagent), &mut f.host).unwrap() else {
            panic!()
        };
        plan.planned_at_ms -= LAUNCH_DEADLINE_MS as i64 + 1;
        let prepared = f.runtime.prepare(plan).unwrap();
        let e = f.runtime.commit(prepared, &mut f.host).unwrap_err();
        assert_eq!(e.code, AgentErrorCode::LaunchFailed);
        assert!(f.host.launches.is_empty());
        assert_eq!(f.runtime.model_snapshot().runs.len(), 1, "late success is not accepted");
    }

    #[test]
    fn finish_next_and_completed_result_survive_cleanup() {
        let mut f = fixture();
        let root = register(&mut f, 4);
        let w = spawn(&mut f, &root_caller(), "a", HarnessKind::Generic).unwrap();
        let task_id = w.current_task_id.clone().unwrap();
        let caller = by_run(&w.id);
        // Unfinished task: next without --after hands the held task out
        // again; naming it waits and keeps the worker busy.
        let AgentResponse::Next(n0) = f.runtime.handle(&caller, AgentRequest::Next { request_id: new_id(), after_task_id: None, timeout_ms: None }, &mut f.host).unwrap() else { panic!() };
        assert_eq!(n0.task.as_ref().map(|t| t.id.as_str()), Some(task_id.as_str()));
        let AgentResponse::Next(n0) = f.runtime.handle(&caller, AgentRequest::Next { request_id: new_id(), after_task_id: Some(task_id.clone()), timeout_ms: None }, &mut f.host).unwrap() else { panic!() };
        assert!(n0.timed_out && n0.task.is_none());
        assert_eq!(f.runtime.model_snapshot().runs[&w.id].worker_state, WorkerAvailability::Busy);
        f.runtime
            .handle(&caller, AgentRequest::Finish { request_id: new_id(), task_id: Some(task_id.clone()), target_id: None, outcome: Outcome::Succeeded, body: "done".into() }, &mut f.host)
            .unwrap();
        let AgentResponse::Next(n) = f.runtime.handle(&caller, AgentRequest::Next { request_id: new_id(), after_task_id: None, timeout_ms: None }, &mut f.host).unwrap() else { panic!() };
        assert!(n.timed_out && n.task.is_none());
        f.runtime.wait_ended(&w.id);
        assert_eq!(f.runtime.model_snapshot().runs[&w.id].worker_state, WorkerAvailability::Idle);
        // Root queues more; next claims it and the claim is remembered.
        let assign = AgentRequest::Assign { request_id: new_id(), worker_id: w.id.clone(), name: "second".into(), task: "more".into() };
        f.runtime.handle(&root_caller(), assign, &mut f.host).unwrap();
        let next = AgentRequest::Next { request_id: new_id(), after_task_id: None, timeout_ms: None };
        let AgentResponse::Next(n1) = f.runtime.handle(&caller, next.clone(), &mut f.host).unwrap() else { panic!() };
        let AgentResponse::Next(n2) = f.runtime.handle(&caller, next, &mut f.host).unwrap() else { panic!() };
        assert_eq!(n1.task.as_ref().unwrap().name, "second");
        assert_eq!(n1, n2, "retry replays the claim");
        // Root sees events and can wait.
        let AgentResponse::Events { page, reason } = f.runtime.handle(&root_caller(), AgentRequest::Wait { after_sequence: 0, timeout_ms: None }, &mut f.host).unwrap() else { panic!() };
        assert_eq!(reason, WaitReason::Events);
        assert!(page.items.iter().any(|e| matches!(e.kind, EventKind::AssignmentFinished { .. })));
        // Process exit after the report keeps the reported result.
        f.runtime
            .handle(&caller, AgentRequest::Finish { request_id: new_id(), task_id: None, target_id: None, outcome: Outcome::Failed, body: "gave up".into() }, &mut f.host)
            .unwrap();
        assert!(f.runtime.on_terminal_exit(&w.terminal_id, Some(0), &mut f.host));
        let m = f.runtime.model_snapshot();
        let first = &m.assignments[&task_id];
        assert_eq!(first.result.as_ref().unwrap().body, "done");
        assert_eq!(m.runs[&w.id].process_state, ProcessState::Exited);
        assert_eq!(m.runs[&root.id].root_state, Some(RootState::Active));
        // Root finishes: nothing left to kill.
        let AgentResponse::Finished { .. } = f.runtime.handle(&root_caller(), AgentRequest::Finish { request_id: new_id(), task_id: None, target_id: None, outcome: Outcome::Succeeded, body: "all".into() }, &mut f.host).unwrap() else { panic!() };
        assert_eq!(f.runtime.model_snapshot().runs[&root.id].root_state, Some(RootState::Completed));
    }

    #[test]
    fn lead_opens_a_managed_root_in_the_focused_project() {
        let mut f = fixture();
        let req = AgentRequest::Lead {
            request_id: new_id(),
            project_id: None,
            harness: HarnessKind::Codex,
            total_start_budget: 3,
            name: None,
            cwd: None,
            executable: None,
        };
        let AgentResponse::Run(root) = f.runtime.handle(&Caller::default(), req.clone(), &mut f.host).unwrap() else { panic!() };
        assert!(root.role.is_root());
        assert_eq!(root.process_state, ProcessState::Running);
        assert_eq!(root.cwd, f.work);
        assert!(f.host.panes[0].4, "the orchestrator pane takes focus");
        let launch = &f.host.launches[0].1;
        assert!(launch.env.iter().any(|(k, v)| k == "NOTMUX_AGENT_ROLE" && v == "root"));
        // The new root can spawn right away by its run id.
        spawn(&mut f, &by_run(&root.id), "w", HarnessKind::Notagent).unwrap();
        // Replay.
        let AgentResponse::Run(again) = f.runtime.handle(&Caller::default(), req, &mut f.host).unwrap() else { panic!() };
        assert_eq!(again.id, root.id);
        assert_eq!(f.host.launches.len(), 2);
    }

    #[test]
    fn restart_interrupts_everything_live() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        fake_exe(&bin, "notagent");
        let work = dir.path().join("w");
        std::fs::create_dir(&work).unwrap();
        let vendor = vendor_paths(dir.path());
        let orch = dir.path().join("orch");
        let (runtime, _) = Runtime::open(&orch, vendor.clone(), bin.to_string_lossy().into_owned()).unwrap();
        let mut host = FakeHost::default();
        host.projects.insert("p".into(), work.to_string_lossy().into_owned());
        host.terminals.insert("t".into(), "p".into());
        let caller = Caller { run_id: None, terminal_id: Some("t".into()) };
        runtime.handle(&caller, AgentRequest::Register { harness: HarnessKind::Notagent, total_start_budget: 2 }, &mut host).unwrap();
        runtime.handle(&caller, spawn_req("a", HarnessKind::Notagent), &mut host).unwrap();
        assert_eq!(runtime.live_terminal_ids().len(), 1);
        drop(runtime);
        let (runtime, opened) = Runtime::open(&orch, vendor, bin.to_string_lossy().into_owned()).unwrap();
        assert_eq!(opened, Opened::Loaded);
        let m = runtime.model_snapshot();
        assert!(m.runs.values().all(|r| !r.process_state.is_live()));
        assert!(m.runs.values().filter(|r| r.role.is_root()).all(|r| r.root_state == Some(RootState::Interrupted)));
        assert!(m.assignments.values().all(|a| a.state == AssignmentState::Interrupted));
        assert!(runtime.live_terminal_ids().is_empty());
    }

    // ── Fixes for the review findings ─────────────────────────────────

    #[test]
    fn a_worker_cannot_open_a_new_root_but_a_plain_pane_can() {
        let mut f = fixture();
        register(&mut f, 4);
        let w = spawn(&mut f, &root_caller(), "a", HarnessKind::Generic).unwrap();
        let lead = |f: &mut Fixture, caller: &Caller| {
            f.runtime.handle(
                caller,
                AgentRequest::Lead {
                    request_id: new_id(),
                    project_id: None,
                    harness: HarnessKind::Notagent,
                    total_start_budget: 2,
                    name: None,
                    cwd: None,
                    executable: None,
                },
                &mut f.host,
            )
        };
        let launches_before = f.host.launches.len();
        let e = lead(&mut f, &by_run(&w.id)).unwrap_err();
        assert_eq!(e.code, AgentErrorCode::RootOnly);
        assert_eq!(f.host.launches.len(), launches_before, "nothing started");
        assert_eq!(f.runtime.model_snapshot().runs.len(), 2);
        // A shell that is nobody's run may still open an orchestrator.
        let plain = Caller {
            run_id: None,
            terminal_id: Some("some-shell".into()),
        };
        assert!(matches!(lead(&mut f, &plain).unwrap(), AgentResponse::Run(r) if r.role.is_root()));
    }

    #[test]
    fn lost_next_answer_redelivers_the_held_task_and_after_waits_for_messages() {
        let mut f = fixture();
        register(&mut f, 4);
        let w = spawn(&mut f, &root_caller(), "a", HarnessKind::Generic).unwrap();
        let first = w.current_task_id.clone().unwrap();
        let caller = by_run(&w.id);
        let next = |f: &mut Fixture, after: Option<&str>| match f
            .runtime
            .handle(
                &caller,
                AgentRequest::Next {
                    request_id: new_id(),
                    after_task_id: after.map(str::to_string),
                    timeout_ms: None,
                },
                &mut f.host,
            )
            .unwrap()
        {
            AgentResponse::Next(n) => n,
            other => panic!("{other:?}"),
        };
        // The answer to the launch was lost: the held task comes again.
        let again = next(&mut f, None);
        assert_eq!(again.task.as_ref().map(|t| t.id.as_str()), Some(first.as_str()));
        assert!(!again.timed_out);
        // Naming the held task waits for messages instead.
        let waiting = next(&mut f, Some(&first));
        assert!(waiting.task.is_none() && waiting.timed_out);
        f.runtime
            .handle(
                &caller,
                AgentRequest::Finish {
                    request_id: new_id(),
                    task_id: Some(first.clone()),
                    target_id: None,
                    outcome: Outcome::Succeeded,
                    body: "ok".into(),
                },
                &mut f.host,
            )
            .unwrap();
        f.runtime
            .handle(
                &root_caller(),
                AgentRequest::Assign {
                    request_id: new_id(),
                    worker_id: w.id.clone(),
                    name: "second".into(),
                    task: "more".into(),
                },
                &mut f.host,
            )
            .unwrap();
        let second = next(&mut f, None).task.unwrap();
        assert_eq!(second.name, "second");
        // Lost again: same task, still running, not a new one.
        let redelivered = next(&mut f, None).task.unwrap();
        assert_eq!(redelivered.id, second.id);
        assert_eq!(f.runtime.model_snapshot().assignments[&second.id].state, AssignmentState::Running);
        assert!(next(&mut f, Some(&second.id)).timed_out);
    }

    #[test]
    fn a_start_that_cannot_be_recorded_takes_process_and_pane_down_again() {
        let mut f = fixture();
        register(&mut f, 4);
        // Writes during commit: accept (1), started + request record (2).
        f.runtime.store().fail_write_at(2);
        let e = spawn(&mut f, &root_caller(), "a", HarnessKind::Generic).unwrap_err();
        assert_eq!(e.code, AgentErrorCode::LaunchFailed);
        assert!(e.message.contains("recorded as started"), "{}", e.message);
        assert_eq!(f.host.launches.len(), 1, "the process was started");
        assert_eq!(f.host.killed.len(), 1, "and killed again");
        assert_eq!(f.host.removed.len(), 1, "its pane removed");
        let m = f.runtime.model_snapshot();
        let w = m.runs.values().find(|r| !r.role.is_root()).unwrap();
        assert_eq!(w.process_state, ProcessState::Lost);
        assert!(w.launch_error.as_deref().unwrap().contains("recorded as started"));
        assert_eq!(m.live_workers_total(), 0, "capacity is free again");
        // The same request retried is not a replay of a phantom run.
        assert!(m.requests.is_empty());
    }

    #[test]
    fn mutation_and_its_request_record_land_together_or_not_at_all() {
        let mut f = fixture();
        let root = register(&mut f, 4);
        let w = spawn(&mut f, &root_caller(), "a", HarnessKind::Generic).unwrap();
        let ok = AgentRequest::Message {
            request_id: new_id(),
            recipient_id: root.id.clone(),
            body: "one".into(),
        };
        f.runtime.handle(&by_run(&w.id), ok.clone(), &mut f.host).unwrap();
        let key = crate::orchestration::request_key(&w.id, &ok).unwrap();
        let digest = crate::orchestration::request_digest(&ok);
        assert!(f.runtime.model_snapshot().lookup_request(&key, &digest).unwrap().is_some());
        // The next write fails: no message, no record, so the retry sends once.
        let failing = AgentRequest::Message {
            request_id: new_id(),
            recipient_id: root.id.clone(),
            body: "two".into(),
        };
        f.runtime.store().fail_write_at(1);
        let e = f.runtime.handle(&by_run(&w.id), failing.clone(), &mut f.host).unwrap_err();
        assert_eq!(e.code, AgentErrorCode::StorageFailed);
        let key2 = crate::orchestration::request_key(&w.id, &failing).unwrap();
        let m = f.runtime.model_snapshot();
        assert_eq!(m.messages.len(), 1);
        assert!(m.lookup_request(&key2, &crate::orchestration::request_digest(&failing)).unwrap().is_none());
        f.runtime.handle(&by_run(&w.id), failing.clone(), &mut f.host).unwrap();
        f.runtime.handle(&by_run(&w.id), failing, &mut f.host).unwrap();
        assert_eq!(f.runtime.model_snapshot().messages.len(), 2, "retry replayed, not duplicated");
    }

    #[test]
    fn preparation_is_bounded_by_the_deadline() {
        let mut f = fixture();
        register(&mut f, 4);
        let Plan::Launch(slow) = f.runtime.plan(&root_caller(), &spawn_req("a", HarnessKind::Generic), &mut f.host).unwrap() else {
            panic!()
        };
        PREPARE_DELAY_MS.store(400, Ordering::SeqCst);
        let e = f.runtime.prepare_bounded(slow, std::time::Duration::from_millis(50)).unwrap_err();
        PREPARE_DELAY_MS.store(0, Ordering::SeqCst);
        assert_eq!(e.code, AgentErrorCode::LaunchFailed);
        assert!(e.message.contains("exceeded 50 ms"), "{}", e.message);
        assert_eq!(f.runtime.model_snapshot().runs.len(), 1, "nothing was committed");
        let Plan::Launch(quick) = f.runtime.plan(&root_caller(), &spawn_req("b", HarnessKind::Generic), &mut f.host).unwrap() else {
            panic!()
        };
        let prepared = f.runtime.prepare_bounded(quick, std::time::Duration::from_secs(10)).unwrap();
        assert!(matches!(f.runtime.commit(prepared, &mut f.host).unwrap(), AgentResponse::Run(_)));
    }

    #[test]
    fn history_stays_readable_after_the_worker_exited() {
        let mut f = fixture();
        register(&mut f, 4);
        let w = spawn(&mut f, &root_caller(), "a", HarnessKind::Generic).unwrap();
        let task_id = w.current_task_id.clone().unwrap();
        assert!(f.runtime.on_terminal_exit(&w.terminal_id, Some(0), &mut f.host));
        let dead = by_run(&w.id);
        assert!(matches!(f.runtime.handle(&dead, AgentRequest::List, &mut f.host).unwrap(), AgentResponse::Runs { runs } if runs.len() == 2));
        assert!(matches!(f.runtime.handle(&dead, AgentRequest::Task { task_id: task_id.clone() }, &mut f.host).unwrap(), AgentResponse::Assignment(a) if a.state == AssignmentState::Succeeded));
        assert!(matches!(f.runtime.handle(&dead, AgentRequest::Inbox { after_sequence: 0, limit: None, unacknowledged_only: false }, &mut f.host).unwrap(), AgentResponse::Messages(_)));
        // Mutations still need a live identity.
        let e = f
            .runtime
            .handle(
                &dead,
                AgentRequest::Message {
                    request_id: new_id(),
                    recipient_id: w.id.clone(),
                    body: "x".into(),
                },
                &mut f.host,
            )
            .unwrap_err();
        assert_eq!(e.code, AgentErrorCode::StaleIdentity);
    }

    #[test]
    fn published_state_carries_the_task_outcome() {
        let mut f = fixture();
        register(&mut f, 4);
        let w = spawn(&mut f, &root_caller(), "a", HarnessKind::Generic).unwrap();
        let task_id = w.current_task_id.clone().unwrap();
        let last_for = |f: &Fixture| f.host.published.iter().rev().find(|p| p.0 == w.id).cloned().unwrap();
        assert_eq!(last_for(&f).3, Some(AssignmentState::Running));
        f.runtime
            .handle(
                &by_run(&w.id),
                AgentRequest::Finish {
                    request_id: new_id(),
                    task_id: Some(task_id),
                    target_id: None,
                    outcome: Outcome::Failed,
                    body: "no".into(),
                },
                &mut f.host,
            )
            .unwrap();
        let p = last_for(&f);
        assert_eq!(p.3, Some(AssignmentState::Failed), "outcome, not just availability");
        assert_eq!(p.2, WorkerAvailability::Idle);
    }

    // ── Second review pass ────────────────────────────────────────────

    #[test]
    fn an_unchanged_answer_notifies_nobody() {
        let mut f = fixture();
        register(&mut f, 4);
        let w = spawn(&mut f, &root_caller(), "a", HarnessKind::Generic).unwrap();
        let task = w.current_task_id.clone().unwrap();
        let next = AgentRequest::Next {
            request_id: new_id(),
            after_task_id: Some(task.clone()),
            timeout_ms: None,
        };
        let mut changes = f.runtime.subscribe();
        // Busy worker naming its task: nothing changes, nobody is woken.
        changes.borrow_and_update();
        f.runtime.handle(&by_run(&w.id), next.clone(), &mut f.host).unwrap();
        assert!(!changes.has_changed().unwrap(), "empty next woke waiters");
        let published = f.host.published.len();
        f.runtime.handle(&by_run(&w.id), next, &mut f.host).unwrap();
        assert!(!changes.has_changed().unwrap());
        assert_eq!(f.host.published.len(), published, "nothing to publish either");
        // A real change does notify.
        f.runtime
            .handle(
                &root_caller(),
                AgentRequest::Message {
                    request_id: new_id(),
                    recipient_id: w.id.clone(),
                    body: "x".into(),
                },
                &mut f.host,
            )
            .unwrap();
        assert!(changes.has_changed().unwrap());
    }

    #[test]
    fn lead_with_an_identity_that_does_not_resolve_is_refused() {
        let mut f = fixture();
        register(&mut f, 4);
        let lead = AgentRequest::Lead {
            request_id: new_id(),
            project_id: None,
            harness: HarnessKind::Notagent,
            total_start_budget: 2,
            name: None,
            cwd: None,
            executable: None,
        };
        let unknown = Caller {
            run_id: Some(new_id()),
            terminal_id: None,
        };
        assert_eq!(
            f.runtime.handle(&unknown, lead.clone(), &mut f.host).unwrap_err().code,
            AgentErrorCode::StaleIdentity
        );
        // A worker's id claimed from another terminal: identity error too.
        let w = spawn(&mut f, &root_caller(), "a", HarnessKind::Generic).unwrap();
        let mismatched = Caller {
            run_id: Some(w.id.clone()),
            terminal_id: Some("term-root".into()),
        };
        assert_eq!(
            f.runtime.handle(&mismatched, lead, &mut f.host).unwrap_err().code,
            AgentErrorCode::StaleIdentity
        );
        assert_eq!(f.runtime.model_snapshot().runs.len(), 2, "no new root");
    }

    #[test]
    fn the_shown_task_state_is_the_one_being_worked_on() {
        let mut f = fixture();
        register(&mut f, 4);
        let w = spawn(&mut f, &root_caller(), "a", HarnessKind::Generic).unwrap();
        f.runtime
            .handle(
                &root_caller(),
                AgentRequest::Assign {
                    request_id: new_id(),
                    worker_id: w.id.clone(),
                    name: "later".into(),
                    task: "t".into(),
                },
                &mut f.host,
            )
            .unwrap();
        let last = f.host.published.iter().rev().find(|p| p.0 == w.id).cloned().unwrap();
        assert_eq!(last.3, Some(AssignmentState::Running), "not the queued follow-up");
    }

    #[test]
    fn a_cancelled_preparation_writes_nothing() {
        let mut f = fixture();
        register(&mut f, 4);
        let Plan::Launch(plan) = f.runtime.plan(&root_caller(), &spawn_req("a", HarnessKind::Claude), &mut f.host).unwrap() else {
            panic!()
        };
        let run_dir = plan.run_dir.clone();
        let cancelled = std::sync::atomic::AtomicBool::new(true);
        let e = f.runtime.prepare_with(plan, &cancelled).unwrap_err();
        assert_eq!(e.code, AgentErrorCode::LaunchFailed);
        assert!(!run_dir.exists(), "no run files");
        let vendor = &f.runtime.inner.vendor;
        assert!(!vendor.claude_json.exists(), "no trust file touched");
        assert!(!vendor.claude_settings.exists());
    }

    #[test]
    fn a_dead_run_is_readable_from_any_pane() {
        let mut f = fixture();
        register(&mut f, 4);
        let w = spawn(&mut f, &root_caller(), "a", HarnessKind::Generic).unwrap();
        assert!(f.runtime.on_terminal_exit(&w.terminal_id, Some(0), &mut f.host));
        // `notmux agent list --run <old id>` from a new pane sends that
        // pane's terminal id as well.
        let from_new_pane = Caller {
            run_id: Some(w.id.clone()),
            terminal_id: Some("a-new-pane".into()),
        };
        assert!(matches!(
            f.runtime.handle(&from_new_pane, AgentRequest::List, &mut f.host).unwrap(),
            AgentResponse::Runs { runs } if runs.len() == 2
        ));
        // A live run is still bound to its pane.
        let root_elsewhere = Caller {
            run_id: Some(f.runtime.model_snapshot().runs.values().find(|r| r.role.is_root()).unwrap().id.clone()),
            terminal_id: Some("a-new-pane".into()),
        };
        assert_eq!(
            f.runtime.handle(&root_elsewhere, AgentRequest::List, &mut f.host).unwrap_err().code,
            AgentErrorCode::StaleIdentity
        );
    }

    // ── Third review pass ─────────────────────────────────────────────

    /// Many threads send the same request at once. The replay check runs
    /// inside the single writer, so exactly one message exists afterwards
    /// and every thread gets that message back.
    #[test]
    fn concurrent_retries_of_one_request_create_one_message() {
        let mut f = fixture();
        let root = register(&mut f, 4);
        let w = spawn(&mut f, &root_caller(), "a", HarnessKind::Generic).unwrap();
        for round in 0..20 {
            let request = AgentRequest::Message {
                request_id: new_id(),
                recipient_id: root.id.clone(),
                body: format!("round {round}"),
            };
            let barrier = Arc::new(std::sync::Barrier::new(8));
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let (runtime, request, barrier, caller) =
                        (f.runtime.clone(), request.clone(), Arc::clone(&barrier), by_run(&w.id));
                    std::thread::spawn(move || {
                        let mut host = FakeHost::default();
                        barrier.wait();
                        runtime.handle(&caller, request, &mut host)
                    })
                })
                .collect();
            let answers: Vec<_> = handles.into_iter().map(|h| h.join().unwrap().unwrap()).collect();
            let m = f.runtime.model_snapshot();
            let sent: Vec<_> = m.messages.values().filter(|x| x.body == format!("round {round}")).collect();
            assert_eq!(sent.len(), 1, "round {round}: one request, one message");
            for a in &answers {
                assert!(matches!(a, AgentResponse::Message(msg) if msg.id == sent[0].id));
            }
        }
    }

    /// The same request id with two payloads at once: one wins, the others
    /// are conflicts, never a second message.
    #[test]
    fn concurrent_conflicting_payloads_never_both_apply() {
        let mut f = fixture();
        let root = register(&mut f, 4);
        let w = spawn(&mut f, &root_caller(), "a", HarnessKind::Generic).unwrap();
        for _ in 0..20 {
            let id = new_id();
            let barrier = Arc::new(std::sync::Barrier::new(6));
            let handles: Vec<_> = (0..6)
                .map(|i| {
                    let request = AgentRequest::Message {
                        request_id: id.clone(),
                        recipient_id: root.id.clone(),
                        body: format!("payload {}", i % 2),
                    };
                    let (runtime, barrier, caller) = (f.runtime.clone(), Arc::clone(&barrier), by_run(&w.id));
                    std::thread::spawn(move || {
                        let mut host = FakeHost::default();
                        barrier.wait();
                        runtime.handle(&caller, request, &mut host)
                    })
                })
                .collect();
            let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
            let conflicts = results
                .iter()
                .filter(|r| matches!(r, Err(e) if e.code == AgentErrorCode::RequestConflict))
                .count();
            let ok = results.iter().filter(|r| r.is_ok()).count();
            assert_eq!(ok + conflicts, 6);
            assert_eq!(conflicts, 3, "the three with the other payload conflict");
            let m = f.runtime.model_snapshot();
            assert_eq!(m.messages.values().filter(|x| x.request_id == id).count(), 1);
        }
    }

    /// Two spawns with one request id, both past preparation, record their
    /// intent concurrently: one worker, the other a replay.
    #[test]
    fn concurrent_intents_of_one_spawn_start_one_worker() {
        let mut f = fixture();
        register(&mut f, 8);
        let request = spawn_req("a", HarnessKind::Generic);
        let prepared: Vec<Prepared> = (0..2)
            .map(|_| {
                let Plan::Launch(plan) = f.runtime.plan(&root_caller(), &request, &mut f.host).unwrap() else {
                    panic!()
                };
                f.runtime.prepare(plan).unwrap()
            })
            .collect();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = prepared
            .into_iter()
            .map(|p| {
                let (runtime, barrier) = (f.runtime.clone(), Arc::clone(&barrier));
                std::thread::spawn(move || {
                    barrier.wait();
                    runtime.commit_intent(p).unwrap()
                })
            })
            .collect();
        let outcomes: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let proceeds = outcomes.iter().filter(|o| matches!(o, IntentOutcome::Proceed(_))).count();
        assert_eq!(proceeds, 1);
        assert_eq!(f.runtime.model_snapshot().runs.values().filter(|r| !r.role.is_root()).count(), 1);
    }

    /// Plan, prepare and record the intent of a generic worker spawn.
    fn intent_for(f: &mut Fixture, name: &str, harness: HarnessKind) -> Box<Intent> {
        let Plan::Launch(plan) = f.runtime.plan(&root_caller(), &spawn_req(name, harness), &mut f.host).unwrap() else {
            panic!()
        };
        let prepared = f.runtime.prepare(plan).unwrap();
        let IntentOutcome::Proceed(intent) = f.runtime.commit_intent(prepared).unwrap() else {
            panic!()
        };
        intent
    }

    fn assert_undone(f: &Fixture, run_id: &str) {
        let m = f.runtime.model_snapshot();
        assert_eq!(m.runs[run_id].process_state, ProcessState::Lost);
        assert_eq!(m.live_workers_total(), 0, "capacity free");
        assert_eq!(m.runs[run_id].cleanup_error, None);
        assert!(m.requests.is_empty(), "the reservation is gone");
    }

    /// A stop confirmed before the process starts means the program never
    /// runs: no launch at all.
    #[test]
    fn a_stop_before_the_start_means_the_program_never_runs() {
        for stop_whole_group in [false, true] {
            let mut f = fixture();
            register(&mut f, 4);
            let intent = intent_for(&mut f, "a", HarnessKind::Generic);
            let run_id = intent.plan.run_id.clone();
            let target = (!stop_whole_group).then(|| run_id.clone());
            f.runtime
                .handle(&root_caller(), AgentRequest::Stop { target_id: target }, &mut f.host)
                .unwrap();
            let started = f.runtime.start_process(&intent, &mut f.host);
            let e = started.clone().unwrap_err();
            assert!(e.contains("stopped before its process started"), "{e}");
            assert!(f.host.launches.is_empty(), "the program was never started");
            assert!(f.host.panes.is_empty());
            let finished = f.runtime.finish_start(*intent, started, &mut f.host);
            assert_eq!(finished.response.unwrap_err().code, AgentErrorCode::LaunchFailed);
            assert_eq!(finished.remove_pane, None, "no pane was inserted");
            assert_undone(&f, &run_id);
        }
    }

    /// A stop that commits while the process is being started: the check
    /// after the launch sees it and kills the process at once, before the
    /// pane is even inserted.
    #[test]
    fn a_stop_during_the_launch_kills_the_process_immediately() {
        let mut f = fixture();
        register(&mut f, 4);
        let intent = intent_for(&mut f, "a", HarnessKind::Generic);
        let run_id = intent.plan.run_id.clone();
        let runtime = f.runtime.clone();
        f.host.on_launch = Some(Box::new(move |_terminal| {
            runtime
                .handle(&root_caller(), AgentRequest::Stop { target_id: None }, &mut FakeHost::default())
                .unwrap();
        }));
        let started = f.runtime.start_process(&intent, &mut f.host);
        assert!(started.is_err());
        assert_eq!(f.host.launches.len(), 1);
        assert!(f.host.killed.contains(&intent.terminal_id), "killed right after the launch");
        assert!(f.host.panes.is_empty(), "never shown");
        let finished = f.runtime.finish_start(*intent, started, &mut f.host);
        assert!(finished.response.is_err());
        assert_undone(&f, &run_id);
    }

    /// Defence in depth: should the start be reported as done although a
    /// stop landed after the last check, recording it is refused and the
    /// process is killed and its pane removed.
    #[test]
    fn recording_a_start_after_a_stop_is_refused() {
        let mut f = fixture();
        register(&mut f, 4);
        let intent = intent_for(&mut f, "a", HarnessKind::Generic);
        let run_id = intent.plan.run_id.clone();
        f.runtime.start_process(&intent, &mut f.host).unwrap();
        f.runtime
            .handle(&root_caller(), AgentRequest::Stop { target_id: Some(run_id.clone()) }, &mut f.host)
            .unwrap();
        let finished = f.runtime.finish_start(*intent.clone(), Ok(()), &mut f.host);
        let e = finished.response.unwrap_err();
        assert!(e.message.contains("stopped before its process started"), "{}", e.message);
        assert!(f.host.killed.contains(&intent.terminal_id));
        assert_eq!(finished.remove_pane, Some((intent.plan.project_id.clone(), intent.slot_id.clone())));
        assert_undone(&f, &run_id);
    }

    /// The process exits before its start is recorded. The exit recorder
    /// finds the run by the terminal recorded with the intent, and the start
    /// never sets the finished run back to `running`.
    #[test]
    fn an_exit_before_the_start_is_recorded_is_not_lost() {
        let mut f = fixture();
        register(&mut f, 4);
        let intent = intent_for(&mut f, "a", HarnessKind::Generic);
        let run_id = intent.plan.run_id.clone();
        f.runtime.start_process(&intent, &mut f.host).unwrap();
        // The exit recorder runs first.
        assert!(f.runtime.on_terminal_exit(&intent.terminal_id, Some(0), &mut f.host));
        let finished = f.runtime.finish_start(*intent, Ok(()), &mut f.host);
        let AgentResponse::Run(run) = finished.response.unwrap() else { panic!() };
        assert_eq!(run.process_state, ProcessState::Exited);
        let m = f.runtime.model_snapshot();
        assert_eq!(m.runs[&run_id].process_state, ProcessState::Exited);
        let task = m.assignments.values().find(|a| a.worker_id == run_id).unwrap();
        assert_eq!(task.state, AssignmentState::Succeeded, "exit code 0 decided the task");
        assert_eq!(m.live_workers_total(), 0, "no capacity held by a dead process");
        assert!(finished.remove_pane.is_none(), "the pane shows the finished process");
    }

    /// Even faster: the exit is recorded while the launch is still in
    /// progress. The start goes through and reports the exited run; a
    /// reported (non-generic) worker's task is interrupted, not hanging.
    #[test]
    fn an_exit_during_the_launch_is_not_lost() {
        let mut f = fixture();
        register(&mut f, 4);
        let intent = intent_for(&mut f, "a", HarnessKind::Claude);
        let run_id = intent.plan.run_id.clone();
        let runtime = f.runtime.clone();
        f.host.on_launch = Some(Box::new(move |terminal| {
            assert!(runtime.on_terminal_exit(terminal, Some(1), &mut FakeHost::default()));
        }));
        f.runtime.start_process(&intent, &mut f.host).unwrap();
        let finished = f.runtime.finish_start(*intent, Ok(()), &mut f.host);
        let AgentResponse::Run(run) = finished.response.unwrap() else { panic!() };
        assert_eq!(run.process_state, ProcessState::Exited);
        let m = f.runtime.model_snapshot();
        let task = m.assignments.values().find(|a| a.worker_id == run_id).unwrap();
        assert_eq!(task.state, AssignmentState::Interrupted);
        assert_eq!(m.live_workers_total(), 0);
    }

    /// The launch deadline also covers the wait before the process start.
    #[test]
    fn a_start_after_the_deadline_never_runs() {
        let mut f = fixture();
        register(&mut f, 4);
        let mut intent = intent_for(&mut f, "a", HarnessKind::Generic);
        let run_id = intent.plan.run_id.clone();
        intent.plan.planned_at_ms -= LAUNCH_DEADLINE_MS as i64 + 1;
        let started = f.runtime.start_process(&intent, &mut f.host);
        assert!(started.clone().unwrap_err().contains("exceeded"));
        assert!(f.host.launches.is_empty());
        let _ = f.runtime.finish_start(*intent, started, &mut f.host);
        assert_undone(&f, &run_id);
    }

    /// The UI step of a launch touches no store state.
    #[test]
    fn starting_the_process_does_not_touch_the_store() {
        let mut f = fixture();
        register(&mut f, 4);
        let Plan::Launch(plan) =
            f.runtime.plan(&root_caller(), &spawn_req("a", HarnessKind::Generic), &mut f.host).unwrap()
        else {
            panic!()
        };
        let prepared = f.runtime.prepare(plan).unwrap();
        let IntentOutcome::Proceed(intent) = f.runtime.commit_intent(prepared).unwrap() else {
            panic!()
        };
        let before = f.runtime.model_snapshot();
        f.runtime.start_process(&intent, &mut f.host).unwrap();
        assert_eq!(f.runtime.model_snapshot(), before);
        let finished = f.runtime.finish_start(*intent, Ok(()), &mut f.host);
        assert!(matches!(finished.response, Ok(AgentResponse::Run(r)) if r.process_state == ProcessState::Running));
    }

    /// A trust file held by another editor is waited for, not failed; a
    /// cancelled preparation stops waiting and writes nothing.
    #[test]
    fn a_busy_trust_file_is_retried_until_free_or_cancelled() {
        let mut f = fixture();
        register(&mut f, 4);
        let vendor = f.runtime.inner.vendor.clone();
        std::fs::create_dir_all(vendor.claude_json.parent().unwrap()).unwrap();
        // The lock `ConfigFile::load` takes: `.{file name}.notmux.lock`.
        let lock_path = vendor.claude_json.with_file_name("..claude.json.notmux.lock");
        let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&lock_path).unwrap();
        lock.lock().unwrap();

        // Released after a moment: the preparation succeeds.
        let Plan::Launch(plan) =
            f.runtime.plan(&root_caller(), &spawn_req("a", HarnessKind::Claude), &mut f.host).unwrap()
        else {
            panic!()
        };
        let runtime = f.runtime.clone();
        let handle = std::thread::spawn(move || runtime.prepare(plan));
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert!(!handle.is_finished(), "it waits for the lock");
        lock.unlock().unwrap();
        assert!(handle.join().unwrap().is_ok());

        // Held and cancelled: it gives up without writing its run files.
        lock.lock().unwrap();
        let Plan::Launch(plan) =
            f.runtime.plan(&root_caller(), &spawn_req("b", HarnessKind::Claude), &mut f.host).unwrap()
        else {
            panic!()
        };
        let run_dir = plan.run_dir.clone();
        let e = f.runtime.prepare_bounded(plan, std::time::Duration::from_millis(100)).unwrap_err();
        assert_eq!(e.code, AgentErrorCode::LaunchFailed);
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(!run_dir.join(harness::SYSTEM_PROMPT_FILE).exists());
        lock.unlock().unwrap();
    }
}
