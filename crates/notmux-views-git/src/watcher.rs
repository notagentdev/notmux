use gpui::prelude::*;
use gpui::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use notmux_core::api::ApiGitStatus;
use notmux_git::{self as git, GitStatus};
use notmux_workspace::state::Workspace;

/// Filesystem change event emitted after a `notify` debouncer batch.
/// Listeners (file explorer, git header) use this for live updates.
#[derive(Clone, Debug)]
pub struct FsChangeEvent {
    pub project_id: String,
    pub files: Vec<PathBuf>,
    /// True when any path is inside `.git/` — consumers should treat this
    /// as a signal to do a debounced *full* refresh (branch switch, rebase).
    pub is_git_internal: bool,
}

impl EventEmitter<FsChangeEvent> for GitStatusWatcher {}

/// Git status poll interval (seconds) — fallback for cases where the FS
/// watcher isn't available (e.g. remote projects) or to cover edge cases.
const STATUS_POLL_INTERVAL: u64 = 30;

/// How many status poll cycles between PR URL checks (~60s).
const PR_POLL_EVERY_N_CYCLES: u64 = 2;
/// How many status poll cycles between CI checks when pending (~30s).
const CI_PENDING_POLL_EVERY_N_CYCLES: u64 = 1;
/// How many status poll cycles between CI checks when settled (~60s).
const CI_SETTLED_POLL_EVERY_N_CYCLES: u64 = 2;

/// FS-watcher debounce window (ms). Matches NotMux.
const FS_DEBOUNCE_MS: u64 = 300;

/// Centralized git status coordinator.
///
/// - Filesystem changes for **local** projects are driven by per-project
///   `notify` debouncers (300 ms) that batch events and forward them to
///   the GPUI thread. Each batch triggers an immediate git status refresh
///   plus a `FsChangeEvent` so UI consumers can patch incrementally.
/// - A slower polling loop continues to run for PR/CI status (network) and
///   as a fallback git-status refresh for remote-subscribed projects.
/// - Pushes changes to:
///   - Local UI via `cx.notify()` (ProjectColumn observes this entity).
///   - Remote clients via `tokio::sync::watch` channel (WS stream handler).
pub struct GitStatusWatcher {
    workspace: Entity<Workspace>,
    statuses: HashMap<String, Option<GitStatus>>,
    pr_infos: HashMap<String, Option<notmux_git::PrInfo>>,
    ci_checks: HashMap<String, Option<notmux_git::CiCheckSummary>>,
    any_pending_ci: bool,
    remote_tx: Arc<tokio::sync::watch::Sender<HashMap<String, ApiGitStatus>>>,
    remote_subscribed_terminals: Arc<RwLock<HashMap<u64, HashSet<String>>>>,

    /// Active FS debouncers keyed by project_id. Dropping removes the watch.
    fs_watchers: HashMap<String, FsWatcherHandle>,
    /// Channel used by the watcher callbacks to push batches into GPUI.
    fs_tx: async_channel::Sender<FsBatch>,
    fs_rx: Option<async_channel::Receiver<FsBatch>>,
}

/// Opaque handle owning the notify debouncer for a single project.
struct FsWatcherHandle {
    _debouncer: Box<dyn std::any::Any + Send>,
    _root: PathBuf,
}

struct FsBatch {
    project_id: String,
    files: Vec<PathBuf>,
    is_git_internal: bool,
}

impl GitStatusWatcher {
    pub fn new(
        workspace: Entity<Workspace>,
        remote_tx: Arc<tokio::sync::watch::Sender<HashMap<String, ApiGitStatus>>>,
        remote_subscribed_terminals: Arc<RwLock<HashMap<u64, HashSet<String>>>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let (fs_tx, fs_rx) = async_channel::unbounded();
        let mut watcher = Self {
            workspace,
            statuses: HashMap::new(),
            pr_infos: HashMap::new(),
            ci_checks: HashMap::new(),
            any_pending_ci: false,
            remote_tx,
            remote_subscribed_terminals,
            fs_watchers: HashMap::new(),
            fs_tx,
            fs_rx: Some(fs_rx),
        };
        watcher.spawn_fs_event_pump(cx);
        watcher.sync_fs_watchers(cx);
        watcher.spawn_status_poll(cx);
        // Observe workspace to add/remove FS watchers when projects change.
        let weak = cx.weak_entity();
        cx.observe(&watcher.workspace, move |this, _ws, cx| {
            this.sync_fs_watchers(cx);
            let _ = weak.clone();
        })
        .detach();
        watcher
    }

    /// Get cached git status for a project.
    pub fn get(&self, project_id: &str) -> Option<&GitStatus> {
        self.statuses.get(project_id).and_then(|s| s.as_ref())
    }

