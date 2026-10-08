//! Lift the version-2 document of the earlier prototype into the current
//! model. The mapping is lossy where version 2 recorded less (no harness,
//! no document paths, no request IDs on records); unknown enum spellings
//! fall back to their terminal or unknown counterpart. Version-2 request
//! entries are dropped: their response shape no longer exists, and every
//! group they belong to is interrupted on the first open anyway.

use super::model::{Model, STORE_VERSION};
use notmux_core::orchestration::*;
use serde_json::Value;
use std::collections::BTreeMap;

fn s(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn i(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn harness_from_executable(exe: &str) -> HarnessKind {
    let base = std::path::Path::new(exe)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(exe);
    HarnessKind::parse(base).unwrap_or(HarnessKind::Generic)
}

fn result_from(v: Option<&Value>, state: &str) -> Option<TaskResult> {
    let v = v?;
    if v.is_null() {
        return None;
    }
    Some(TaskResult {
        outcome: if state == "succeeded" {
            Outcome::Succeeded
        } else {
            Outcome::Failed
        },
        source: match s(v, "source").as_str() {
            "reported" => ResultSource::Reported,
            "exit_code" => ResultSource::ExitCode,
            _ => ResultSource::Runtime,
        },
        body: s(v, "body"),
        request_id: None,
        reported_at_ms: i(v, "committed_at_ms"),
    })
}

fn assignment_state(state: &str) -> AssignmentState {
    match state {
        "queued" => AssignmentState::Queued,
        "running" => AssignmentState::Running,
        "succeeded" => AssignmentState::Succeeded,
        "failed" => AssignmentState::Failed,
        "cancelled" => AssignmentState::Cancelled,
        _ => AssignmentState::Interrupted,
    }
}

fn root_state(task_state: &str) -> RootState {
    match task_state {
        "succeeded" | "failed" | "completed" => RootState::Completed,
        "cancelled" => RootState::Cancelled,
        "interrupted" => RootState::Interrupted,
        _ => RootState::Active,
    }
}

fn exit_from(v: Option<&Value>) -> Option<ProcessExit> {
    let v = v?;
    if v.is_null() {
        return None;
    }
    Some(ProcessExit {
        code: v.get("code").and_then(Value::as_i64).map(|c| c as i32),
        signal: v.get("signal").and_then(Value::as_str).map(str::to_string),
    })
}

fn run_from(v: &Value) -> Option<RunRecord> {
    let id = s(v, "id");
    if id.is_empty() {
        return None;
    }
    let role_v = v.get("role")?;
    let (role, budget) = match s(role_v, "role").as_str() {
        "root" => {
            let total = role_v
                .get("total_start_budget")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32;
            let consumed = role_v
                .get("consumed_starts")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32;
            (
                RunRole::Root,
                Some(RootBudget {
                    total_starts: total,
                    consumed_starts: consumed,
                    remaining_starts: total.saturating_sub(consumed),
                    live_workers: 0,
                    max_live_workers: MAX_WORKERS_PER_ROOT as u32,
                }),
            )
        }
        "worker" => (
            RunRole::Worker {
                root_id: s(role_v, "root_id"),
            },
            None,
        ),
        _ => return None,
    };
    let is_root = role.is_root();
    let executable = s(v, "executable");
    let task_state = s(v, "task_state");
    let process_state = match s(v, "process_state").as_str() {
        "exited" => ProcessState::Exited,
        "adopted" | "running" | "stopping" => ProcessState::Running,
        "starting" => ProcessState::Starting,
        _ => ProcessState::Lost,
    };
    let worker_state = match s(v, "worker_state").as_str() {
        "stopped" | "stopping" => WorkerAvailability::Stopped,
        "busy" => WorkerAvailability::Busy,
        "idle" => WorkerAvailability::Idle,
        "waiting" => WorkerAvailability::Waiting,
        _ if is_root => WorkerAvailability::Idle,
        _ => WorkerAvailability::Starting,
    };
    Some(RunRecord {
        id,
        role,
        harness: harness_from_executable(&executable),
        project_id: s(v, "project_id"),
        terminal_id: s(v, "terminal_id"),
        slot_id: s(v, "slot_id"),
        name: s(v, "name"),
        cwd: s(v, "cwd"),
        executable,
        delivery: if s(v, "handoff") == "cooperative" {
            DeliveryMode::Cooperative
        } else {
            DeliveryMode::Hook
        },
        completion: if s(v, "completion_mode") == "reported" {
            CompletionMode::Reported
        } else {
            CompletionMode::ExitCode
        },
        created_at_ms: i(v, "created_at_ms"),
        process_state,
        exit: exit_from(v.get("exit")),
        worker_state,
        root_state: is_root.then(|| root_state(&task_state)),
        budget,
        result: if is_root {
            result_from(v.get("result"), &task_state)
        } else {
            None
        },
        current_task_id: v
            .get("current_task_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        launch_error: None,
        trust_writes: vec![],
        files: RunFiles::default(),
        cleanup_error: v
            .get("cleanup_error")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

fn event_kind(run_id: &str, root_id: &str, e: &Value) -> Option<EventKind> {
    let worker_id = run_id.to_string();
    Some(match s(e, "kind").as_str() {
        "registered" => EventKind::RootRegistered,
        "accepted" => EventKind::WorkerAccepted { worker_id },
        "started" => EventKind::WorkerStarted { worker_id },
        "process_exited" => EventKind::WorkerExited {
            worker_id,
            exit: exit_from(e.get("exit")).unwrap_or_default(),
        },
        "assignment_result" => EventKind::AssignmentFinished {
            worker_id,
            task_id: s(e, "task_id"),
            outcome: if s(e, "state") == "succeeded" {
                Outcome::Succeeded
            } else {
                Outcome::Failed
            },
        },
        "result" if run_id == root_id => EventKind::RootFinished {
            state: root_state(&s(e, "state")),
        },
        "message" => EventKind::MessageSent {
            message_id: s(e, "message_id"),
            sender_id: worker_id,
            recipient_id: s(e, "recipient_id"),
        },
        "acknowledged" => EventKind::MessageAcknowledged {
            message_id: s(e, "message_id"),
        },
        "cleanup_failed" => EventKind::CleanupFailed {
            worker_id,
            error: s(e, "error"),
        },
        _ => return None,
    })
}

/// Convert a parsed version-2 document. Returns `Err` only when the
/// document is not a version-2 store at all.
pub fn from_v2(doc: &Value) -> Result<Model, String> {
    if doc.get("version").and_then(Value::as_u64) != Some(2) {
        return Err("not a version-2 store".into());
    }
    let mut model = Model {
        version: STORE_VERSION,
        ..Model::default()
    };
    if let Some(runs) = doc.get("runs").and_then(Value::as_object) {
        for v in runs.values() {
            if let Some(run) = run_from(v) {
                model.runs.insert(run.id.clone(), run);
            }
        }
    }
    let root_of: BTreeMap<String, String> = model
        .runs
        .values()
        .map(|r| (r.id.clone(), model.root_id_of(r)))
        .collect();
    if let Some(assignments) = doc.get("assignments").and_then(Value::as_object) {
        for v in assignments.values() {
            let id = s(v, "id");
            let worker_id = s(v, "worker_id");
            let Some(root_id) = root_of.get(&worker_id) else {
                log::warn!("orchestration migration: dropping assignment {id} of unknown worker {worker_id}");
                continue;
            };
            let state = s(v, "state");
            model.assignments.insert(
                id.clone(),
                AssignmentRecord {
                    id: id.clone(),
                    worker_id,
                    root_id: root_id.clone(),
                    sequence: v.get("sequence").and_then(Value::as_u64).unwrap_or(1),
                    name: s(v, "name"),
                    state: assignment_state(&state),
                    body: s(v, "body"),
                    document_path: String::new(),
                    created_at_ms: i(v, "created_at_ms"),
                    started_at_ms: None,
                    result: result_from(v.get("result"), &state),
                    request_id: id,
                },
            );
        }
    }
    if let Some(messages) = doc.get("messages").and_then(Value::as_object) {
        for v in messages.values() {
            let id = s(v, "id");
            let recipient_id = s(v, "recipient_id");
            let sequence = v.get("sequence").and_then(Value::as_u64).unwrap_or(0);
            if let Some(root_id) = root_of.get(&recipient_id) {
                let counter = model.message_sequences.entry(root_id.clone()).or_insert(0);
                *counter = (*counter).max(sequence);
            }
            model.messages.insert(
                id.clone(),
                MessageRecord {
                    id: id.clone(),
                    sequence,
                    sender_id: s(v, "sender_id"),
                    recipient_id,
                    body: s(v, "body"),
                    created_at_ms: i(v, "created_at_ms"),
                    acknowledged: v
                        .get("acknowledged")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    request_id: id,
                },
            );
        }
    }
    if let Some(events) = doc.get("events").and_then(Value::as_object) {
        for (root_id, list) in events {
            let Some(list) = list.as_array() else { continue };
            let mut stream: Vec<EventRecord> = vec![];
            for v in list {
                let run_id = s(v, "run_id");
                let Some(kind) = event_kind(&run_id, root_id, v.get("event").unwrap_or(&Value::Null))
                else {
                    continue;
                };
                stream.push(EventRecord {
                    sequence: v.get("sequence").and_then(Value::as_u64).unwrap_or(0),
                    root_id: root_id.clone(),
                    at_ms: i(v, "created_at_ms"),
                    kind,
                });
            }
            stream.sort_by_key(|e| e.sequence);
            // Dropped kinds leave holes; renumber so paging stays gapless.
            for (n, e) in stream.iter_mut().enumerate() {
                e.sequence = n as u64 + 1;
            }
            model.events.insert(root_id.clone(), stream);
        }
    }
    Ok(model)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        serde_json::json!({
            "version": 2,
            "runs": {
                "r1": {"id": "r1", "role": {"role": "root", "total_start_budget": 2, "consumed_starts": 2},
                       "project_id": "p", "terminal_id": "t-root", "slot_id": null, "name": "Orchestrator",
                       "executable": null, "cwd": "/w", "completion_mode": "reported", "handoff": "cooperative",
                       "task_state": "cancelled", "process_state": "adopted", "worker_state": null,
                       "current_task_id": null, "exit": null, "cleanup_error": null, "created_at_ms": 10},
                "w1": {"id": "w1", "role": {"role": "worker", "root_id": "r1"}, "project_id": "p",
                       "terminal_id": "t-w1", "slot_id": "s1", "name": "Worker A",
                       "executable": "/opt/homebrew/bin/notagent", "cwd": "/w/a", "completion_mode": "reported",
                       "handoff": "claude_stop", "task_state": "succeeded", "process_state": "exited",
                       "worker_state": "stopped", "current_task_id": "a1",
                       "exit": {"code": 0, "signal": null},
                       "result": {"source": "reported", "body": "done", "reporter_id": "w1", "committed_at_ms": 30},
                       "cleanup_error": null, "created_at_ms": 20},
                "w2": {"id": "w2", "role": {"role": "worker", "root_id": "r1"}, "project_id": "p",
                       "terminal_id": "t-w2", "slot_id": "s2", "name": "Worker B", "executable": "claude",
                       "cwd": "/w/b", "completion_mode": "reported", "handoff": "cooperative",
                       "task_state": "interrupted", "process_state": "stopping", "worker_state": "stopping",
                       "current_task_id": null, "exit": null, "cleanup_error": "kill failed", "created_at_ms": 21}
            },
            "assignments": {
                "a1": {"id": "a1", "worker_id": "w1", "sequence": 1, "document_id": "w1", "name": "slug",
                       "body": "do slug", "state": "succeeded", "created_at_ms": 20,
                       "result": {"source": "reported", "body": "done", "reporter_id": "w1", "committed_at_ms": 30}},
                "a9": {"id": "a9", "worker_id": "ghost", "sequence": 1, "document_id": "x", "name": "n",
                       "body": "b", "state": "failed", "created_at_ms": 1, "result": null}
            },
            "messages": {
                "m1": {"id": "m1", "sender_id": "w1", "recipient_id": "r1", "body": "hi", "created_at_ms": 25,
                       "sequence": 6, "acknowledged": true}
            },
            "events": {
                "r1": [
                    {"root_id": "r1", "run_id": "r1", "sequence": 1, "created_at_ms": 10, "event": {"kind": "registered"}},
                    {"root_id": "r1", "run_id": "w1", "sequence": 2, "created_at_ms": 20, "event": {"kind": "accepted"}},
                    {"root_id": "r1", "run_id": "w1", "sequence": 3, "created_at_ms": 21, "event": {"kind": "weird"}},
                    {"root_id": "r1", "run_id": "w1", "sequence": 4, "created_at_ms": 30,
                     "event": {"kind": "assignment_result", "task_id": "a1", "state": "succeeded"}},
                    {"root_id": "r1", "run_id": "r1", "sequence": 5, "created_at_ms": 40, "event": {"kind": "result", "state": "cancelled"}}
                ]
            },
            "requests": {"w1/finish/x": {"digest": "d", "response": {"type": "run", "view": {}}}}
        })
    }

    #[test]
    fn migrates_runs_assignments_messages_and_events() {
        let m = from_v2(&sample()).unwrap();
        assert_eq!(m.version, STORE_VERSION);
        assert_eq!(m.runs.len(), 3);
        let root = &m.runs["r1"];
        assert_eq!(root.role, RunRole::Root);
        assert_eq!(root.root_state, Some(RootState::Cancelled));
        assert_eq!(root.budget.as_ref().unwrap().consumed_starts, 2);
        assert_eq!(root.process_state, ProcessState::Running);
        let w1 = &m.runs["w1"];
        assert_eq!(w1.harness, HarnessKind::Notagent);
        assert_eq!(w1.delivery, DeliveryMode::Hook);
        assert_eq!(w1.process_state, ProcessState::Exited);
        assert_eq!(w1.exit.as_ref().unwrap().code, Some(0));
        let w2 = &m.runs["w2"];
        assert_eq!(w2.harness, HarnessKind::Claude);
        assert_eq!(w2.worker_state, WorkerAvailability::Stopped);
        assert_eq!(w2.cleanup_error.as_deref(), Some("kill failed"));
        assert_eq!(m.assignments.len(), 1, "orphan assignment dropped");
        let a1 = &m.assignments["a1"];
        assert_eq!(a1.root_id, "r1");
        assert_eq!(a1.state, AssignmentState::Succeeded);
        assert_eq!(a1.result.as_ref().unwrap().outcome, Outcome::Succeeded);
        assert_eq!(a1.body, "do slug");
        assert_eq!(m.messages["m1"].sequence, 6);
        assert_eq!(m.message_sequences["r1"], 6);
        let events = &m.events["r1"];
        assert_eq!(events.len(), 4, "unknown kind dropped");
        assert_eq!(
            events.iter().map(|e| e.sequence).collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        assert!(matches!(events[3].kind, EventKind::RootFinished { state: RootState::Cancelled }));
        assert!(m.requests.is_empty());
    }

    #[test]
    fn refuses_other_versions() {
        assert!(from_v2(&serde_json::json!({"version": 3})).is_err());
        assert!(from_v2(&serde_json::json!({})).is_err());
    }

    #[test]
    fn migrated_model_survives_restart_interruption() {
        let mut m = from_v2(&sample()).unwrap();
        let affected = m.interrupt_live_runs(100);
        assert_eq!(affected.len(), 2, "adopted root and stopping worker");
        assert_eq!(m.runs["w2"].process_state, ProcessState::Lost);
        assert_eq!(m.runs["w2"].cleanup_error, None);
        assert_eq!(m.live_workers_total(), 0);
    }
}
