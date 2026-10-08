//! Codex adapter. Codex has no system-prompt flag, so the bootstrap is
//! prepended to the task document and the first prompt points at the
//! file. Follow-up work is cooperative only: the worker blocks in
//! `notmux agent next` when its assignment is done.
//!
//! Trust (`projects.<cwd>.trust_level`) is written to `config.toml` before
//! the launch.

use super::*;

pub struct Codex;

impl Harness for Codex {
    fn prepare(&self, req: &LaunchRequest<'_>) -> Result<PreparedLaunch, AgentError> {
        let executable = resolve_executable(req.executable.unwrap_or("codex"), req.search_path)?;
        ensure_not_cancelled(req)?;
        let trust_writes =
            notmux_hooks::agent_hooks::trust::trust_codex_project_at(&req.vendor.codex_config, req.cwd)
                .map_err(trust_error)?;
        ensure_not_cancelled(req)?;
        let task_file = match req.task {
            Some(task) => Some(write_run_file(
                req.run_dir,
                TASK_FILE,
                &task_document(req, task, Some(req.bootstrap)),
            )?),
            None => None,
        };
        // codex 0.155 refuses `-a never` next to the bypass flag ("cannot be
        // used with"); the bypass alone already means no approvals.
        let mut args = vec![
            "-C".to_string(),
            req.cwd.to_string_lossy().into_owned(),
            "--dangerously-bypass-approvals-and-sandbox".to_string(),
        ];
        match &task_file {
            Some(task_file) => args.push(pointer_prompt(task_file)),
            None => {
                // A root starts without an assignment; the bootstrap is
                // still its first message.
                let prompt_file = write_run_file(req.run_dir, SYSTEM_PROMPT_FILE, req.bootstrap)?;
                args.push(format!(
                    "Read {} completely before doing anything else; it describes your role as NotMux orchestrator.",
                    prompt_file.display()
                ));
            }
        }
        Ok(PreparedLaunch {
            env: reserved_env(req, HarnessKind::Codex, task_file.as_deref()),
            extra_env: vec![],
            executable,
            args,
            delivery: DeliveryMode::Cooperative,
            completion: CompletionMode::Reported,
            files: RunFiles {
                task_file: task_file.map(|p| p.to_string_lossy().into_owned()),
                system_prompt_file: req.task.is_none().then(|| {
                    req.run_dir.join(SYSTEM_PROMPT_FILE).to_string_lossy().into_owned()
                }),
                settings_file: None,
            },
            trust_writes: convert_trust_writes(trust_writes),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

    #[test]
    fn worker_argv_bootstrap_in_document_and_trust() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let exe = fake_exe(&bin, "codex");
        let vendor = vendor_paths(dir.path());
        let run_dir = dir.path().join("run");
        let cwd = dir.path().join("work");
        std::fs::create_dir(&cwd).unwrap();
        let req = LaunchRequest {
            run_id: "r",
            parent_run_id: Some("root"),
            role: AgentRole::Worker,
            name: "Worker C",
            cwd: &cwd,
            run_dir: &run_dir,
            task: Some("Review it."),
            bootstrap: "BOOT",
            executable: None,
            argv: &[],
            search_path: bin.to_str().unwrap(),
            vendor: &vendor,
            cancel: &NEVER_CANCELLED,
        };
        let launch = Codex.prepare(&req).unwrap();
        assert_eq!(launch.executable, exe);
        assert_eq!(
            &launch.args[..3],
            &["-C", cwd.to_str().unwrap(), "--dangerously-bypass-approvals-and-sandbox"]
        );
        assert!(!launch.args.iter().any(|a| a == "-a"), "codex refuses -a beside the bypass flag");
        let doc = std::fs::read_to_string(run_dir.join(TASK_FILE)).unwrap();
        assert!(doc.starts_with("BOOT\n\n---\n\n# Assignment: Worker C"));
        assert!(doc.ends_with("Review it.\n"));
        let toml = std::fs::read_to_string(&vendor.codex_config).unwrap();
        assert!(toml.contains("trust_level = \"trusted\""));
        assert_eq!(launch.delivery, DeliveryMode::Cooperative);
        assert!(launch.files.system_prompt_file.is_none());
        assert!(launch.trust_writes.iter().any(|w| w.key.ends_with("trust_level")));
    }

    #[test]
    fn root_gets_bootstrap_file_as_first_message() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        fake_exe(&bin, "codex");
        let vendor = vendor_paths(dir.path());
        let run_dir = dir.path().join("run");
        let req = LaunchRequest {
            run_id: "r",
            parent_run_id: None,
            role: AgentRole::Root,
            name: "Orchestrator",
            cwd: dir.path(),
            run_dir: &run_dir,
            task: None,
            bootstrap: "ROLE",
            executable: None,
            argv: &[],
            search_path: bin.to_str().unwrap(),
            vendor: &vendor,
            cancel: &NEVER_CANCELLED,
        };
        let launch = Codex.prepare(&req).unwrap();
        assert_eq!(std::fs::read_to_string(run_dir.join(SYSTEM_PROMPT_FILE)).unwrap(), "ROLE");
        assert!(launch.args.last().unwrap().contains(SYSTEM_PROMPT_FILE));
        assert!(launch.files.task_file.is_none());
    }
}
