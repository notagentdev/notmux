//! GitProvider trait and implementations for local and remote git operations.

use okena_git::{DiffMode, DiffResult, FileDiffSummary, FileStatusRefresh, GraphRow, WorkingTreeStatus};
use std::path::Path;

/// Provides git data from either local git commands or a remote server.
pub trait GitProvider: Send + Sync + 'static {
    fn is_git_repo(&self) -> bool;
    fn get_diff(&self, mode: DiffMode, ignore_whitespace: bool) -> Result<DiffResult, String>;
    fn get_file_contents(&self, file_path: &str, mode: DiffMode) -> (Option<String>, Option<String>);
    fn get_diff_file_summary(&self) -> Vec<FileDiffSummary>;
    fn get_commit_graph(&self, count: usize, branch: Option<&str>) -> Vec<GraphRow>;
    fn list_branches(&self) -> Vec<String>;

    /// Local filesystem root of this repo, if the provider is local.
    /// Remote providers return `None` — callers should fall back to a full
    /// refresh when no local root is available.
    fn local_repo_root(&self) -> Option<&Path> {
        None
    }

    /// Per-file status refresh. Default implementation returns an empty
    /// vec (forces callers to treat as "no info" and fall back to full
    /// refresh). Local provider overrides with `git status -- <paths>`.
    fn get_file_statuses(&self, _rel_paths: &[String]) -> Vec<FileStatusRefresh> {
        Vec::new()
    }

    // ── Commit-tab operations ─────────────────────────────────────

    /// Get full working tree status (files + branch + ahead/behind).
    fn get_working_tree_status(&self) -> WorkingTreeStatus;
    /// Stage a single file.
    fn stage_file(&self, file_path: &str) -> Result<(), String>;
    /// Unstage a single file.
    fn unstage_file(&self, file_path: &str) -> Result<(), String>;
    /// Stage all changes.
    fn stage_all(&self) -> Result<(), String>;
    /// Unstage all changes.
    fn unstage_all(&self) -> Result<(), String>;
    /// Discard changes in a file (tracked: restore, untracked: delete).
    fn discard_file(&self, file_path: &str, is_untracked: bool) -> Result<(), String>;
    /// Create a commit with the given message.
    fn commit(&self, message: &str, amend: bool, signoff: bool) -> Result<(), String>;
    /// Undo the last commit, keep changes staged.
    fn uncommit(&self) -> Result<(), String>;
    /// Fetch from upstream.
    fn fetch(&self) -> Result<(), String>;
    /// Pull from upstream.
    fn pull(&self) -> Result<(), String>;
    /// Push current branch to upstream.
    fn push(&self) -> Result<(), String>;
}

/// Local git provider — wraps existing git functions.
pub struct LocalGitProvider {
    path: String,
}

impl LocalGitProvider {
    pub fn new(path: String) -> Self {
        Self { path }
    }
}

impl GitProvider for LocalGitProvider {
    fn is_git_repo(&self) -> bool {
        okena_git::is_git_repo(std::path::Path::new(&self.path))
    }

    fn get_diff(&self, mode: DiffMode, ignore_whitespace: bool) -> Result<DiffResult, String> {
        okena_git::get_diff_with_options(std::path::Path::new(&self.path), mode, ignore_whitespace)
    }

    fn get_file_contents(&self, file_path: &str, mode: DiffMode) -> (Option<String>, Option<String>) {
        okena_git::get_file_contents_for_diff(std::path::Path::new(&self.path), file_path, mode)
    }

    fn get_diff_file_summary(&self) -> Vec<FileDiffSummary> {
        okena_git::get_diff_file_summary(std::path::Path::new(&self.path))
    }

    fn get_commit_graph(&self, count: usize, branch: Option<&str>) -> Vec<GraphRow> {
        okena_git::get_commit_graph(std::path::Path::new(&self.path), count, branch)
    }

    fn list_branches(&self) -> Vec<String> {
        okena_git::list_branches(std::path::Path::new(&self.path))
    }

    fn get_working_tree_status(&self) -> WorkingTreeStatus {
        okena_git::get_working_tree_status(std::path::Path::new(&self.path))
    }

