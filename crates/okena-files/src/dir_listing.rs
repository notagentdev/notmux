//! Single-level directory listing for the workspace file explorer.
//!
//! Unlike `file_search::scan_files` (which recurses through the whole
//! project respecting `.gitignore`), `list_directory` reads a single
//! directory level so the explorer can lazy-load children on expand.

use std::path::{Path, PathBuf};

/// One entry (file or directory) returned from `list_directory`.
#[derive(Clone, Debug)]
pub struct DirEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: Option<u64>,
}

/// Read one level of `dir`. Returns entries sorted directories-first,
/// then alphabetically (case-insensitive). Dotfiles are included when
/// `show_hidden` is true; `.git` is always excluded regardless to avoid
/// noise in the tree view.
pub fn list_directory(dir: &Path, show_hidden: bool) -> Vec<DirEntry> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut entries: Vec<DirEntry> = read
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if name == ".git" {
                return None;
            }
            if !show_hidden && name.starts_with('.') {
                return None;
            }
            let ft = e.file_type().ok()?;
            let is_dir = ft.is_dir();
            let size = if is_dir {
                None
            } else {
                e.metadata().ok().map(|m| m.len())
            };
            Some(DirEntry {
                name,
                path: e.path(),
                is_dir,
                size,
            })
        })
        .collect();

    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    entries
}
