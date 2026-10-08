//! Durable, single-writer home of the orchestration [`Model`].
//!
//! Every mutation goes through one writer thread in arrival order. The
//! thread applies the mutation to a copy of the model, writes the copy
//! atomically (temp file, fsync, rename), and only then replaces the model
//! readers see. A failed write is reported to the caller as
//! `storage_failed` and leaves memory untouched, so nothing is ever
//! acknowledged that is not on disk.
//!
//! Reads take the current model under a short lock and never touch disk.

use super::migrate;
use super::model::{Model, STORE_VERSION};
use notmux_core::orchestration::{AgentError, AgentErrorCode};
use std::any::Any;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

pub const STORE_FILE: &str = "store.json";
/// The version-2 original is kept beside the migrated document.
pub const STORE_V2_BACKUP: &str = "store.v2.json";

type Boxed = Box<dyn Any + Send>;
type Mutation = Box<dyn FnOnce(&mut Model) -> Result<Boxed, AgentError> + Send>;

struct Job {
    mutation: Mutation,
    /// The value and whether the model changed.
    reply: Sender<Result<(Boxed, bool), AgentError>>,
}

struct Inner {
    path: PathBuf,
    model: Mutex<Model>,
    jobs: Mutex<Sender<Job>>,
    #[cfg(test)]
    fail_write_at: std::sync::atomic::AtomicU64,
    #[cfg(test)]
    write_counter: std::sync::atomic::AtomicU64,
}

#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").field("path", &self.inner.path).finish()
    }
}

/// What `open` found on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Opened {
    Fresh,
    Loaded,
    /// A version-2 document was migrated; the original lives at the path.
    Migrated(PathBuf),
}

impl Store {
    /// Open or create the store in `dir`, migrate older documents, and
    /// mark every run that was live before this process as lost. The
    /// result of that startup pass is written before `open` returns.
    pub fn open(dir: &Path, now: i64) -> Result<(Store, Opened), AgentError> {
        fs::create_dir_all(dir).map_err(|e| storage(format!("create {}: {e}", dir.display())))?;
        let path = dir.join(STORE_FILE);
        let (mut model, opened) = load(&path)?;
        let interrupted = model.interrupt_live_runs(now);
        if !interrupted.is_empty() {
            log::info!(
                "orchestration: {} run(s) were live before restart and are now lost",
                interrupted.len()
            );
        }
        write_document(&path, &model)?;
        let (tx, rx) = channel::<Job>();
        let inner = Arc::new(Inner {
            path,
            model: Mutex::new(model),
            jobs: Mutex::new(tx),
            #[cfg(test)]
            fail_write_at: std::sync::atomic::AtomicU64::new(0),
            #[cfg(test)]
            write_counter: std::sync::atomic::AtomicU64::new(0),
        });
        let worker = Arc::clone(&inner);
        std::thread::Builder::new()
            .name("orchestration-store".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    let result = worker.apply(job.mutation);
                    let _ = job.reply.send(result);
                }
            })
            .map_err(|e| storage(format!("spawn store thread: {e}")))?;
        Ok((Store { inner }, opened))
    }

    /// Open a store that lives only for tests, in a temporary directory.
    #[cfg(test)]
    pub fn open_temp() -> (Store, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = Store::open(dir.path(), 0).unwrap();
        (store, dir)
    }

    /// Run a read against the current model.
    pub fn read<T>(&self, f: impl FnOnce(&Model) -> T) -> T {
        let guard = self.inner.model.lock().unwrap_or_else(|p| p.into_inner());
        f(&guard)
    }

    /// Queue a mutation and block until it is durable. A mutation that
    /// returns `Err` is discarded without touching disk or memory.
    pub fn mutate<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Model) -> Result<T, AgentError> + Send + 'static,
    ) -> Result<T, AgentError> {
        self.mutate_tracked(f).map(|(value, _)| value)
    }

    /// Like `mutate`, also reporting whether the model changed (and was
    /// written). Callers use it to notify waiters only on real changes.
    pub fn mutate_tracked<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Model) -> Result<T, AgentError> + Send + 'static,
    ) -> Result<(T, bool), AgentError> {
        let (reply, response) = channel();
        let mutation: Mutation = Box::new(move |m| f(m).map(|v| Box::new(v) as Boxed));
        self.inner
            .jobs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .send(Job { mutation, reply })
            .map_err(|_| storage("store writer thread is gone"))?;
        let (boxed, changed) = response
            .recv()
            .map_err(|_| storage("store writer thread dropped the reply"))??;
        boxed
            .downcast::<T>()
            .map(|b| (*b, changed))
            .map_err(|_| storage("store reply had an unexpected type"))
    }
}