    fn local_repo_root(&self) -> Option<&Path> {
        Some(Path::new(&self.path))
    }

    fn get_file_statuses(&self, rel_paths: &[String]) -> Vec<FileStatusRefresh> {
        okena_git::get_file_statuses(std::path::Path::new(&self.path), rel_paths)
    }

    fn stage_file(&self, file_path: &str) -> Result<(), String> {
        okena_git::stage_file(std::path::Path::new(&self.path), file_path)
    }

    fn unstage_file(&self, file_path: &str) -> Result<(), String> {
        okena_git::unstage_file(std::path::Path::new(&self.path), file_path)
    }

    fn stage_all(&self) -> Result<(), String> {
        okena_git::stage_all(std::path::Path::new(&self.path))
    }

    fn unstage_all(&self) -> Result<(), String> {
        okena_git::unstage_all(std::path::Path::new(&self.path))
    }

    fn discard_file(&self, file_path: &str, is_untracked: bool) -> Result<(), String> {
        okena_git::discard_file(std::path::Path::new(&self.path), file_path, is_untracked)
    }

    fn commit(&self, message: &str, amend: bool, signoff: bool) -> Result<(), String> {
        okena_git::commit(std::path::Path::new(&self.path), message, amend, signoff)
    }

    fn uncommit(&self) -> Result<(), String> {
        okena_git::uncommit(std::path::Path::new(&self.path))
    }

    fn fetch(&self) -> Result<(), String> {
        okena_git::fetch_all(std::path::Path::new(&self.path))
    }

    fn pull(&self) -> Result<(), String> {
        okena_git::pull(std::path::Path::new(&self.path))
    }

    fn push(&self) -> Result<(), String> {
        let path = std::path::Path::new(&self.path);
        let branch = okena_git::get_current_branch(path)
            .ok_or_else(|| "No current branch (detached HEAD?)".to_string())?;
        okena_git::push_branch(path, &branch)
    }
}

/// Remote git provider — fetches git data via HTTP from a remote server.
pub struct RemoteGitProvider {
    host: String,
    port: u16,
    token: String,
    project_id: String,
}

impl RemoteGitProvider {
    pub fn new(host: String, port: u16, token: String, project_id: String) -> Self {
        Self { host, port, token, project_id }
    }

    fn post_action(&self, action: okena_core::api::ActionRequest) -> Result<Option<serde_json::Value>, String> {
        okena_core::remote_action::post_action(&self.host, self.port, &self.token, action)
    }
}

impl GitProvider for RemoteGitProvider {
    fn is_git_repo(&self) -> bool {
        true
    }

    fn get_diff(&self, mode: DiffMode, ignore_whitespace: bool) -> Result<DiffResult, String> {
        let action = okena_core::api::ActionRequest::GitDiff {
            project_id: self.project_id.clone(),
            mode,
            ignore_whitespace,
        };
        let result = self.post_action(action)?;
        match result {
            Some(value) => serde_json::from_value(value).map_err(|e| format!("Failed to deserialize DiffResult: {}", e)),
            None => Ok(DiffResult::default()),
        }
    }

    fn get_file_contents(&self, file_path: &str, mode: DiffMode) -> (Option<String>, Option<String>) {
        let action = okena_core::api::ActionRequest::GitFileContents {
            project_id: self.project_id.clone(),
            file_path: file_path.to_string(),
            mode,
        };
        match self.post_action(action) {
            Ok(Some(value)) => {
                let old = value.get("old_content").and_then(|v| v.as_str()).map(String::from);
                let new = value.get("new_content").and_then(|v| v.as_str()).map(String::from);
                (old, new)
            }
            _ => (None, None),
        }
    }

    fn get_diff_file_summary(&self) -> Vec<FileDiffSummary> {
        let action = okena_core::api::ActionRequest::GitDiffSummary {
            project_id: self.project_id.clone(),
        };
        match self.post_action(action) {
            Ok(Some(value)) => serde_json::from_value(value).unwrap_or_else(|e| {
                log::warn!("Failed to deserialize diff summary: {}", e);
                Vec::new()
            }),
            _ => Vec::new(),
        }
    }

