//! The two [`Host`]s the application hands to the runtime.
//!
//! [`AppHost`] runs on the GPUI thread and can do everything: it is used
//! for launches (panes) and for `register` (project lookup). [`BackgroundHost`]
//! runs on any thread and serves every store-only command (`next`, `wait`,
//! `message`, `finish`, `stop`, …), so their store writes never block the
//! UI; it only kills processes and publishes state, both thread-safe.

use super::harness::PreparedLaunch;
use super::runtime::Host;
use crate::terminal::backend::TerminalBackend;
use crate::terminal::terminal::{ManagedRunInfo, Terminal, TerminalSize};
use crate::views::root::TerminalsRegistry;
use crate::workspace::state::Workspace;
use gpui::{App, Entity};
use notmux_core::orchestration::{AssignmentState, RunRecord};
use std::collections::HashMap;
use std::sync::Arc;

pub struct AppHost<'a> {
    pub workspace: &'a Entity<Workspace>,
    pub backend: &'a Arc<dyn TerminalBackend>,
    pub terminals: &'a TerminalsRegistry,
    pub cx: &'a mut App,
}

/// Show a run on its terminal model: header, sidebar rollup, API.
pub fn publish_to_terminal(terminals: &TerminalsRegistry, run: &RunRecord, task_state: Option<AssignmentState>) {
    use notmux_core::agent_state::{AgentState, AgentStateSource};
    use notmux_core::orchestration::{HarnessKind, ProcessState, WorkerAvailability};
    let terminal = terminals.lock().get(&run.terminal_id).cloned();
    let Some(terminal) = terminal else {
        return;
    };
    let (worker_state, remaining_starts) = if run.role.is_root() {
        let state = match run.root_state {
            Some(s) => s.as_str(),
            None => "unknown",
        };
        (state.to_string(), run.budget.as_ref().map(|b| b.remaining_starts))
    } else {
        (run.worker_state.as_str().to_string(), None)
    };
    terminal.set_managed_run(Some(ManagedRunInfo {
        run_id: run.id.clone(),
        role: run.role.as_str().to_string(),
        harness: run.harness.as_str().to_string(),
        name: run.name.clone(),
        worker_state,
        process_state: run.process_state.as_str().to_string(),
        remaining_starts,
        task_state: task_state.map(|s| s.as_str().to_string()),
    }));
    // Workers without their own hooks (generic, Codex) still roll up into
    // the sidebar through the ordinary agent state: working while an
    // assignment runs, idle between, gone with the process.
    if !run.role.is_root() {
        terminal.set_agent_kind(Some(run.harness.as_str().to_string()));
        let state = match (run.process_state, run.worker_state) {
            (ProcessState::Exited | ProcessState::Lost, _) => None,
            (_, WorkerAvailability::Busy | WorkerAvailability::Starting) => Some(AgentState::Working),
            (_, WorkerAvailability::Stopped) => Some(AgentState::Done),
            (_, WorkerAvailability::Waiting | WorkerAvailability::Idle) => Some(AgentState::Idle),
        };
        if run.harness == HarnessKind::Generic || run.harness == HarnessKind::Codex || state.is_none() {
            terminal.set_agent_state(state, AgentStateSource::Hook);
        }
    }
}

fn kill_and_forget(backend: &Arc<dyn TerminalBackend>, terminals: &TerminalsRegistry, terminal_id: &str) {
    backend.kill(terminal_id);
    terminals.lock().remove(terminal_id);
}

impl Host for AppHost<'_> {
    fn project_for_terminal(&self, terminal_id: &str) -> Option<(String, String)> {
        self.workspace
            .read(self.cx)
            .find_project_for_terminal(terminal_id)
            .map(|p| (p.id.clone(), p.path.clone()))
    }

    fn project_path(&self, project_id: &str) -> Option<String> {
        self.workspace
            .read(self.cx)
            .project(project_id)
            .map(|p| p.path.clone())
    }

    fn focused_project(&self) -> Option<String> {
        self.workspace.read(self.cx).focused_project_id().cloned()
    }

    fn terminal_env(&self) -> HashMap<String, String> {
        crate::settings::settings(self.cx).terminal_env
    }

    fn launch(
        &mut self,
        terminal_id: &str,
        cwd: &str,
        launch: &PreparedLaunch,
        env: &HashMap<String, String>,
    ) -> Result<(), String> {
        // The model must exist before the first byte arrives: the PTY event
        // loop drops output for unknown terminals.
        let terminal = Arc::new(Terminal::new(
            terminal_id.to_string(),
            TerminalSize::default(),
            self.backend.transport(),
            cwd.to_string(),
        ));
        self.terminals
            .lock()
            .insert(terminal_id.to_string(), terminal.clone());
        let mut env = env.clone();
        for (key, value) in &launch.extra_env {
            env.insert(key.clone(), value.clone());
        }
        match self.backend.create_managed_terminal(
            terminal_id,
            &launch.executable,
            &launch.args,
            cwd,
            &env,
            &launch.env,
        ) {
            Ok(()) => {
                if let Some(pid) = self.backend.get_shell_pid(terminal_id) {
                    terminal.set_shell_pid(pid);
                }
                Ok(())
            }
            Err(e) => {
                self.terminals.lock().remove(terminal_id);
                Err(e.to_string())
            }
        }
    }

    fn insert_pane(
        &mut self,
        project_id: &str,
        slot_id: &str,
        terminal_id: &str,
        run_id: &str,
        name: &str,
        focus: bool,
    ) -> Result<(), String> {
        self.workspace.update(self.cx, |ws, cx| {
            let path = ws.insert_managed_terminal(project_id, slot_id, terminal_id, run_id, name, cx)?;
            if focus {
                ws.set_focused_project(Some(project_id.to_string()), cx);
                ws.set_focused_terminal(project_id.to_string(), path, cx);
            }
            Ok(())
        })
    }

    fn remove_pane(&mut self, project_id: &str, slot_id: &str) {
        self.workspace.update(self.cx, |ws, cx| {
            ws.remove_managed_terminal(project_id, slot_id, cx);
        });
    }

    fn kill_terminal(&mut self, terminal_id: &str) {
        kill_and_forget(self.backend, self.terminals, terminal_id);
    }

    fn publish_run(&mut self, run: &RunRecord, task_state: Option<AssignmentState>) {
        publish_to_terminal(self.terminals, run, task_state);
    }
}