impl Store {
    /// Test-only failure injection: the `nth` document write from now on
    /// (1-based) fails with `storage_failed`. Zero disables it.
    #[cfg(test)]
    pub(crate) fn fail_write_at(&self, nth: u64) {
        use std::sync::atomic::Ordering;
        self.inner.write_counter.store(0, Ordering::SeqCst);
        self.inner.fail_write_at.store(nth, Ordering::SeqCst);
    }
}

impl Inner {
    fn apply(&self, mutation: Mutation) -> Result<(Boxed, bool), AgentError> {
        let current = self.model.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let mut next = current.clone();
        let value = mutation(&mut next)?;
        // A mutation that changed nothing (a poll that found nothing, a
        // repeated acknowledgement) costs no disk write and no UI stall.
        if next == current {
            return Ok((value, false));
        }
        #[cfg(test)]
        {
            use std::sync::atomic::Ordering;
            let n = self.write_counter.fetch_add(1, Ordering::SeqCst) + 1;
            if self.fail_write_at.load(Ordering::SeqCst) == n {
                self.fail_write_at.store(0, Ordering::SeqCst);
                return Err(storage("injected write failure"));
            }
        }
        write_document(&self.path, &next)?;
        *self.model.lock().unwrap_or_else(|p| p.into_inner()) = next;
        Ok((value, true))
    }
}

fn storage(message: impl Into<String>) -> AgentError {
    AgentError::new(AgentErrorCode::StorageFailed, message)
}

fn load(path: &Path) -> Result<(Model, Opened), AgentError> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Model::default(), Opened::Fresh));
        }
        Err(e) => return Err(storage(format!("read {}: {e}", path.display()))),
    };
    // A document that cannot be read is never replaced by an empty one:
    // orchestration stays unavailable until the user looks at the file.
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
        storage(format!(
            "{} is not valid JSON ({e}); orchestration is disabled until the file is repaired or removed",
            path.display()
        ))
    })?;
    match value.get("version").and_then(serde_json::Value::as_u64) {
        Some(v) if v == STORE_VERSION as u64 => serde_json::from_value::<Model>(value)
            .map(|model| (model, Opened::Loaded))
            .map_err(|e| {
                storage(format!(
                    "{} does not match store version {STORE_VERSION} ({e}); orchestration is disabled until the file is repaired or removed",
                    path.display()
                ))
            }),
        Some(2) => {
            let model = migrate::from_v2(&value).map_err(storage)?;
            let backup = path.with_file_name(STORE_V2_BACKUP);
            if !backup.exists() {
                fs::copy(path, &backup)
                    .map_err(|e| storage(format!("keep version-2 original at {}: {e}", backup.display())))?;
            }
            log::info!("orchestration: migrated version-2 store; original kept at {}", backup.display());
            Ok((model, Opened::Migrated(backup)))
        }
        Some(v) if v > STORE_VERSION as u64 => Err(storage(format!(
            "{} is version {v}, newer than this NotMux ({STORE_VERSION}); refusing to open it",
            path.display()
        ))),
        other => Err(storage(format!(
            "{} has unknown store version {other:?}; orchestration is disabled until the file is repaired or removed",
            path.display()
        ))),
    }
}

