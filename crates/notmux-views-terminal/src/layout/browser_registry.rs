//! Registry of live browser panes plus the dispatcher for remote automation
//! commands (`POST /v1/browser`, `notmux browser …`).
//!
//! Browser panes register themselves on construction, keyed by their layout
//! slot id. Entries hold weak handles — a dead entry is pruned on the next
//! access, so panes need no explicit deregistration (and a re-created pane
//! for the same slot simply replaces the old entry).

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use gpui::{App, Entity, WeakEntity};
use notmux_core::api::BrowserRequest;
use notmux_workspace::state::Workspace;

use crate::layout::browser_pane::BrowserPane;

/// Answers one automation request; may be called from a spawned task after
/// the page JavaScript resolved.
pub type BrowserRespond = Box<dyn FnOnce(Result<serde_json::Value, String>) + Send + 'static>;

struct Registered {
    project_id: String,
    pane: WeakEntity<BrowserPane>,
}

static REGISTRY: LazyLock<Mutex<HashMap<String, Registered>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Registers a pane under its slot id (re-registration replaces).
pub(crate) fn register(slot_id: &str, project_id: &str, pane: WeakEntity<BrowserPane>) {
    REGISTRY.lock().unwrap().insert(
        slot_id.to_string(),
        Registered {
            project_id: project_id.to_string(),
            pane,
        },
    );
}

/// All live panes as (slot_id, project_id, entity), pruning dead entries.
/// Sorted by slot id for deterministic output.
fn live_panes() -> Vec<(String, String, Entity<BrowserPane>)> {
    let mut registry = REGISTRY.lock().unwrap();
    registry.retain(|_, r| r.pane.upgrade().is_some());
    let mut panes: Vec<_> = registry
        .iter()
        .filter_map(|(slot, r)| {
            r.pane
                .upgrade()
                .map(|pane| (slot.clone(), r.project_id.clone(), pane))
        })
        .collect();
    panes.sort_by(|a, b| a.0.cmp(&b.0));
    panes
}