    fn get_commit_graph(&self, count: usize, branch: Option<&str>) -> Vec<GraphRow> {
        let action = okena_core::api::ActionRequest::GitCommitGraph {
            project_id: self.project_id.clone(),
            count,
            branch: branch.map(String::from),
        };
        match self.post_action(action) {
            Ok(Some(value)) => serde_json::from_value(value).unwrap_or_else(|e| {
                log::warn!("Failed to deserialize commit graph: {}", e);
                Vec::new()
            }),
            _ => Vec::new(),
        }
    }

    fn list_branches(&self) -> Vec<String> {
        let action = okena_core::api::ActionRequest::GitListBranches {
            project_id: self.project_id.clone(),
        };
        match self.post_action(action) {
            Ok(Some(value)) => serde_json::from_value(value).unwrap_or_else(|e| {
                log::warn!("Failed to deserialize branch list: {}", e);
                Vec::new()
            }),
            _ => Vec::new(),
        }
    }

    fn get_working_tree_status(&self) -> WorkingTreeStatus {
        let action = okena_core::api::ActionRequest::GitWorkingTreeStatus {
            project_id: self.project_id.clone(),
        };
        match self.post_action(action) {
            Ok(Some(value)) => serde_json::from_value(value).unwrap_or_else(|e| {
                log::warn!("Failed to deserialize working tree status: {}", e);
                WorkingTreeStatus::default()
            }),
            _ => WorkingTreeStatus::default(),
        }
    }

    fn stage_file(&self, file_path: &str) -> Result<(), String> {
        let action = okena_core::api::ActionRequest::GitStageFile {
            project_id: self.project_id.clone(),
            file_path: file_path.to_string(),
        };
        self.post_action(action).map(|_| ())
    }

    fn unstage_file(&self, file_path: &str) -> Result<(), String> {
        let action = okena_core::api::ActionRequest::GitUnstageFile {
            project_id: self.project_id.clone(),
            file_path: file_path.to_string(),
        };
        self.post_action(action).map(|_| ())
    }

    fn stage_all(&self) -> Result<(), String> {
        let action = okena_core::api::ActionRequest::GitStageAll {
            project_id: self.project_id.clone(),
        };
        self.post_action(action).map(|_| ())
    }

    fn unstage_all(&self) -> Result<(), String> {
        let action = okena_core::api::ActionRequest::GitUnstageAll {
            project_id: self.project_id.clone(),
        };
        self.post_action(action).map(|_| ())
    }

    fn discard_file(&self, file_path: &str, is_untracked: bool) -> Result<(), String> {
        let action = okena_core::api::ActionRequest::GitDiscardFile {
            project_id: self.project_id.clone(),
            file_path: file_path.to_string(),
            is_untracked,
        };
        self.post_action(action).map(|_| ())
    }

    fn commit(&self, message: &str, amend: bool, signoff: bool) -> Result<(), String> {
        let action = okena_core::api::ActionRequest::GitCommit {
            project_id: self.project_id.clone(),
            message: message.to_string(),
            amend,
            signoff,
        };
        self.post_action(action).map(|_| ())
    }

    fn uncommit(&self) -> Result<(), String> {
        let action = okena_core::api::ActionRequest::GitUncommit {
            project_id: self.project_id.clone(),
        };
        self.post_action(action).map(|_| ())
    }

    fn fetch(&self) -> Result<(), String> {
        let action = okena_core::api::ActionRequest::GitFetch {
            project_id: self.project_id.clone(),
        };
        self.post_action(action).map(|_| ())
    }

    fn pull(&self) -> Result<(), String> {
        let action = okena_core::api::ActionRequest::GitPull {
            project_id: self.project_id.clone(),
        };
        self.post_action(action).map(|_| ())
    }

    fn push(&self) -> Result<(), String> {
        let action = okena_core::api::ActionRequest::GitPush {
            project_id: self.project_id.clone(),
        };
        self.post_action(action).map(|_| ())
    }
}
