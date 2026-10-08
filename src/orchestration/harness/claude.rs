//! Claude Code adapter. The session ID is the run ID, permissions are
//! skipped, the bootstrap is appended through a file, and a per-run
//! `--settings` file carries the hooks that deliver follow-up work:
//! `Stop` hands the next assignment into the running conversation,
//! `UserPromptSubmit` drains the inbox into context. Both call the notmux
//! binary through `NOTMUX_BINARY_PATH`, which every pane sets.
//!
//! Trust is written to `.claude.json` and `settings.json` before the launch.

use super::*;

pub struct Claude;

/// Seconds Claude waits for the Stop hook. Long on purpose: the hook
/// blocks until the next assignment arrives or the wait limit elapses.
pub const STOP_HOOK_TIMEOUT_S: u64 = 600;
pub const PROMPT_HOOK_TIMEOUT_S: u64 = 15;

/// The per-run settings document. Hook commands invoke `notmux agent hook`,
/// which reads the payload from stdin and answers in Claude's hook JSON.
pub fn settings_document() -> serde_json::Value {
    let stop = "\"$NOTMUX_BINARY_PATH\" agent hook --event stop";
    let prompt = "\"$NOTMUX_BINARY_PATH\" agent hook --event prompt";
    serde_json::json!({
        "skipDangerousModePermissionPrompt": true,
        "hooks": {
            "Stop": [{ "hooks": [{ "type": "command", "command": stop, "timeout": STOP_HOOK_TIMEOUT_S }] }],
            "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": prompt, "timeout": PROMPT_HOOK_TIMEOUT_S }] }]
        }
    })
}

impl Harness for Claude {
    fn prepare(&self, req: &LaunchRequest<'_>) -> Result<PreparedLaunch, AgentError> {
        let executable = resolve_executable(req.executable.unwrap_or("claude"), req.search_path)?;
        ensure_not_cancelled(req)?;
        let trust_writes = notmux_hooks::agent_hooks::trust::trust_claude_project_at(
            &req.vendor.claude_json,
            &req.vendor.claude_settings,
            req.cwd,
        )
        .map_err(trust_error)?;
        ensure_not_cancelled(req)?;
        let prompt_file = write_run_file(req.run_dir, SYSTEM_PROMPT_FILE, req.bootstrap)?;
        let settings_text = serde_json::to_string_pretty(&settings_document())
            .map_err(|e| AgentError::new(AgentErrorCode::LaunchFailed, e.to_string()))?;
        let settings_file = write_run_file(req.run_dir, CLAUDE_SETTINGS_FILE, &settings_text)?;
        let task_file = match req.task {
            Some(task) => Some(write_run_file(req.run_dir, TASK_FILE, &task_document(req, task, None))?),
            None => None,
        };
        let mut args = vec![
            "--session-id".to_string(),
            req.run_id.to_string(),
            "--dangerously-skip-permissions".to_string(),
            "--append-system-prompt-file".to_string(),
            prompt_file.to_string_lossy().into_owned(),
            "--settings".to_string(),
            settings_file.to_string_lossy().into_owned(),
        ];
        if let Some(task_file) = &task_file {
            args.push(pointer_prompt(task_file));
        }
        Ok(PreparedLaunch {
            env: reserved_env(req, HarnessKind::Claude, task_file.as_deref()),
            extra_env: vec![],
            executable,
            args,
            delivery: DeliveryMode::Hook,
            completion: CompletionMode::Reported,
            files: RunFiles {
                task_file: task_file.map(|p| p.to_string_lossy().into_owned()),
                system_prompt_file: Some(prompt_file.to_string_lossy().into_owned()),
                settings_file: Some(settings_file.to_string_lossy().into_owned()),
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
    fn worker_argv_trust_and_settings() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let exe = fake_exe(&bin, "claude");
        let vendor = vendor_paths(dir.path());
        let run_dir = dir.path().join("run");
        let cwd = dir.path().join("work");
        std::fs::create_dir(&cwd).unwrap();
        let req = LaunchRequest {
            run_id: "22222222-2222-4222-8222-222222222222",
            parent_run_id: Some("root"),
            role: AgentRole::Worker,
            name: "Worker B",
            cwd: &cwd,
            run_dir: &run_dir,
            task: Some("Fix it."),
            bootstrap: "SYSTEM",
            executable: None,
            argv: &[],
            search_path: bin.to_str().unwrap(),
            vendor: &vendor,
            cancel: &NEVER_CANCELLED,
        };
        let launch = Claude.prepare(&req).unwrap();
        assert_eq!(launch.executable, exe);
        assert_eq!(&launch.args[..3], &["--session-id", req.run_id, "--dangerously-skip-permissions"]);
        assert_eq!(launch.args[3], "--append-system-prompt-file");
        assert_eq!(std::fs::read_to_string(&launch.args[4]).unwrap(), "SYSTEM");
        assert_eq!(launch.args[5], "--settings");
        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&launch.args[6]).unwrap()).unwrap();
        assert_eq!(settings["skipDangerousModePermissionPrompt"], true);
        assert_eq!(
            settings["hooks"]["Stop"][0]["hooks"][0]["command"],
            "\"$NOTMUX_BINARY_PATH\" agent hook --event stop"
        );
        assert_eq!(settings["hooks"]["Stop"][0]["hooks"][0]["timeout"], STOP_HOOK_TIMEOUT_S);
        assert!(settings["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .ends_with("--event prompt"));
        assert!(launch.args[7].contains(run_dir.join(TASK_FILE).to_str().unwrap()));
        // Trust landed in the temporary vendor files, nowhere else.
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&vendor.claude_json).unwrap()).unwrap();
        assert_eq!(json["projects"][cwd.to_str().unwrap()]["hasTrustDialogAccepted"], true);
        assert!(launch.trust_writes.iter().any(|w| w.key.contains("hasTrustDialogAccepted")));
        assert_eq!(launch.files.settings_file.as_deref(), Some(launch.args[6].as_str()));
    }

    #[cfg(unix)]
    #[test]
    fn read_only_trust_file_fails_before_any_run_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        fake_exe(&bin, "claude");
        let vendor = vendor_paths(dir.path());
        std::fs::create_dir_all(vendor.claude_json.parent().unwrap()).unwrap();
        std::fs::write(&vendor.claude_json, "{}").unwrap();
        std::fs::set_permissions(&vendor.claude_json, std::fs::Permissions::from_mode(0o444)).unwrap();
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
            search_path: bin.to_str().unwrap(),
            vendor: &vendor,
            cancel: &NEVER_CANCELLED,
        };
        let e = Claude.prepare(&req).unwrap_err();
        assert_eq!(e.code, AgentErrorCode::TrustPreparationFailed);
        assert!(!run_dir.exists());
    }
}