/// Executes one remote browser-automation request and answers through
/// `respond` — possibly asynchronously, once the page JavaScript resolved.
///
/// Targeting: `pane` selects a slot id explicitly; otherwise the only open
/// browser pane is used. `open` additionally creates a fresh pane (like the
/// UI's OpenBrowser action) when none exists, or when `project_id` names the
/// project to split explicitly.
pub fn execute_browser_request(
    req: BrowserRequest,
    workspace: &Entity<Workspace>,
    respond: BrowserRespond,
    cx: &mut App,
) {
    let panes = live_panes();

    if req.action == "list" {
        let list: Vec<serde_json::Value> = panes
            .iter()
            .map(|(slot, project, pane)| {
                serde_json::json!({
                    "pane": slot,
                    "project_id": project,
                    "url": pane.read(cx).url(),
                })
            })
            .collect();
        let text = if panes.is_empty() {
            "no browser panes open".to_string()
        } else {
            panes
                .iter()
                .map(|(slot, project, pane)| {
                    format!("{slot}\t{project}\t{}", pane.read(cx).url())
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        respond(Ok(serde_json::json!({ "text": text, "panes": list })));
        return;
    }

    // Who asked? A request from inside a NotMux terminal (an agent) is
    // scoped to that terminal's project: its own browser panes are the
    // implicit target, and a new pane is created there — never in whatever
    // project the user happens to be looking at.
    let caller = resolve_caller(req.terminal_id.as_deref(), workspace, cx);

    // An explicit project on `open` is the request for a *new* pane there.
    if req.action == "open" && req.project_id.is_some() && req.pane.is_none() {
        open_new_pane(req, caller, workspace, respond, cx);
        return;
    }

    let target = if let Some(slot) = &req.pane {
        match panes.iter().find(|(s, ..)| s == slot) {
            Some((_, _, pane)) => pane.clone(),
            None => {
                respond(Err(format!(
                    "browser pane not found: {slot} (see `notmux browser list`)"
                )));
                return;
            }
        }
    } else {
        let scope = caller.as_ref().map(|c| c.project_id.as_str());
        let candidates: Vec<&(String, String, Entity<BrowserPane>)> = panes
            .iter()
            .filter(|(_, project, _)| scope.is_none_or(|p| p == project))
            .collect();
        match candidates.len() {
            1 => candidates[0].2.clone(),
            0 => {
                // No pane in scope: `open` creates one, everything else
                // needs an existing pane.
                if req.action == "open" {
                    open_new_pane(req, caller, workspace, respond, cx);
                } else if scope.is_some() {
                    respond(Err(
                        "no browser pane is open in this terminal's project — `notmux browser open <url>` first, or pass `--pane <id>`"
                            .to_string(),
                    ));
                } else {
                    respond(Err(
                        "no browser pane is open — `notmux browser open <url>` first".to_string(),
                    ));
                }
                return;
            }
            _ => {
                let listing = candidates
                    .iter()
                    .map(|(slot, project, _)| format!("{slot} ({project})"))
                    .collect::<Vec<_>>()
                    .join("\n");
                respond(Err(format!(
                    "multiple browser panes open — pass `--pane <id>`; open panes:\n{listing}"
                )));
                return;
            }
        }
    };

    target.update(cx, |pane, cx| pane.automation_execute(req, respond, cx));
}

/// Splits a fresh browser pane off the target project and loads `url`
/// (mirrors the UI's OpenBrowser action). The pane and its webview are
/// created lazily on the next render, so follow-up commands should
/// `browser list` / `snapshot` once it is up.
/// The pane a browser request came from: its project and layout slot.
struct Caller {
    project_id: String,
    slot_id: Option<String>,
}

/// Resolve `terminal_id` (from the CLI's `NOTMUX_TERMINAL_ID`) to the
/// project and slot hosting that terminal. `None` when the request did not
/// come from inside a NotMux terminal or the terminal is gone.
fn resolve_caller(
    terminal_id: Option<&str>,
    workspace: &Entity<Workspace>,
    cx: &App,
) -> Option<Caller> {
    let tid = terminal_id?;
    let ws = workspace.read(cx);
    let project = ws.find_project_for_terminal(tid)?;
    let slot_id = project
        .layout
        .as_ref()
        .and_then(|l| l.find_terminal_path(tid).and_then(|p| l.get_at_path(&p)))
        .and_then(|n| n.slot_id().map(str::to_string));
    Some(Caller {
        project_id: project.id.clone(),
        slot_id,
    })
}

fn open_new_pane(
    req: BrowserRequest,
    caller: Option<Caller>,
    workspace: &Entity<Workspace>,
    respond: BrowserRespond,
    cx: &mut App,
) {
    let Some(url) = req.url.clone().filter(|u| !u.trim().is_empty()) else {
        respond(Err("open requires `url`".to_string()));
        return;
    };
    let project_id = match req.project_id.clone().or_else(|| caller.as_ref().map(|c| c.project_id.clone())) {
        Some(id) => id,
        None => {
            let ws = workspace.read(cx);
            // Default to the project the user is currently in (its focused
            // pane), falling back to the only project when there is just one.
            // add_browser_right makes that project visible, so the pane shows
            // up right where the user is looking.
            if let Some(id) = ws.focused_project_id().cloned() {
                id
            } else {
                let projects = &ws.data().projects;
                match projects.len() {
                    1 => projects[0].id.clone(),
                    _ => {
                        let ids = projects
                            .iter()
                            .map(|p| p.id.clone())
                            .collect::<Vec<_>>()
                            .join(", ");
                        respond(Err(format!(
                            "no project is focused — pass `--project <id>`; projects: {ids}"
                        )));
                        return;
                    }
                }
            }
        }
    };
    if workspace.read(cx).project(&project_id).is_none() {
        respond(Err(format!("project not found: {project_id}")));
        return;
    }
    let url = super::browser_pane::normalize_url(&url);
    // The caller's slot only steers placement inside its own project; an
    // explicit `--project` elsewhere behaves like a user action there.
    let caller_slot = caller
        .filter(|c| c.project_id == project_id)
        .and_then(|c| c.slot_id);
    workspace.update(cx, |ws, cx| {
        ws.add_browser_right_for(&project_id, &url, caller_slot.as_deref(), cx)
    });
    respond(Ok(serde_json::json!({
        "text": format!(
            "opened a new browser pane in project {project_id} — loading {url}; \
             run `browser list` and `snapshot` once it rendered"
        ),
    })));
}
