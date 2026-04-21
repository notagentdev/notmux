//! Filesystem operations used by the sidebar file explorer context menu.
//!
//! Each operation is synchronous and expects to be run from a blocking
//! context (callers typically wrap in `smol::unblock(...)`). Errors are
//! surfaced as `String` for easy propagation to toast notifications.

use std::path::{Path, PathBuf};

/// Create an empty file at `path`. Fails if the path already exists.
pub fn create_file(path: &Path) -> Result<(), String> {
    if path.exists() {
        return Err(format!("File already exists: {}", path.display()));
    }
    std::fs::File::create(path)
        .map(|_| ())
        .map_err(|e| format!("Failed to create file: {}", e))
}

/// Create a directory (including any missing parents). Fails if the path
/// already exists.
pub fn create_folder(path: &Path) -> Result<(), String> {
    if path.exists() {
        return Err(format!("Folder already exists: {}", path.display()));
    }
    std::fs::create_dir_all(path).map_err(|e| format!("Failed to create folder: {}", e))
}

/// Rename `old` → `new`. Fails if `old` doesn't exist or `new` already exists.
pub fn rename(old: &Path, new: &Path) -> Result<(), String> {
    if !old.exists() {
        return Err(format!("Path does not exist: {}", old.display()));
    }
    if new.exists() {
        return Err(format!("Target path already exists: {}", new.display()));
    }
    std::fs::rename(old, new).map_err(|e| format!("Failed to rename: {}", e))
}

/// Delete a file or directory (recursive for directories).
pub fn delete(path: &Path, is_dir: bool) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("Path does not exist: {}", path.display()));
    }
    if is_dir {
        std::fs::remove_dir_all(path).map_err(|e| format!("Failed to delete directory: {}", e))
    } else {
        std::fs::remove_file(path).map_err(|e| format!("Failed to delete file: {}", e))
    }
}

/// Copy `src` → `dst`, resolving collisions via `unique_path`. Returns the
/// actual destination path used. Works recursively for directories.
pub fn copy(src: &Path, dst: &Path) -> Result<PathBuf, String> {
    if !src.exists() {
        return Err(format!("Source does not exist: {}", src.display()));
    }
    let dst = unique_path(dst);
    if src.is_dir() {
        copy_dir_recursive(src, &dst)?;
    } else {
        std::fs::copy(src, &dst).map_err(|e| format!("Failed to copy file: {}", e))?;
    }
    Ok(dst)
}

/// Move `src` → `dst`, resolving collisions via `unique_path`. Tries
/// `fs::rename` first (fast path, same filesystem) and falls back to
/// copy+delete for cross-filesystem moves. Returns the actual destination
/// path used.
pub fn move_to(src: &Path, dst: &Path) -> Result<PathBuf, String> {
    if !src.exists() {
        return Err(format!("Source does not exist: {}", src.display()));
    }
    let dst = unique_path(dst);

    if std::fs::rename(src, &dst).is_ok() {
        return Ok(dst);
    }

    if src.is_dir() {
        copy_dir_recursive(src, &dst)?;
        std::fs::remove_dir_all(src)
            .map_err(|e| format!("Failed to remove source directory: {}", e))?;
    } else {
        std::fs::copy(src, &dst).map_err(|e| format!("Failed to copy file: {}", e))?;
        std::fs::remove_file(src).map_err(|e| format!("Failed to remove source file: {}", e))?;
    }
    Ok(dst)
}

/// Open the platform file manager and highlight `path`.
pub fn reveal_in_file_manager(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("Path does not exist: {}", path.display()));
    }

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn()
            .map_err(|e| format!("Failed to reveal in Finder: {}", e))?;
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn()
            .map_err(|e| format!("Failed to reveal in Explorer: {}", e))?;
    }

    #[cfg(target_os = "linux")]
    {
        let parent = path.parent().unwrap_or(path);
        std::process::Command::new("xdg-open")
            .arg(parent)
            .spawn()
            .map_err(|e| format!("Failed to open file manager: {}", e))?;
    }

    Ok(())
}

