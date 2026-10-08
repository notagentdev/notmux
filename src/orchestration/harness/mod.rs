//! Harness adapters: how each supported CLI is started unattended.
//!
//! An adapter turns a launch request into an executable, an argument
//! vector, the reserved environment, and the per-run files. It writes the
//! vendor's trust record before the launch intent is committed, so a
//! failure here never reaches the store. Adapters never run the process.
//!
//! Verified flags (see the plan's baseline findings):
//! * notagent: `--yolo --approve --session-id <id> --append-system-prompt <file>`
//!   (`../notagent-main-v2/crates/notagent/src/cli/args.rs`).
//! * Claude Code: `--session-id <uuid> --dangerously-skip-permissions
//!   --append-system-prompt-file <file> --settings <file>`.
//! * Codex: `-C <cwd> -a never --dangerously-bypass-approvals-and-sandbox`.

pub mod claude;
pub mod codex;
pub mod generic;
pub mod notagent;

use notmux_core::orchestration::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub const TASK_FILE: &str = "task.md";
pub const SYSTEM_PROMPT_FILE: &str = "system-prompt.md";
pub const CLAUDE_SETTINGS_FILE: &str = "claude-settings.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentRole {
    Root,
    Worker,
}

impl AgentRole {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentRole::Root => "root",
            AgentRole::Worker => "worker",
        }
    }
}

/// Where the vendors keep their trust records. Resolved once from the
/// environment by the runtime; tests point it at temporary directories.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VendorPaths {
    pub claude_json: PathBuf,
    pub claude_settings: PathBuf,
    pub codex_config: PathBuf,
}

impl VendorPaths {
    pub fn from_env() -> Result<VendorPaths, AgentError> {
        let (claude_json, claude_settings) =
            notmux_hooks::agent_hooks::trust::claude_paths().map_err(trust_error)?;
        let codex_config = notmux_hooks::agent_hooks::trust::codex_config_path().map_err(trust_error)?;
        Ok(VendorPaths {
            claude_json,
            claude_settings,
            codex_config,
        })
    }
}

/// Everything an adapter needs to prepare one launch.
#[derive(Clone, Debug)]
pub struct LaunchRequest<'a> {
    pub run_id: &'a str,
    /// The root's run ID for workers; `None` for a root.
    pub parent_run_id: Option<&'a str>,
    pub role: AgentRole,
    pub name: &'a str,
    pub cwd: &'a Path,
    /// Per-run directory for the task document and generated files.
    pub run_dir: &'a Path,
    /// The assignment text; `None` for a root that starts without one.
    pub task: Option<&'a str>,
    /// Role-specific bootstrap that becomes the system prompt.
    pub bootstrap: &'a str,
    /// Explicit executable (path or name) instead of the harness default.
    pub executable: Option<&'a str>,
    /// Generic only: program and arguments.
    pub argv: &'a [String],
    /// Search path for executables.
    pub search_path: &'a str,
    pub vendor: &'a VendorPaths,
    /// Set when the caller gave up (launch deadline): adapters check it
    /// before every write, so a late preparation touches nothing.
    pub cancel: &'a AtomicBool,
}

/// A cancel flag that never fires, for callers without a deadline.
pub static NEVER_CANCELLED: AtomicBool = AtomicBool::new(false);

/// Refuse to continue once the caller gave up.
pub fn ensure_not_cancelled(req: &LaunchRequest<'_>) -> Result<(), AgentError> {
    if req.cancel.load(Ordering::SeqCst) {
        Err(AgentError::new(
            AgentErrorCode::LaunchFailed,
            "launch was cancelled before preparation finished",
        ))
    } else {
        Ok(())
    }
}

/// The outcome of `prepare`: ready to hand to the managed PTY launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedLaunch {
    pub executable: PathBuf,
    pub args: Vec<String>,
    /// Reserved variables; the launcher adds them on top of the pane env.
    pub env: Vec<(String, String)>,
    /// Harness-specific variables, merged into the ordinary pane
    /// environment. Empty for every current adapter.
    pub extra_env: Vec<(String, String)>,
    pub delivery: DeliveryMode,
    pub completion: CompletionMode,
    pub files: RunFiles,
    pub trust_writes: Vec<TrustWrite>,
}