    /// Add/remove notify watchers so we track exactly the currently-visible
    /// non-remote projects.
    fn sync_fs_watchers(&mut self, _cx: &mut Context<Self>) {
        let wanted: HashMap<String, PathBuf> = {
            let ws = self.workspace.read(_cx);
            ws.projects()
                .iter()
                .filter(|p| !p.is_remote)
                .map(|p| (p.id.clone(), PathBuf::from(&p.path)))
                .collect()
        };

        // Remove watchers for projects no longer tracked.
        self.fs_watchers.retain(|id, _| wanted.contains_key(id));

        // Add watchers for new projects.
        for (id, path) in wanted {
            if self.fs_watchers.contains_key(&id) {
                continue;
            }
            if let Some(handle) = spawn_fs_watcher(id.clone(), path, self.fs_tx.clone()) {
                self.fs_watchers.insert(id, handle);
            }
        }
    }

    /// Pump FS batches from the notify thread into GPUI updates.
    fn spawn_fs_event_pump(&mut self, cx: &mut Context<Self>) {
        let Some(rx) = self.fs_rx.take() else {
            return;
        };
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            while let Ok(batch) = rx.recv().await {
                let ok = this
                    .update(cx, |this, cx| {
                        this.handle_fs_batch(batch, cx);
                    })
                    .is_ok();
                if !ok {
                    break;
                }
            }
        })
        .detach();
    }

    fn handle_fs_batch(&mut self, batch: FsBatch, cx: &mut Context<Self>) {
        let project_id = batch.project_id.clone();
        let is_git_internal = batch.is_git_internal;
        let path = self
            .workspace
            .read(cx)
            .project(&project_id)
            .map(|p| p.path.clone());
        cx.emit(FsChangeEvent {
            project_id: project_id.clone(),
            files: batch.files,
            is_git_internal,
        });
        // Kick a git status refresh for this project right away so the
        // summary (branch/lines/ahead/behind) stays fresh too.
        if let Some(path) = path {
            let id = project_id.clone();
            cx.spawn(async move |this: WeakEntity<Self>, cx| {
                let status = smol::unblock(move || git::refresh_git_status(Path::new(&path))).await;
                let _ = this.update(cx, |this, cx| {
                    this.apply_status_update(id, status, cx);
                });
            })
            .detach();
        }
    }

    fn apply_status_update(
        &mut self,
        project_id: String,
        mut status: Option<GitStatus>,
        cx: &mut Context<Self>,
    ) {
        if let Some(status) = status.as_mut()
            && let Some(mut pr) = self.pr_infos.get(&project_id).cloned().flatten()
        {
            pr.ci_checks = self.ci_checks.get(&project_id).cloned().flatten();
            status.pr_info = Some(pr);
        }
        let changed = self.statuses.get(&project_id).cloned().unwrap_or(None) != status;
        self.statuses.insert(project_id, status);
        if changed {
            cx.notify();
            self.push_remote_snapshot();
        }
    }

    fn push_remote_snapshot(&self) {
        let api_statuses: HashMap<String, ApiGitStatus> = self
            .statuses
            .iter()
            .filter_map(|(id, status)| {
                status.as_ref().map(|s| {
                    (
                        id.clone(),
                        ApiGitStatus {
                            branch: s.branch.clone(),
                            lines_added: s.lines_added,
                            lines_removed: s.lines_removed,
                        },
                    )
                })
            })
            .collect();
        self.remote_tx.send_modify(|current| {
            *current = api_statuses;
        });
    }

    /// Spawn a slower polling loop for PR/CI info (network) + fallback
    /// status refresh for remote-subscribed local projects.
    fn spawn_status_poll(&mut self, cx: &mut Context<Self>) {
        let workspace = self.workspace.clone();
        let remote_subscribed_terminals = self.remote_subscribed_terminals.clone();

        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let mut cycle: u64 = 0;
            loop {
                let projects: Vec<(String, String)> = cx.update(|cx| {
                    let ws = workspace.read(cx);
                    let mut project_ids: HashSet<String> = ws
                        .visible_projects()
                        .iter()
                        .filter(|p| !p.is_remote)
                        .map(|p| p.id.clone())
                        .collect();
                    if let Ok(remote_terminals) = remote_subscribed_terminals.read() {
                        for terminal_ids in remote_terminals.values() {
                            for tid in terminal_ids {
                                if let Some(p) = ws.find_project_for_terminal(tid)
                                    && !p.is_remote
                                {
                                    project_ids.insert(p.id.clone());
                                }
                            }
                        }
                    }
                    ws.projects()
                        .iter()
                        .filter(|p| project_ids.contains(&p.id))
                        .map(|p| (p.id.clone(), p.path.clone()))
                        .collect()
                });

                let check_prs = cycle.is_multiple_of(PR_POLL_EVERY_N_CYCLES);
                let ci_poll_interval = if this
                    .update(cx, |this, _| this.any_pending_ci)
                    .unwrap_or(false)
                {
                    CI_PENDING_POLL_EVERY_N_CYCLES
                } else {
                    CI_SETTLED_POLL_EVERY_N_CYCLES
                };
                let check_ci = cycle.is_multiple_of(ci_poll_interval);

                // Status refresh (covers remote-subscribed + fallback path).
                let status_futures: Vec<_> = projects
                    .iter()
                    .map(|(id, path)| {
                        let id = id.clone();
                        let path = path.clone();
                        async move {
                            let status =
                                smol::unblock(move || git::refresh_git_status(Path::new(&path)))
                                    .await;
                            (id, status)
                        }
                    })
                    .collect();
                let mut new_statuses: HashMap<String, Option<GitStatus>> =
                    futures::future::join_all(status_futures)
                        .await
                        .into_iter()
                        .collect();

                let new_pr_infos: HashMap<String, Option<notmux_git::PrInfo>> = if check_prs {
                    let pr_futures: Vec<_> = projects
                        .iter()
                        .map(|(id, path)| {
                            let id = id.clone();
                            let path = path.clone();
                            async move {
                                let pr = smol::unblock(move || {
                                    git::repository::get_pr_info(Path::new(&path))
                                })
                                .await;
                                (id, pr)
                            }
                        })
                        .collect();
                    futures::future::join_all(pr_futures)
                        .await
                        .into_iter()
                        .collect()
                } else {
                    HashMap::new()
                };

                let new_ci_checks: HashMap<String, Option<notmux_git::CiCheckSummary>> = if check_ci {
                    let pr_infos_snapshot: HashMap<String, Option<notmux_git::PrInfo>> = if check_prs
                    {
                        new_pr_infos.clone()
                    } else {
                        this.update(cx, |this, _| this.pr_infos.clone())
                            .unwrap_or_default()
                    };
                    let ci_futures: Vec<_> = projects
                        .iter()
                        .filter(|&(id, _): &&(String, String)| {
                            pr_infos_snapshot
                                .get(id)
                                .map(|p| p.is_some())
                                .unwrap_or(false)
                        })
                        .map(|(id, path): &(String, String)| {
                            let id = id.clone();
                            let path = path.clone();
                            async move {
                                let checks = smol::unblock(move || {
                                    git::repository::get_ci_checks(Path::new(&path))
                                })
                                .await;
                                (id, checks)
                            }
                        })
                        .collect();
                    let results: Vec<(String, Option<notmux_git::CiCheckSummary>)> =
                        futures::future::join_all(ci_futures).await;
                    results.into_iter().collect()
                } else {
                    HashMap::new()
                };

                let should_continue = this
                    .update(cx, |this, cx| {
                        if check_prs {
                            this.pr_infos = new_pr_infos;
                        }
                        if check_ci {
                            for (id, checks) in new_ci_checks {
                                this.ci_checks.insert(id, checks);
                            }
                            this.any_pending_ci = this.ci_checks.values().any(|c| {
                                c.as_ref().map(|s| s.status.is_pending()).unwrap_or(false)
                            });
                        }
                        for (id, status) in new_statuses.iter_mut() {
                            if let Some(Some(status)) = status.as_mut().map(Some)
                                && let Some(mut pr) = this.pr_infos.get(id).cloned().flatten()
                            {
                                pr.ci_checks = this.ci_checks.get(id).cloned().flatten();
                                status.pr_info = Some(pr);
                            }
                        }
                        let changed = this.statuses != new_statuses;
                        if changed {
                            this.statuses = new_statuses;
                            cx.notify();
                            this.push_remote_snapshot();
                        }
                        true
                    })
                    .unwrap_or(false);

                if !should_continue {
                    break;
                }

                cycle += 1;
                smol::Timer::after(Duration::from_secs(STATUS_POLL_INTERVAL)).await;
            }
        })
        .detach();
    }
}

