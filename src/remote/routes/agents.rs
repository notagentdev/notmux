//! `POST /v1/agents`: the agent orchestration API for the local CLI.
//!
//! Loopback and the local CLI token are required explicitly here, on top of
//! the protected router's middleware: paired remote tokens never reach the
//! orchestration. Launches run in three steps so the GPUI thread only sees
//! the fast parts; `wait` and `next` are served by this task, which re-asks
//! the runtime whenever it reports a change, until the client's timeout.

use super::AppState;
use crate::remote::bridge::{AgentCommand, AgentReply, BridgeMessage, CommandResult, RemoteCommand};
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use notmux_core::orchestration::*;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

fn status_for(code: AgentErrorCode) -> StatusCode {
    use AgentErrorCode::*;
    match code {
        Usage => StatusCode::BAD_REQUEST,
        NotFound => StatusCode::NOT_FOUND,
        NotRegistered | StaleIdentity | ScopeViolation | RootOnly | WorkerOnly => StatusCode::FORBIDDEN,
        CapacityExceeded | BudgetExhausted | QueueFull | MailboxFull | RequestConflict | AlreadyFinished => {
            StatusCode::CONFLICT
        }
        HarnessUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        LaunchFailed | TrustPreparationFailed | StorageFailed | Unsupported => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn error_response(error: AgentError) -> Response {
    (status_for(error.code), Json(serde_json::json!({ "error": error }))).into_response()
}

fn ok_response(response: AgentResponse) -> Response {
    (StatusCode::OK, Json(response)).into_response()
}

fn respond(result: Result<AgentResponse, AgentError>) -> Response {
    match result {
        Ok(r) => ok_response(r),
        Err(e) => error_response(e),
    }
}

async fn bridge(state: &AppState, command: AgentCommand) -> Result<AgentReply, AgentError> {
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let msg = BridgeMessage {
        command: RemoteCommand::Agent(command),
        reply: Some(reply_tx),
    };
    if state.bridge_tx.send(msg).await.is_err() {
        return Err(AgentError::new(AgentErrorCode::StorageFailed, "bridge unavailable"));
    }
    match reply_rx.await {
        Ok(CommandResult::Agent(reply)) => Ok(reply),
        Ok(other) => Err(AgentError::new(
            AgentErrorCode::StorageFailed,
            format!("unexpected bridge reply: {other:?}"),
        )),
        Err(_) => Err(AgentError::new(AgentErrorCode::StorageFailed, "command processing failed")),
    }
}

/// Answer a request. Everything that only touches the store, processes and
/// terminal models runs on a blocking worker thread, never on the UI
/// thread; only `register` (project lookup) goes through the GPUI bridge.
fn unexpected(reply: AgentReply) -> AgentError {
    AgentError::new(AgentErrorCode::StorageFailed, format!("unexpected reply {reply:?}"))
}

fn background_host(project: Option<(String, String)>) -> Option<crate::orchestration::app_host::BackgroundHost> {
    crate::orchestration::background_parts().map(|parts| crate::orchestration::app_host::BackgroundHost {
        backend: parts.backend,
        terminals: parts.terminals,
        project,
    })
}

/// Run a closure with the runtime and a background host on a blocking
/// worker thread: store writes never happen on the UI thread.
async fn off_ui<T: Send + 'static>(
    project: Option<(String, String)>,
    f: impl FnOnce(&crate::orchestration::runtime::Runtime, &mut crate::orchestration::app_host::BackgroundHost) -> T
        + Send
        + 'static,
) -> Result<T, AgentError> {
    let runtime = crate::orchestration::runtime()
        .ok_or_else(|| AgentError::new(AgentErrorCode::StorageFailed, "agent orchestration is not available"))?;
    let mut host = background_host(project)
        .ok_or_else(|| AgentError::new(AgentErrorCode::StorageFailed, "agent orchestration is not available"))?;
    tokio::task::spawn_blocking(move || f(&runtime, &mut host))
        .await
        .map_err(|e| AgentError::new(AgentErrorCode::StorageFailed, format!("request task failed: {e}")))
}

/// Answer a request. Everything that touches the store runs on a blocking
/// worker thread, never on the UI thread; `register` first asks the UI for
/// the project of the caller's terminal (a lookup, no store access).
async fn handle_once(state: &AppState, caller: Caller, request: AgentRequest) -> Result<AgentResponse, AgentError> {
    let project = match (&request, &caller.terminal_id) {
        (AgentRequest::Register { .. }, Some(terminal_id)) => {
            match bridge(state, AgentCommand::ProjectForTerminal { terminal_id: terminal_id.clone() }).await? {
                AgentReply::Project(p) => p,
                other => return Err(unexpected(other)),
            }
        }
        _ => None,
    };
    off_ui(project, move |runtime, host| runtime.handle(&caller, request, host)).await?
}

/// Whether a `wait`/`next` answer says "nothing yet".
fn is_empty(response: &AgentResponse) -> bool {
    match response {
        AgentResponse::Events { reason, .. } => *reason == WaitReason::Timeout,
        AgentResponse::Next(n) => n.timed_out,
        _ => false,
    }
}

async fn launch(state: &AppState, caller: Caller, request: AgentRequest) -> Result<AgentResponse, AgentError> {
    let runtime = crate::orchestration::runtime()
        .ok_or_else(|| AgentError::new(AgentErrorCode::StorageFailed, "agent orchestration is not available"))?;
    let plan = match bridge(state, AgentCommand::Plan { caller, request }).await? {
        AgentReply::Plan(p) => p?,
        other => {
            return Err(AgentError::new(
                AgentErrorCode::StorageFailed,
                format!("unexpected reply {other:?}"),
            ));
        }
    };
    let plan = match plan {
        crate::orchestration::runtime::Plan::Replay(response) => return Ok(response),
        crate::orchestration::runtime::Plan::Launch(plan) => plan,
    };
    // Trust files may block: off the UI thread, and bounded by the launch
    // deadline so a hanging file system answers the caller instead of
    // holding the request forever.
    let rt = runtime.clone();
    let prepared = tokio::task::spawn_blocking(move || {
        rt.prepare_bounded(plan, Duration::from_millis(LAUNCH_DEADLINE_MS))
    })
    .await
    .map_err(|e| AgentError::new(AgentErrorCode::LaunchFailed, format!("preparation task failed: {e}")))??;
    // Record the intent off the UI thread.
    let intent = match off_ui(None, move |runtime, _| runtime.commit_intent(prepared)).await?? {
        crate::orchestration::runtime::IntentOutcome::Replay(response) => return Ok(response),
        crate::orchestration::runtime::IntentOutcome::Proceed(intent) => intent,
    };
    // Process and pane on the UI thread, without store access.
    let started = match bridge(state, AgentCommand::Start { intent: intent.clone() }).await? {
        AgentReply::Started(s) => s,
        other => return Err(unexpected(other)),
    };
    // Record the start off the UI thread; undo the pane if that fails.
    let finished = off_ui(None, move |runtime, host| runtime.finish_start(*intent, started, host)).await?;
    if let Some((project_id, slot_id)) = finished.remove_pane {
        let _ = bridge(state, AgentCommand::RemovePane { project_id, slot_id }).await;
    }
    finished.response
}

async fn wait_for(
    state: &AppState,
    caller: Caller,
    request: AgentRequest,
    timeout_ms: Option<u64>,
) -> Result<AgentResponse, AgentError> {
    let runtime = crate::orchestration::runtime()
        .ok_or_else(|| AgentError::new(AgentErrorCode::StorageFailed, "agent orchestration is not available"))?;
    let timeout = Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS).min(MAX_WAIT_TIMEOUT_MS));
    let is_next = matches!(request, AgentRequest::Next { .. });
    let (response, _asked) = wait_loop(&runtime, timeout, || handle_once(state, caller.clone(), request.clone())).await?;
    if is_empty(&response) && is_next {
        let rt = runtime.clone();
        let c = caller.clone();
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(run) = rt.store().read(|m| m.resolve_caller(&c).cloned()) {
                rt.wait_ended(&run.id);
            }
        })
        .await;
    }
    Ok(response)
}