/// A host without the UI: enough for every command that only reads or
/// writes the store, kills processes, or publishes state. Launches and
/// `register` need [`AppHost`]; here they fail with a clear message.
pub struct BackgroundHost {
    pub backend: Arc<dyn TerminalBackend>,
    pub terminals: TerminalsRegistry,
    /// `(project id, project path)` of the caller's terminal, looked up on
    /// the UI thread beforehand; only `register` needs it.
    pub project: Option<(String, String)>,
}

impl Host for BackgroundHost {
    fn project_for_terminal(&self, _terminal_id: &str) -> Option<(String, String)> {
        self.project.clone()
    }

    fn project_path(&self, _project_id: &str) -> Option<String> {
        None
    }

    fn focused_project(&self) -> Option<String> {
        None
    }

    fn terminal_env(&self) -> HashMap<String, String> {
        HashMap::new()
    }

    fn launch(
        &mut self,
        _terminal_id: &str,
        _cwd: &str,
        _launch: &PreparedLaunch,
        _env: &HashMap<String, String>,
    ) -> Result<(), String> {
        Err("launches need the UI thread".to_string())
    }

    fn insert_pane(
        &mut self,
        _project_id: &str,
        _slot_id: &str,
        _terminal_id: &str,
        _run_id: &str,
        _name: &str,
        _focus: bool,
    ) -> Result<(), String> {
        Err("panes need the UI thread".to_string())
    }

    fn remove_pane(&mut self, _project_id: &str, _slot_id: &str) {}

    fn kill_terminal(&mut self, terminal_id: &str) {
        kill_and_forget(&self.backend, &self.terminals, terminal_id);
    }

    fn publish_run(&mut self, run: &RunRecord, task_state: Option<AssignmentState>) {
        publish_to_terminal(&self.terminals, run, task_state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::harness::test_support::{fake_exe, vendor_paths};
    use crate::orchestration::runtime::Runtime;
    use crate::orchestration::runtime::test_support::FakeHost;
    use crate::terminal::backend::LocalBackend;
    use crate::terminal::pty_manager::PtyManager;
    use notmux_core::orchestration::*;
    use notmux_terminal::session_backend::SessionBackend;

    /// Store-only commands run through the background host, off the UI
    /// thread; a launch or a registration does not.
    #[test]
    fn background_host_serves_store_commands_and_refuses_launches() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        fake_exe(&bin, "runner");
        let work = dir.path().join("w");
        std::fs::create_dir(&work).unwrap();
        let (runtime, _) =
            Runtime::open(&dir.path().join("orch"), vendor_paths(dir.path()), bin.to_string_lossy().into_owned()).unwrap();
        // Register and launch through the UI host.
        let mut ui = FakeHost::default();
        ui.projects.insert("p".into(), work.to_string_lossy().into_owned());
        ui.terminals.insert("t".into(), "p".into());
        let by_term = Caller {
            run_id: None,
            terminal_id: Some("t".into()),
        };
        let AgentResponse::Run(root) = runtime
            .handle(&by_term, AgentRequest::Register { harness: HarnessKind::Notagent, total_start_budget: 2 }, &mut ui)
            .unwrap()
        else {
            panic!()
        };
        let AgentResponse::Run(worker) = runtime
            .handle(
                &by_term,
                AgentRequest::Spawn {
                    request_id: crate::orchestration::new_id(),
                    name: "w".into(),
                    harness: HarnessKind::Generic,
                    task: "t".into(),
                    cwd: None,
                    executable: None,
                    argv: vec!["runner".into()],
                    completion: None,
                },
                &mut ui,
            )
            .unwrap()
        else {
            panic!()
        };

        // Everything else through the background host, with a real (idle)
        // PTY manager and registry.
        let (pty, _events) = PtyManager::new(SessionBackend::None);
        let pty = Arc::new(pty);
        let mut bg = BackgroundHost {
            backend: Arc::new(LocalBackend::new(pty.clone())),
            terminals: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            project: None,
        };
        let as_worker = Caller {
            run_id: Some(worker.id.clone()),
            terminal_id: None,
        };
        let msg = runtime
            .handle(
                &as_worker,
                AgentRequest::Message {
                    request_id: crate::orchestration::new_id(),
                    recipient_id: root.id.clone(),
                    body: "hi".into(),
                },
                &mut bg,
            )
            .unwrap();
        assert!(matches!(msg, AgentResponse::Message(_)));
        let stopped = runtime
            .handle(&by_term, AgentRequest::Stop { target_id: Some(worker.id.clone()) }, &mut bg)
            .unwrap();
        assert!(matches!(stopped, AgentResponse::Stopped { .. }));
        // Registration needs the project lookup only the UI has.
        let e = runtime
            .handle(
                &Caller {
                    run_id: None,
                    terminal_id: Some("other".into()),
                },
                AgentRequest::Register { harness: HarnessKind::Codex, total_start_budget: 1 },
                &mut bg,
            )
            .unwrap_err();
        assert_eq!(e.code, AgentErrorCode::NotRegistered);
    }
}