/// If `path` does not exist, returns it unchanged. Otherwise returns a
/// sibling path with a numeric suffix (`name (1).ext`, `name (2).ext`, ...)
/// that does not yet exist. Falls back to a timestamp suffix after 999
/// attempts.
pub fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let ext = path.extension().and_then(|s| s.to_str());

    for i in 1..1000 {
        let new_name = match ext {
            Some(e) => format!("{} ({}).{}", stem, i, e),
            None => format!("{} ({})", stem, i),
        };
        let candidate = parent.join(new_name);
        if !candidate.exists() {
            return candidate;
        }
    }

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let new_name = match ext {
        Some(e) => format!("{}_{}.{}", stem, ts, e),
        None => format!("{}_{}", stem, ts),
    };
    parent.join(new_name)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst)
        .map_err(|e| format!("Failed to create destination directory: {}", e))?;
    let read = std::fs::read_dir(src)
        .map_err(|e| format!("Failed to read source directory: {}", e))?;
    for entry in read.flatten() {
        let ft = entry
            .file_type()
            .map_err(|e| format!("Failed to stat entry: {}", e))?;
        let src_entry = entry.path();
        let dst_entry = dst.join(entry.file_name());
        if ft.is_dir() {
            copy_dir_recursive(&src_entry, &dst_entry)?;
        } else {
            std::fs::copy(&src_entry, &dst_entry)
                .map_err(|e| format!("Failed to copy {}: {}", src_entry.display(), e))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmpdir() -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!(
            "okena-fs-ops-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn unique_path_no_collision() {
        let dir = tmpdir();
        let p = dir.join("foo.txt");
        assert_eq!(unique_path(&p), p);
    }

    #[test]
    fn unique_path_with_extension() {
        let dir = tmpdir();
        let p = dir.join("foo.txt");
        fs::write(&p, "").unwrap();
        let u = unique_path(&p);
        assert_eq!(u.file_name().unwrap(), "foo (1).txt");
    }

    #[test]
    fn unique_path_without_extension() {
        let dir = tmpdir();
        let p = dir.join("README");
        fs::write(&p, "").unwrap();
        let u = unique_path(&p);
        assert_eq!(u.file_name().unwrap(), "README (1)");
    }

    #[test]
    fn unique_path_multiple_collisions() {
        let dir = tmpdir();
        let p = dir.join("a.md");
        fs::write(&p, "").unwrap();
        fs::write(dir.join("a (1).md"), "").unwrap();
        fs::write(dir.join("a (2).md"), "").unwrap();
        let u = unique_path(&p);
        assert_eq!(u.file_name().unwrap(), "a (3).md");
    }

    #[test]
    fn create_and_delete_file() {
        let dir = tmpdir();
        let p = dir.join("x.txt");
        create_file(&p).unwrap();
        assert!(p.exists());
        assert!(create_file(&p).is_err());
        delete(&p, false).unwrap();
        assert!(!p.exists());
    }

    #[test]
    fn create_and_delete_folder() {
        let dir = tmpdir();
        let p = dir.join("sub/nested");
        create_folder(&p).unwrap();
        assert!(p.exists());
        delete(&dir.join("sub"), true).unwrap();
        assert!(!dir.join("sub").exists());
    }

    #[test]
    fn rename_file() {
        let dir = tmpdir();
        let a = dir.join("a.txt");
        let b = dir.join("b.txt");
        fs::write(&a, "hi").unwrap();
        rename(&a, &b).unwrap();
        assert!(!a.exists());
        assert_eq!(fs::read_to_string(&b).unwrap(), "hi");
    }

    #[test]
    fn rename_fails_on_target_exists() {
        let dir = tmpdir();
        let a = dir.join("a.txt");
        let b = dir.join("b.txt");
        fs::write(&a, "").unwrap();
        fs::write(&b, "").unwrap();
        assert!(rename(&a, &b).is_err());
    }

    #[test]
    fn copy_file_with_collision() {
        let dir = tmpdir();
        let a = dir.join("a.txt");
        fs::write(&a, "hi").unwrap();
        let dst = dir.join("a.txt");
        let used = copy(&a, &dst).unwrap();
        assert_eq!(used.file_name().unwrap(), "a (1).txt");
        assert_eq!(fs::read_to_string(&used).unwrap(), "hi");
    }

    #[test]
    fn copy_directory_recursive() {
        let dir = tmpdir();
        let src = dir.join("src");
        fs::create_dir_all(src.join("inner")).unwrap();
        fs::write(src.join("inner/file.txt"), "x").unwrap();
        let dst = dir.join("dst");
        let used = copy(&src, &dst).unwrap();
        assert_eq!(used, dst);
        assert_eq!(fs::read_to_string(dst.join("inner/file.txt")).unwrap(), "x");
    }

    #[test]
    fn move_to_renames_same_filesystem() {
        let dir = tmpdir();
        let a = dir.join("a.txt");
        fs::write(&a, "yo").unwrap();
        let b = dir.join("sub/b.txt");
        fs::create_dir_all(dir.join("sub")).unwrap();
        let used = move_to(&a, &b).unwrap();
        assert_eq!(used, b);
        assert!(!a.exists());
        assert_eq!(fs::read_to_string(&b).unwrap(), "yo");
    }
}
