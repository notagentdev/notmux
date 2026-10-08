//! notagent adapter. `--yolo --approve` runs unattended, `--session-id`
//! pins the session to the run ID, `--append-system-prompt` takes a file
//! path and appends its contents. notagent itself is not changed by NotMux:
//! no extra flags, no modes, no hook contract beyond what notagent ships.
//! Follow-up work is cooperative: the worker reports with `notmux agent
//! finish` and waits in `notmux agent next`.

use super::*;

pub struct Notagent;

impl Harness for Notagent {
    fn prepare(&self, req: &LaunchRequest<'_>) -> Result<PreparedLaunch, AgentError> {
        let executable = resolve_executable(req.executable.unwrap_or("notagent"), req.search_path)?;
        ensure_not_cancelled(req)?;
        let prompt_file = write_run_file(req.run_dir, SYSTEM_PROMPT_FILE, req.bootstrap)?;
        let task_file = match req.task {
            Some(task) => Some(write_run_file(req.run_dir, TASK_FILE, &task_document(req, task, None))?),
            None => None,
        };
        let mut args = vec![
            "--yolo".to_string(),
            "--approve".to_string(),
            "--session-id".to_string(),
            req.run_id.to_string(),
            "--append-system-prompt".to_string(),
            prompt_file.to_string_lossy().into_owned(),
        ];
        if let Some(task_file) = &task_file {
            args.push(pointer_prompt(task_file));
        }
        Ok(PreparedLaunch {
            env: reserved_env(req, HarnessKind::Notagent, task_file.as_deref()),
            extra_env: vec![],
            executable,
            args,
            delivery: DeliveryMode::Cooperative,
            completion: CompletionMode::Reported,
            files: RunFiles {
                task_file: task_file.map(|p| p.to_string_lossy().into_owned()),
                system_prompt_file: Some(prompt_file.to_string_lossy().into_owned()),
                settings_file: None,
            },
            trust_writes: vec![],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

    #[test]
    fn worker_argv_and_files() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let exe = fake_exe(&bin, "notagent");
        let vendor = vendor_paths(dir.path());
        let run_dir = dir.path().join("run");
        let req = LaunchRequest {
            run_id: "11111111-1111-4111-8111-111111111111",
            parent_run_id: Some("root-id"),
            role: AgentRole::Worker,
            name: "Worker A",
            cwd: dir.path(),
            run_dir: &run_dir,
            task: Some("Implement the thing."),
            bootstrap: "BOOTSTRAP",
            executable: None,
            argv: &[],
            search_path: bin.to_str().unwrap(),
            vendor: &vendor,
            cancel: &NEVER_CANCELLED,
        };
        let launch = Notagent.prepare(&req).unwrap();
        assert_eq!(launch.executable, exe);
        assert_eq!(&launch.args[..4], &["--yolo", "--approve", "--session-id", req.run_id]);
        assert_eq!(launch.args[4], "--append-system-prompt");
        assert_eq!(std::fs::read_to_string(&launch.args[5]).unwrap(), "BOOTSTRAP");
        assert_eq!(launch.args.len(), 7, "only the documented flags plus the pointer prompt");
        let task_file = run_dir.join(TASK_FILE);
        assert!(launch.args.last().unwrap().contains(task_file.to_str().unwrap()));
        assert!(std::fs::read_to_string(&task_file).unwrap().ends_with("Implement the thing.\n"));
        let env: std::collections::HashMap<_, _> = launch.env.into_iter().collect();
        assert_eq!(env["NOTMUX_RUN_ID"], req.run_id);
        assert_eq!(env["NOTMUX_PARENT_RUN_ID"], "root-id");
        assert_eq!(env["NOTMUX_AGENT_ROLE"], "worker");
        assert_eq!(env["NOTMUX_AGENT_HARNESS"], "notagent");
        assert_eq!(env["NOTMUX_TASK_FILE"], task_file.to_str().unwrap());
        assert!(launch.trust_writes.is_empty());
        assert!(launch.extra_env.is_empty(), "nothing notagent-specific is set");
        assert_eq!(launch.delivery, DeliveryMode::Cooperative);
    }

    #[test]
    fn root_without_task_has_no_task_file_or_parent() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        fake_exe(&bin, "notagent");
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
            bootstrap: "B",
            executable: None,
            argv: &[],
            search_path: bin.to_str().unwrap(),
            vendor: &vendor,
            cancel: &NEVER_CANCELLED,
        };
        let launch = Notagent.prepare(&req).unwrap();
        assert!(!run_dir.join(TASK_FILE).exists());
        assert!(launch.env.iter().all(|(k, _)| k != "NOTMUX_TASK_FILE" && k != "NOTMUX_PARENT_RUN_ID"));
        assert!(launch.env.iter().any(|(k, v)| k == "NOTMUX_AGENT_ROLE" && v == "root"));
        assert_eq!(launch.args.len(), 6);
    }

    #[test]
    fn missing_executable_is_unavailable_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let vendor = vendor_paths(dir.path());
        let run_dir = dir.path().join("run");
        let req = LaunchRequest {
            run_id: "r",
            parent_run_id: None,
            role: AgentRole::Worker,
            name: "w",
            cwd: dir.path(),
            run_dir: &run_dir,
            task: Some("t"),
            bootstrap: "B",
            executable: None,
            argv: &[],
            search_path: dir.path().to_str().unwrap(),
            vendor: &vendor,
            cancel: &NEVER_CANCELLED,
        };
        let e = Notagent.prepare(&req).unwrap_err();
        assert_eq!(e.code, AgentErrorCode::HarnessUnavailable);
        assert!(!run_dir.exists());
    }
}
