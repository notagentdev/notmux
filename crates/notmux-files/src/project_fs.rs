//! ProjectFs trait and implementations for local and remote file operations.

use crate::content_search::{ContentSearchConfig, FileSearchResult, SearchMode};
use crate::file_search::FileEntry;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

/// Provides file system operations from either local disk or a remote server.
pub trait ProjectFs: Send + Sync + 'static {
    /// List files in the project (for file search dialog).
    fn list_files(&self, show_ignored: bool, show_hidden: bool) -> Vec<FileEntry>;

    /// Read file content as UTF-8 string.
    fn read_file(&self, relative_path: &str) -> Result<String, String>;

    /// Write UTF-8 file content.
    fn write_file(&self, relative_path: &str, content: &str) -> Result<(), String>;

    /// Get file size in bytes.
    fn file_size(&self, relative_path: &str) -> Result<u64, String>;

    /// Search content across project files.
    fn search_content(
        &self,
        query: &str,
        config: &ContentSearchConfig,
        cancelled: &AtomicBool,
        on_result: &mut dyn FnMut(FileSearchResult),
    );

    /// Project display name (directory name).
    fn project_name(&self) -> String;

    /// Unique project identifier (used for caching).
    fn project_id(&self) -> String;

    /// File content at git `HEAD`, used as the baseline for the diff editor.
    /// Returns `None` when the file is untracked, the project is not a git
    /// repo, or the backend does not support it. Default: unsupported.
    fn file_at_head(&self, _relative_path: &str) -> Option<String> {
        None
    }
}

/// Local file system provider — delegates to existing functions.
pub struct LocalProjectFs {
    path: PathBuf,
}

impl LocalProjectFs {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl ProjectFs for LocalProjectFs {
    fn list_files(&self, show_ignored: bool, show_hidden: bool) -> Vec<FileEntry> {
        crate::file_search::FileSearchDialog::scan_files(&self.path, show_ignored, show_hidden)
    }

    fn read_file(&self, relative_path: &str) -> Result<String, String> {
        let full = self.path.join(relative_path);
        std::fs::read_to_string(&full).map_err(|e| format!("Cannot read file: {}", e))
    }

    fn write_file(&self, relative_path: &str, content: &str) -> Result<(), String> {
        let full = self.path.join(relative_path);
        std::fs::write(&full, content).map_err(|e| format!("Cannot write file: {}", e))
    }

    fn file_size(&self, relative_path: &str) -> Result<u64, String> {
        let full = self.path.join(relative_path);
        std::fs::metadata(&full)
            .map(|m| m.len())
            .map_err(|e| format!("Cannot read file: {}", e))
    }

    fn file_at_head(&self, relative_path: &str) -> Option<String> {
        // `HEAD:./<path>` resolves relative to the -C directory (the project
        // path), so this works even when the repo root differs from cwd.
        let output = notmux_core::process::command("git")
            .arg("-C")
            .arg(&self.path)
            .arg("show")
            .arg(format!("HEAD:./{relative_path}"))
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn search_content(
        &self,
        query: &str,
        config: &ContentSearchConfig,
        cancelled: &AtomicBool,
        on_result: &mut dyn FnMut(FileSearchResult),
    ) {
        crate::content_search::search_content(&self.path, query, config, cancelled, on_result);
    }

    fn project_name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "Project".to_string())
    }

    fn project_id(&self) -> String {
        self.path.to_string_lossy().to_string()
    }
}

/// Remote file system provider — fetches data via HTTP from a remote server.
pub struct RemoteProjectFs {
    host: String,
    port: u16,
    token: String,
    project_id: String,
    project_name: String,
}

impl RemoteProjectFs {
    pub fn new(
        host: String,
        port: u16,
        token: String,
        project_id: String,
        project_name: String,
    ) -> Self {
        Self {
            host,
            port,
            token,
            project_id,
            project_name,
        }
    }

    fn post_action(
        &self,
        action: notmux_core::api::ActionRequest,
    ) -> Result<Option<serde_json::Value>, String> {
        notmux_core::remote_action::post_action(&self.host, self.port, &self.token, action)
    }
}

impl ProjectFs for RemoteProjectFs {
    fn list_files(&self, show_ignored: bool, show_hidden: bool) -> Vec<FileEntry> {
        let action = notmux_core::api::ActionRequest::ListFiles {
            project_id: self.project_id.clone(),
            show_ignored,
            show_hidden,
        };
        match self.post_action(action) {
            Ok(Some(value)) => serde_json::from_value(value).unwrap_or_else(|e| {
                log::warn!("Failed to deserialize file list: {}", e);
                Vec::new()
            }),
            _ => Vec::new(),
        }
    }

    fn read_file(&self, relative_path: &str) -> Result<String, String> {
        let action = notmux_core::api::ActionRequest::ReadFile {
            project_id: self.project_id.clone(),
            relative_path: relative_path.to_string(),
        };
        match self.post_action(action)? {
            Some(value) => value
                .get("content")
                .and_then(|v| v.as_str())
                .map(String::from)
                .ok_or_else(|| "Missing content in response".to_string()),
            None => Err("Empty response".to_string()),
        }
    }

    fn write_file(&self, _relative_path: &str, _content: &str) -> Result<(), String> {
        Err("Saving remote files is not supported yet".to_string())
    }

    fn file_size(&self, relative_path: &str) -> Result<u64, String> {
        let action = notmux_core::api::ActionRequest::FileSize {
            project_id: self.project_id.clone(),
            relative_path: relative_path.to_string(),
        };
        match self.post_action(action)? {
            Some(value) => value
                .get("size")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| "Missing size in response".to_string()),
            None => Err("Empty response".to_string()),
        }
    }

    fn search_content(
        &self,
        query: &str,
        config: &ContentSearchConfig,
        cancelled: &AtomicBool,
        on_result: &mut dyn FnMut(FileSearchResult),
    ) {
        let mode = match config.mode {
            SearchMode::Literal => "literal",
            SearchMode::Regex => "regex",
            SearchMode::Fuzzy => "fuzzy",
        };
        let action = notmux_core::api::ActionRequest::SearchContent {
            project_id: self.project_id.clone(),
            query: query.to_string(),
            case_sensitive: config.case_sensitive,
            mode: mode.to_string(),
            max_results: config.max_results,
            file_glob: config.file_glob.clone(),
            context_lines: config.context_lines,
        };
        if let Ok(Some(value)) = self.post_action(action) {
            let results: Vec<FileSearchResult> =
                serde_json::from_value(value).unwrap_or_else(|e| {
                    log::warn!("Failed to deserialize search results: {}", e);
                    Vec::new()
                });
            for result in results {
                if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                on_result(result);
            }
        }
    }

    fn project_name(&self) -> String {
        self.project_name.clone()
    }

    fn project_id(&self) -> String {
        self.project_id.clone()
    }
}