fn write_document(path: &Path, model: &Model) -> Result<(), AgentError> {
    let json = serde_json::to_string_pretty(model).map_err(|e| storage(format!("serialize store: {e}")))?;
    let tmp = path.with_extension("json.tmp");
    let write = || -> std::io::Result<()> {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(json.as_bytes())?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    };
    write().map_err(|e| {
        let _ = fs::remove_file(&tmp);
        storage(format!("write {}: {e}", path.display()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::model::LaunchSpec;
    use notmux_core::orchestration::*;

    fn spec() -> LaunchSpec {
        LaunchSpec {
            harness: HarnessKind::Claude,
            executable: "/bin/claude".into(),
            cwd: "/w".into(),
            delivery: DeliveryMode::Hook,
            completion: CompletionMode::Reported,
            files: RunFiles::default(),
            trust_writes: vec![],
        }
    }

    #[test]
    fn mutations_are_durable_and_ordered_under_concurrency() {
        let (store, dir) = Store::open_temp();
        let root = store
            .mutate(|m| {
                m.register_root(&crate::orchestration::new_id(), "t", "p", "/w", HarnessKind::Notagent, "", "", 4, false, 1)
            })
            .unwrap();
        let (worker, _) = store
            .mutate({
                let root = root.id.clone();
                move |m| {
                    m.accept_worker(
                        &root,
                        &crate::orchestration::new_id(),
                        crate::orchestration::model::WorkerIds::fresh(),
                        "a",
                        "t",
                        spec(),
                        2,
                    )
                }
            })
            .unwrap();
        store
            .mutate({
                let w = worker.id.clone();
                move |m| m.mark_started(&w, "tw", "s", 3)
            })
            .unwrap();
        let mut handles = vec![];
        for i in 0..8 {
            let store = store.clone();
            let w = worker.id.clone();
            let r = root.id.clone();
            handles.push(std::thread::spawn(move || {
                for j in 0..5 {
                    store
                        .mutate({
                            let (w, r) = (w.clone(), r.clone());
                            move |m| m.send_message(&w, &format!("{i}-{j}"), &r, "x", 10)
                        })
                        .unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let seqs = store.read(|m| {
            let mut s: Vec<u64> = m.messages.values().map(|x| x.sequence).collect();
            s.sort();
            s
        });
        assert_eq!(seqs, (1..=40).collect::<Vec<_>>());
        // Reopen: on-disk state matches, live runs are now lost.
        drop(store);
        let (again, opened) = Store::open(dir.path(), 99).unwrap();
        assert_eq!(opened, Opened::Loaded);
        again.read(|m| {
            assert_eq!(m.messages.len(), 40);
            assert_eq!(m.runs[&worker.id].process_state, ProcessState::Lost);
            assert_eq!(m.runs[&root.id].root_state, Some(RootState::Interrupted));
        });
    }

    #[test]
    fn failed_mutation_and_failed_write_leave_memory_untouched() {
        let (store, dir) = Store::open_temp();
        let before = store.read(|m| m.clone());
        let e = store
            .mutate(|m| {
                m.runs.insert("junk".into(), dummy_run());
                Err::<(), _>(AgentError::usage("no"))
            })
            .unwrap_err();
        assert_eq!(e.code, AgentErrorCode::Usage);
        assert_eq!(store.read(|m| m.clone()), before);

        // Make the write fail: the temp file path becomes a directory.
        let tmp = dir.path().join("store.json.tmp");
        fs::create_dir(&tmp).unwrap();
        let e = store
            .mutate(|m| {
                m.runs.insert("junk".into(), dummy_run());
                Ok(())
            })
            .unwrap_err();
        assert_eq!(e.code, AgentErrorCode::StorageFailed);
        assert_eq!(store.read(|m| m.clone()), before);
        fs::remove_dir(&tmp).unwrap();
        store
            .mutate(|m| {
                m.runs.insert("ok".into(), dummy_run());
                Ok(())
            })
            .unwrap();
        assert!(store.read(|m| m.runs.contains_key("ok")));
    }

    #[test]
    fn identical_retry_replays_and_conflict_is_refused() {
        let (store, _dir) = Store::open_temp();
        let req = AgentRequest::Ack {
            message_id: "m".into(),
        };
        let key = "run/ack/req-1";
        let digest = crate::orchestration::request_digest(&req);
        let resp = AgentResponse::Acknowledged {
            message_id: "m".into(),
        };
        store
            .mutate({
                let (resp, digest) = (resp.clone(), digest.clone());
                move |m| {
                    m.record_request(key, &digest, resp, 1);
                    Ok(())
                }
            })
            .unwrap();
        assert_eq!(store.read(|m| m.lookup_request(key, &digest)).unwrap(), Some(resp));
        let other = crate::orchestration::request_digest(&AgentRequest::Ack {
            message_id: "n".into(),
        });
        assert_eq!(
            store.read(|m| m.lookup_request(key, &other)).unwrap_err().code,
            AgentErrorCode::RequestConflict
        );
    }

    #[test]
    fn corrupt_newer_and_v2_documents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STORE_FILE);
        fs::write(&path, "{ not json").unwrap();
        // A corrupt document disables orchestration instead of being
        // replaced by an empty one; the file is left exactly as it was.
        let e = Store::open(dir.path(), 1).unwrap_err();
        assert_eq!(e.code, AgentErrorCode::StorageFailed);
        assert!(e.message.contains("not valid JSON"), "{}", e.message);
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ not json");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1, "nothing moved aside");
        fs::write(&path, r#"{"version": 3, "runs": "not a map"}"#).unwrap();
        let e = Store::open(dir.path(), 1).unwrap_err();
        assert!(e.message.contains("does not match store version"), "{}", e.message);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STORE_FILE);
        fs::write(&path, r#"{"version": 9}"#).unwrap();
        let e = Store::open(dir.path(), 1).unwrap_err();
        assert_eq!(e.code, AgentErrorCode::StorageFailed);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STORE_FILE);
        let v2 = serde_json::json!({
            "version": 2,
            "runs": {"r": {"id": "r", "role": {"role": "root", "total_start_budget": 1, "consumed_starts": 0},
                     "project_id": "p", "terminal_id": "t", "cwd": "/", "completion_mode": "reported",
                     "handoff": "cooperative", "task_state": "active", "process_state": "adopted",
                     "created_at_ms": 5}},
            "messages": {}, "assignments": {}, "events": {}, "requests": {}
        });
        fs::write(&path, serde_json::to_string(&v2).unwrap()).unwrap();
        let (s, opened) = Store::open(dir.path(), 1).unwrap();
        let Opened::Migrated(backup) = opened else {
            panic!("{opened:?}")
        };
        assert!(backup.exists());
        s.read(|m| {
            assert_eq!(m.version, STORE_VERSION);
            assert_eq!(m.runs["r"].root_state, Some(RootState::Interrupted));
        });
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"version\": 3"));
    }

    #[test]
    fn unchanged_mutations_do_not_touch_the_disk() {
        let (store, dir) = Store::open_temp();
        store
            .mutate(|m| m.register_root(&crate::orchestration::new_id(), "t", "p", "/w", HarnessKind::Notagent, "", "", 4, false, 1))
            .unwrap();
        let path = dir.path().join(STORE_FILE);
        let before = fs::read(&path).unwrap();
        let before_meta = fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        // A read-like mutation: nothing changes.
        let n = store.mutate(|m| Ok(m.runs.len())).unwrap();
        assert_eq!(n, 1);
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), before_meta);
        // A real change is written.
        store
            .mutate(|m| {
                m.runs.insert("x".into(), dummy_run());
                Ok(())
            })
            .unwrap();
        assert_ne!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn operation_and_request_record_are_one_write() {
        let (store, _dir) = Store::open_temp();
        let root = store
            .mutate(|m| m.register_root(&crate::orchestration::new_id(), "t", "p", "/w", HarnessKind::Notagent, "", "", 4, false, 1))
            .unwrap();
        let (worker, _) = store
            .mutate({
                let root = root.id.clone();
                move |m| {
                    m.accept_worker(&root, &crate::orchestration::new_id(), crate::orchestration::model::WorkerIds::fresh(), "a", "t", spec(), 2)
                }
            })
            .unwrap();
        store
            .mutate({
                let w = worker.id.clone();
                move |m| m.mark_started(&w, "tw", "s", 3)
            })
            .unwrap();
        // The next document write fails: neither the message nor its
        // request entry may survive, so a retry does not duplicate.
        store.fail_write_at(1);
        let key = "w/message/req-1";
        let e = store
            .mutate({
                let (w, r) = (worker.id.clone(), root.id.clone());
                move |m| {
                    let msg = m.send_message(&w, "req-1", &r, "hello", 4)?;
                    m.record_request(key, "digest", AgentResponse::Message(msg), 4);
                    Ok(())
                }
            })
            .unwrap_err();
        assert_eq!(e.code, AgentErrorCode::StorageFailed);
        store.read(|m| {
            assert!(m.messages.is_empty());
            assert!(m.lookup_request(key, "digest").unwrap().is_none());
        });
        // The retry succeeds once and leaves both behind together.
        store
            .mutate({
                let (w, r) = (worker.id.clone(), root.id.clone());
                move |m| {
                    let msg = m.send_message(&w, "req-1", &r, "hello", 5)?;
                    m.record_request(key, "digest", AgentResponse::Message(msg), 5);
                    Ok(())
                }
            })
            .unwrap();
        store.read(|m| {
            assert_eq!(m.messages.len(), 1);
            assert!(m.lookup_request(key, "digest").unwrap().is_some());
        });
    }

    fn dummy_run() -> RunRecord {
        RunRecord {
            id: "x".into(),
            role: RunRole::Root,
            harness: HarnessKind::Generic,
            project_id: String::new(),
            terminal_id: String::new(),
            slot_id: String::new(),
            name: String::new(),
            cwd: String::new(),
            executable: String::new(),
            delivery: DeliveryMode::Cooperative,
            completion: CompletionMode::ExitCode,
            created_at_ms: 0,
            process_state: ProcessState::Exited,
            exit: None,
            worker_state: WorkerAvailability::Stopped,
            root_state: Some(RootState::Completed),
            budget: None,
            result: None,
            current_task_id: None,
            launch_error: None,
            trust_writes: vec![],
            files: RunFiles::default(),
            cleanup_error: None,
        }
    }
}