pub trait Harness: Send + Sync {
    fn prepare(&self, req: &LaunchRequest<'_>) -> Result<PreparedLaunch, AgentError>;
}

pub fn adapter_for(kind: HarnessKind) -> Box<dyn Harness> {
    match kind {
        HarnessKind::Notagent => Box::new(notagent::Notagent),
        HarnessKind::Claude => Box::new(claude::Claude),
        HarnessKind::Codex => Box::new(codex::Codex),
        HarnessKind::Generic => Box::new(generic::Generic),
    }
}

/// The search path managed launches use: the pane PATH on Unix, the
/// process PATH elsewhere.
pub fn default_search_path() -> String {
    #[cfg(not(windows))]
    {
        notmux_terminal::session_backend::get_extended_path()
    }
    #[cfg(windows)]
    {
        std::env::var("PATH").unwrap_or_default()
    }
}

fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Find `spec` as an executable. A path with a separator is checked as
/// given; a bare name is searched on `search_path`. Directories of the
/// notmux agent shims are skipped: managed launches want the real binary
/// because they inject their own settings.
pub fn resolve_executable(spec: &str, search_path: &str) -> Result<PathBuf, AgentError> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err(AgentError::usage("executable must not be empty"));
    }
    let unavailable = |detail: String| AgentError::new(AgentErrorCode::HarnessUnavailable, detail);
    if spec.contains(std::path::MAIN_SEPARATOR) || spec.contains('/') {
        let path = PathBuf::from(spec);
        return if is_executable(&path) {
            Ok(path)
        } else {
            Err(unavailable(format!("{spec} is not an executable file")))
        };
    }
    #[cfg(windows)]
    let candidates: Vec<String> = {
        let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".into());
        let mut v = vec![spec.to_string()];
        v.extend(exts.split(';').filter(|e| !e.is_empty()).map(|e| format!("{spec}{}", e.to_ascii_lowercase())));
        v
    };
    #[cfg(not(windows))]
    let candidates: Vec<String> = vec![spec.to_string()];
    for dir in std::env::split_paths(search_path) {
        if dir.as_os_str().is_empty() || dir.to_string_lossy().contains("notmux-shims") {
            continue;
        }
        for name in &candidates {
            let candidate = dir.join(name);
            if is_executable(&candidate) {
                return Ok(candidate);
            }
        }
    }
    Err(unavailable(format!(
        "{spec} was not found on PATH; install it or pass --executable"
    )))
}

pub fn trust_error(message: String) -> AgentError {
    AgentError::new(AgentErrorCode::TrustPreparationFailed, message)
}

pub fn convert_trust_writes(writes: Vec<notmux_hooks::agent_hooks::trust::TrustWrite>) -> Vec<TrustWrite> {
    writes
        .into_iter()
        .map(|w| TrustWrite {
            path: w.path.to_string_lossy().into_owned(),
            key: w.key,
            value: w.value,
        })
        .collect()
}

/// Write a per-run file, creating the run directory. Failures are launch
/// failures: the run never reaches the store.
pub fn write_run_file(run_dir: &Path, name: &str, content: &str) -> Result<PathBuf, AgentError> {
    std::fs::create_dir_all(run_dir).map_err(|e| {
        AgentError::new(
            AgentErrorCode::LaunchFailed,
            format!("create {}: {e}", run_dir.display()),
        )
    })?;
    let path = run_dir.join(name);
    std::fs::write(&path, content).map_err(|e| {
        AgentError::new(
            AgentErrorCode::LaunchFailed,
            format!("write {}: {e}", path.display()),
        )
    })?;
    Ok(path)
}

/// The reserved environment every managed process receives.
pub fn reserved_env(req: &LaunchRequest<'_>, kind: HarnessKind, task_file: Option<&Path>) -> Vec<(String, String)> {
    let mut env = vec![
        (ENV_RUN_ID.to_string(), req.run_id.to_string()),
        (ENV_AGENT_ROLE.to_string(), req.role.as_str().to_string()),
        (ENV_AGENT_HARNESS.to_string(), kind.as_str().to_string()),
    ];
    if let Some(parent) = req.parent_run_id {
        env.push((ENV_PARENT_RUN_ID.to_string(), parent.to_string()));
    }
    if let Some(task_file) = task_file {
        env.push((ENV_TASK_FILE.to_string(), task_file.to_string_lossy().into_owned()));
    }
    env
}