/// Spawn a `notify-debouncer-mini` watcher for a single project root.
/// Returns a handle that keeps the debouncer alive until dropped.
fn spawn_fs_watcher(
    project_id: String,
    root: PathBuf,
    tx: async_channel::Sender<FsBatch>,
) -> Option<FsWatcherHandle> {
    use notify::RecursiveMode;
    use notify_debouncer_mini::{DebounceEventResult, new_debouncer};

    let root_clone = root.clone();
    let mut debouncer = match new_debouncer(
        Duration::from_millis(FS_DEBOUNCE_MS),
        move |res: DebounceEventResult| {
            let Ok(events) = res else { return };
            let mut files: Vec<PathBuf> = Vec::new();
            let mut is_git_internal = false;
            for e in events {
                let p = e.path;
                if contains_git_internal(&p) {
                    is_git_internal = true;
                    continue;
                }
                files.push(p);
            }
            if files.is_empty() && !is_git_internal {
                return;
            }
            let _ = tx.try_send(FsBatch {
                project_id: project_id.clone(),
                files,
                is_git_internal,
            });
        },
    ) {
        Ok(d) => d,
        Err(e) => {
            log::warn!(
                "fs-watcher: failed to create debouncer for {}: {}",
                root.display(),
                e
            );
            return None;
        }
    };

    if let Err(e) = debouncer
        .watcher()
        .watch(&root_clone, RecursiveMode::Recursive)
    {
        log::warn!(
            "fs-watcher: watch failed for {}: {}",
            root_clone.display(),
            e
        );
        return None;
    }

    Some(FsWatcherHandle {
        _debouncer: Box::new(debouncer),
        _root: root_clone,
    })
}

fn contains_git_internal(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == ".git")
}