/// Ask until the answer is not empty or the timeout passes, sleeping
/// between asks until the runtime reports a change. Returns the last
/// answer and how often it asked.
async fn wait_loop<F, Fut>(
    runtime: &crate::orchestration::runtime::Runtime,
    timeout: Duration,
    mut ask: F,
) -> Result<(AgentResponse, usize), AgentError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<AgentResponse, AgentError>>,
{
    let deadline = Instant::now() + timeout;
    let mut changes = runtime.subscribe();
    let mut asked = 0usize;
    loop {
        // Mark what we have seen before asking, so a change between the
        // answer and the wait is not missed.
        changes.borrow_and_update();
        let response = ask().await?;
        asked += 1;
        if !is_empty(&response) {
            return Ok((response, asked));
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok((response, asked));
        }
        // Wake on the next change or at the deadline, whichever is first.
        let _ = tokio::time::timeout(deadline - now, changes.changed()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::harness::test_support::{fake_exe, vendor_paths};
    use crate::orchestration::new_id;
    use crate::orchestration::runtime::Runtime;
    use crate::orchestration::runtime::test_support::FakeHost;
    use std::sync::{Arc, Mutex};

    struct Setup {
        _dir: tempfile::TempDir,
        runtime: Runtime,
        host: Arc<Mutex<FakeHost>>,
        root: RunRecord,
        worker: RunRecord,
    }

    fn setup() -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        fake_exe(&bin, "runner");
        let work = dir.path().join("w");
        std::fs::create_dir(&work).unwrap();
        let (runtime, _) =
            Runtime::open(&dir.path().join("orch"), vendor_paths(dir.path()), bin.to_string_lossy().into_owned()).unwrap();
        let mut host = FakeHost::default();
        host.projects.insert("p".into(), work.to_string_lossy().into_owned());
        host.terminals.insert("t".into(), "p".into());
        let by_term = Caller {
            run_id: None,
            terminal_id: Some("t".into()),
        };
        let AgentResponse::Run(root) = runtime
            .handle(&by_term, AgentRequest::Register { harness: HarnessKind::Notagent, total_start_budget: 2 }, &mut host)
            .unwrap()
        else {
            panic!()
        };
        let AgentResponse::Run(worker) = runtime
            .handle(
                &by_term,
                AgentRequest::Spawn {
                    request_id: new_id(),
                    name: "w".into(),
                    harness: HarnessKind::Generic,
                    task: "t".into(),
                    cwd: None,
                    executable: None,
                    argv: vec!["runner".into()],
                    completion: None,
                },
                &mut host,
            )
            .unwrap()
        else {
            panic!()
        };
        Setup {
            _dir: dir,
            runtime,
            host: Arc::new(Mutex::new(host)),
            root,
            worker,
        }
    }

    fn ask(s: &Setup, caller: Caller, request: AgentRequest) -> impl std::future::Future<Output = Result<AgentResponse, AgentError>> + use<> {
        let runtime = s.runtime.clone();
        let host = Arc::clone(&s.host);
        async move {
            tokio::task::spawn_blocking(move || runtime.handle(&caller, request, &mut *host.lock().unwrap()))
                .await
                .unwrap()
        }
    }

    /// An empty `next` must sleep until something changes, not wake itself
    /// and spin: over half a second it asks at most twice (the first empty
    /// answer marks the worker as waiting, a real change; after that the
    /// state is stable and nothing wakes it).
    #[tokio::test(flavor = "multi_thread")]
    async fn an_empty_next_sleeps_instead_of_spinning() {
        let s = setup();
        let task = s.worker.current_task_id.clone().unwrap();
        let caller = Caller {
            run_id: Some(s.worker.id.clone()),
            terminal_id: None,
        };
        let request = AgentRequest::Next {
            request_id: new_id(),
            after_task_id: Some(task),
            timeout_ms: Some(500),
        };
        let started = Instant::now();
        let (response, asked) =
            wait_loop(&s.runtime, Duration::from_millis(500), || ask(&s, caller.clone(), request.clone()))
                .await
                .unwrap();
        assert!(is_empty(&response));
        assert!(started.elapsed() >= Duration::from_millis(450), "it waited for the timeout");
        assert!(asked <= 3, "asked {asked} times: the waiter spun on its own notifications");
    }

    /// A real change (a message for the worker) ends the wait promptly.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_message_wakes_the_waiting_worker() {
        let s = setup();
        let task = s.worker.current_task_id.clone().unwrap();
        let worker = Caller {
            run_id: Some(s.worker.id.clone()),
            terminal_id: None,
        };
        let root = Caller {
            run_id: Some(s.root.id.clone()),
            terminal_id: None,
        };
        let request = AgentRequest::Next {
            request_id: new_id(),
            after_task_id: Some(task),
            timeout_ms: Some(5_000),
        };
        let sender = {
            let fut = ask(
                &s,
                root,
                AgentRequest::Message {
                    request_id: new_id(),
                    recipient_id: s.worker.id.clone(),
                    body: "answer".into(),
                },
            );
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(200)).await;
                fut.await
            })
        };
        let started = Instant::now();
        let (response, asked) =
            wait_loop(&s.runtime, Duration::from_millis(5_000), || ask(&s, worker.clone(), request.clone()))
                .await
                .unwrap();
        sender.await.unwrap().unwrap();
        let AgentResponse::Next(next) = response else { panic!() };
        assert_eq!(next.messages.len(), 1);
        assert!(started.elapsed() < Duration::from_secs(2), "woken by the change, not the timeout");
        assert!(asked <= 3, "asked {asked} times");
    }
}

pub async fn post_agents(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(envelope): Json<AgentEnvelope>,
) -> Response {
    // Local CLI only: loopback peer and the local CLI token, never a paired
    // remote token.
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .unwrap_or_default();
    if !peer.ip().is_loopback() || !state.auth_store.validate_local_cli_token(token) {
        return error_response(AgentError::new(
            AgentErrorCode::NotRegistered,
            "the agent API accepts only the local NotMux CLI on this machine",
        ));
    }
    let AgentEnvelope { caller, request } = envelope;
    if let Err(e) = request.validate() {
        return error_response(e);
    }
    let result = match &request {
        AgentRequest::Spawn { .. } | AgentRequest::Lead { .. } => launch(&state, caller, request).await,
        AgentRequest::Wait { timeout_ms, .. } | AgentRequest::Next { timeout_ms, .. } => {
            let timeout_ms = *timeout_ms;
            wait_for(&state, caller, request, timeout_ms).await
        }
        _ => handle_once(&state, caller, request).await,
    };
    respond(result)
}