/// The task document as written to disk: a header naming the run and the
/// assignment, then the root's text verbatim.
pub fn task_document(req: &LaunchRequest<'_>, task: &str, prelude: Option<&str>) -> String {
    let mut doc = String::new();
    if let Some(prelude) = prelude {
        doc.push_str(prelude.trim_end());
        doc.push_str("\n\n---\n\n");
    }
    doc.push_str(&format!("# Assignment: {}\n\n", req.name));
    doc.push_str(&format!("Run: {}\n", req.run_id));
    if let Some(parent) = req.parent_run_id {
        doc.push_str(&format!("Root: {parent}\n"));
    }
    doc.push_str(&format!("Working directory: {}\n\n", req.cwd.display()));
    doc.push_str(task.trim_end());
    doc.push('\n');
    doc
}

/// The first prompt of a worker: a pointer to the document, not the
/// document itself, so the pane's command line stays readable.
pub fn pointer_prompt(task_file: &Path) -> String {
    format!(
        "Your assignment from the NotMux orchestrator is in {}. Read that file completely, then carry it out. \
         When you are done, report with `notmux agent finish` exactly as the file describes.",
        task_file.display()
    )
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// A vendor-path set inside one temporary directory.
    pub fn vendor_paths(dir: &Path) -> VendorPaths {
        VendorPaths {
            claude_json: dir.join("claude").join(".claude.json"),
            claude_settings: dir.join("claude").join("settings.json"),
            codex_config: dir.join("codex").join("config.toml"),
        }
    }

    /// Create a fake executable named `name` in `dir` and return the dir as
    /// a search path entry.
    pub fn fake_exe(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_on_search_path_and_skips_shims() {
        let dir = tempfile::tempdir().unwrap();
        let shims = dir.path().join("notmux-shims");
        let real = dir.path().join("real");
        std::fs::create_dir_all(&shims).unwrap();
        std::fs::create_dir_all(&real).unwrap();
        test_support::fake_exe(&shims, "claude");
        let expected = test_support::fake_exe(&real, "claude");
        let path = std::env::join_paths([&shims, &real]).unwrap();
        let found = resolve_executable("claude", &path.to_string_lossy()).unwrap();
        assert_eq!(found, expected);
        let e = resolve_executable("nope-nothing", &path.to_string_lossy()).unwrap_err();
        assert_eq!(e.code, AgentErrorCode::HarnessUnavailable);
        let explicit = resolve_executable(expected.to_str().unwrap(), "").unwrap();
        assert_eq!(explicit, expected);
        let e = resolve_executable(dir.path().join("missing").to_str().unwrap(), "").unwrap_err();
        assert_eq!(e.code, AgentErrorCode::HarnessUnavailable);
    }

    #[cfg(unix)]
    #[test]
    fn non_executable_files_are_not_resolved() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("codex");
        std::fs::write(&file, "text").unwrap();
        let e = resolve_executable("codex", dir.path().to_str().unwrap()).unwrap_err();
        assert_eq!(e.code, AgentErrorCode::HarnessUnavailable);
    }

    #[test]
    fn task_document_keeps_text_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let vendor = test_support::vendor_paths(dir.path());
        let req = LaunchRequest {
            run_id: "r",
            parent_run_id: Some("root"),
            role: AgentRole::Worker,
            name: "Worker A",
            cwd: dir.path(),
            run_dir: dir.path(),
            task: Some("do  it\n  exactly"),
            bootstrap: "",
            executable: None,
            argv: &[],
            search_path: "",
            vendor: &vendor,
            cancel: &NEVER_CANCELLED,
        };
        let doc = task_document(&req, "do  it\n  exactly", Some("BOOT"));
        assert!(doc.starts_with("BOOT\n\n---\n\n# Assignment: Worker A\n"));
        assert!(doc.ends_with("do  it\n  exactly\n"));
        assert!(doc.contains("Root: root\n"));
    }
}
