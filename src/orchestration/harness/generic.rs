//! Generic adapter: any executable with an argument vector. No bootstrap,
//! no trust step, no mailbox; the exit code decides the assignment. The
//! task document is still written so the process can read it through
//! `NOTMUX_TASK_FILE`.

use super::*;

pub struct Generic;

impl Harness for Generic {
    fn prepare(&self, req: &LaunchRequest<'_>) -> Result<PreparedLaunch, AgentError> {
        let Some((program, rest)) = req.argv.split_first() else {
            return Err(AgentError::usage("a generic worker needs an executable and arguments after `--`"));
        };
        let executable = resolve_executable(req.executable.unwrap_or(program), req.search_path)?;
        ensure_not_cancelled(req)?;
        let task_file = match req.task {
            Some(task) => Some(write_run_file(req.run_dir, TASK_FILE, &task_document(req, task, None))?),
            None => None,
        };
        Ok(PreparedLaunch {
            env: reserved_env(req, HarnessKind::Generic, task_file.as_deref()),
            extra_env: vec![],
            executable,
            args: rest.to_vec(),
            delivery: DeliveryMode::Cooperative,
            completion: CompletionMode::ExitCode,
            files: RunFiles {
                task_file: task_file.map(|p| p.to_string_lossy().into_owned()),
                system_prompt_file: None,
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
    fn argv_passes_through_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let exe = fake_exe(&bin, "runner");
        let vendor = vendor_paths(dir.path());
        let run_dir = dir.path().join("run");
        let argv = vec![
            "runner".to_string(),
            "--flag".to_string(),
            "with space".to_string(),
            "\"quoted\"".to_string(),
            "ünïcödé".to_string(),
        ];
        let req = LaunchRequest {
            run_id: "r",
            parent_run_id: Some("root"),
            role: AgentRole::Worker,
            name: "gen",
            cwd: dir.path(),
            run_dir: &run_dir,
            task: Some("t"),
            bootstrap: "ignored",
            executable: None,
            argv: &argv,
            search_path: bin.to_str().unwrap(),
            vendor: &vendor,
            cancel: &NEVER_CANCELLED,
        };
        let launch = Generic.prepare(&req).unwrap();
        assert_eq!(launch.executable, exe);
        assert_eq!(launch.args, &argv[1..]);
        assert_eq!(launch.completion, CompletionMode::ExitCode);
        assert!(run_dir.join(TASK_FILE).exists());
        assert!(!run_dir.join(SYSTEM_PROMPT_FILE).exists());
        let empty: Vec<String> = vec![];
        let req = LaunchRequest { argv: &empty, ..req };
        assert_eq!(Generic.prepare(&req).unwrap_err().code, AgentErrorCode::Usage);
    }
}
