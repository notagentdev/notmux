use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Extract the archive and replace the binary, or the complete signed macOS app.
pub fn install_update(archive_path: &Path) -> Result<PathBuf> {
    let current_exe = std::env::current_exe().context("failed to get current exe path")?;

    let extract_dir = archive_path
        .parent()
        .context("archive has no parent dir")?
        .join("extracted");

    let _ = std::fs::remove_dir_all(&extract_dir);
    std::fs::create_dir_all(&extract_dir).context("failed to create extraction dir")?;

    extract_archive(archive_path, &extract_dir)?;

    #[cfg(target_os = "macos")]
    macos::install_app(&current_exe, &extract_dir)?;

    #[cfg(not(target_os = "macos"))]
    {
        let new_binary = find_binary(&extract_dir)?;
        validate_binary(&new_binary)?;
        replace_binary(&current_exe, &new_binary)?;
    }

    let _ = std::fs::remove_dir_all(&extract_dir);
    let _ = std::fs::remove_file(archive_path);

    Ok(current_exe)
}

/// Restart the application by spawning a new process and quitting.
pub fn restart_app(exe: &Path, cx: &mut gpui::App) {
    #[cfg(target_os = "macos")]
    let result = macos::app_bundle(exe).and_then(|app| {
        // Wait for the old process to release the instance lock before launching.
        crate::process::command("/bin/sh")
            .args(["-c", "while kill -0 \"$1\" 2>/dev/null; do sleep 0.1; done; exec /usr/bin/open -n \"$2\"", "notmux-restart"])
            .arg(std::process::id().to_string())
            .arg(app)
            .spawn()
            .context("failed to schedule app restart")
    });
    #[cfg(not(target_os = "macos"))]
    let result = crate::process::command(&exe.to_string_lossy())
        .args(std::env::args().skip(1))
        .spawn()
        .context("failed to restart binary");

    match result {
        Ok(_) => cx.quit(),
        Err(e) => log::error!("Failed to restart: {}", e),
    }
}

/// Remove leftover `.old` binary from a previous update.
pub fn cleanup_old_binary() {
    if let Ok(exe) = std::env::current_exe() {
        #[cfg(target_os = "macos")]
        if let Ok(app) = macos::app_bundle(&exe) {
            let _ = std::fs::remove_dir_all(app.with_extension("app.old"));
        }
        let old_path = exe.with_extension(if cfg!(windows) { "exe.old" } else { "old" });
        if old_path.exists() {
            match std::fs::remove_file(&old_path) {
                Ok(()) => log::info!("Cleaned up old binary: {:?}", old_path),
                Err(e) => log::warn!("Failed to clean up old binary {:?}: {}", old_path, e),
            }
        }
    }
}

fn validate_binary(binary: &Path) -> Result<()> {
    let mut child = crate::process::command(&binary.to_string_lossy())
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("failed to spawn binary for validation")?;

    let timeout = std::time::Duration::from_secs(10);
    let start = std::time::Instant::now();

    let status = loop {
        match child
            .try_wait()
            .context("failed to wait on validation process")?
        {
            Some(status) => break status,
            None => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    anyhow::bail!("binary validation timed out after {}s", timeout.as_secs());
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    };

    if status.success() {
        log::info!("Binary validation passed");
        Ok(())
    } else {
        anyhow::bail!("new binary failed validation (exit {})", status);
    }
}

fn extract_archive(archive: &Path, dest: &Path) -> Result<()> {
    let name = archive.file_name().unwrap_or_default().to_string_lossy();

    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        let status = crate::process::command("tar")
            .args([
                "xzf",
                &archive.to_string_lossy(),
                "-C",
                &dest.to_string_lossy(),
            ])
            .status()
            .context("failed to run tar")?;
        if !status.success() {
            anyhow::bail!("tar extraction failed with status {}", status);
        }
    } else if name.ends_with(".zip") {
        #[cfg(target_os = "macos")]
        {
            let status = crate::process::command("/usr/bin/ditto")
                .args(["-x", "-k"])
                .arg(archive)
                .arg(dest)
                .status()
                .context("failed to extract macOS app")?;
            anyhow::ensure!(status.success(), "ditto extraction failed");
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let status = crate::process::command("unzip")
                .args([
                    "-o",
                    &archive.to_string_lossy(),
                    "-d",
                    &dest.to_string_lossy(),
                ])
                .status()
                .context("failed to run unzip")?;
            if !status.success() {
                anyhow::bail!("unzip failed with status {}", status);
            }
        }
        #[cfg(windows)]
        {
            let status = crate::process::command("tar")
                .args([
                    "-xf",
                    &archive.to_string_lossy(),
                    "-C",
                    &dest.to_string_lossy(),
                ])
                .status()
                .context("failed to run tar on Windows")?;
            if !status.success() {
                anyhow::bail!("tar extraction failed with status {}", status);
            }
        }
    } else {
        anyhow::bail!("unknown archive format: {}", name);
    }

    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn find_binary(dir: &Path) -> Result<PathBuf> {
    #[cfg(unix)]
    let binary_name = "notmux";
    #[cfg(windows)]
    let binary_name = "notmux.exe";

    find_binary_recursive(dir, binary_name, 3)
        .with_context(|| format!("could not find '{}' in extracted archive", binary_name))
}

#[cfg(not(target_os = "macos"))]
fn find_binary_recursive(dir: &Path, name: &str, depth: u32) -> Result<PathBuf> {
    let direct = dir.join(name);
    if direct.exists() {
        return Ok(direct);
    }

    if depth == 0 {
        anyhow::bail!("search depth exhausted");
    }

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir()
                && let Ok(found) = find_binary_recursive(&path, name, depth - 1)
            {
                return Ok(found);
            }
        }
    }

    anyhow::bail!("not found at this level")
}

#[cfg(not(target_os = "macos"))]
fn replace_binary(current: &Path, new_binary: &Path) -> Result<()> {
    let target = current.to_path_buf();
    let old_path = target.with_extension(if cfg!(windows) { "exe.old" } else { "old" });

    let _ = std::fs::remove_file(&old_path);

    #[cfg(windows)]
    {
        let mut last_err = None;
        for _ in 0..5 {
            match std::fs::rename(&target, &old_path) {
                Ok(()) => {
                    last_err = None;
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
            }
        }
        if let Some(e) = last_err {
            anyhow::bail!(
                "failed to rename current binary (file may be locked by antivirus): {}",
                e
            );
        }
    }

    #[cfg(not(windows))]
    std::fs::rename(&target, &old_path).context("failed to rename current binary")?;

    if let Err(e) = std::fs::copy(new_binary, &target) {
        log::error!("Failed to copy new binary, rolling back: {}", e);
        let _ = std::fs::rename(&old_path, &target);
        return Err(e).context("failed to copy new binary");
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)) {
            log::error!("Failed to set permissions, rolling back: {}", e);
            let _ = std::fs::remove_file(&target);
            let _ = std::fs::rename(&old_path, &target);
            return Err(e).context("failed to set executable permission");
        }
    }

    log::info!("Replaced binary at {:?}", target);
    Ok(())
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;

    pub(super) fn app_bundle(exe: &Path) -> Result<&Path> {
        let macos = exe.parent().context("missing executable directory")?;
        let contents = macos.parent().context("missing Contents directory")?;
        let app = contents.parent().context("missing app bundle")?;
        anyhow::ensure!(macos.file_name().is_some_and(|n| n == "MacOS")
            && contents.file_name().is_some_and(|n| n == "Contents")
            && app.extension().is_some_and(|e| e == "app"),
            "macOS self-update requires an installed .app; install the signed release first");
        Ok(app)
    }

    fn signing_team(app: &Path) -> Result<String> {
        let output = crate::process::command("/usr/bin/codesign")
            .args(["--display", "--verbose=4"])
            .arg(app)
            .output()
            .context("failed to inspect signing identity")?;
        anyhow::ensure!(output.status.success(), "app has no signing identity");
        let details = String::from_utf8_lossy(&output.stderr);
        anyhow::ensure!(details.lines().any(|l| l == "Identifier=dev.notmux.app"),
            "unexpected signed app identifier");
        details.lines().find_map(|l| l.strip_prefix("TeamIdentifier="))
            .filter(|team| !team.is_empty() && *team != "not set")
            .map(str::to_owned)
            .context("app is not Developer ID signed; install the signed release first")
    }

    fn verify_app(app: &Path, expected_team: &str) -> Result<()> {
        anyhow::ensure!(signing_team(app)? == expected_team, "update signing team differs from installed app");
        let status = crate::process::command("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(app).status().context("failed to verify app signature")?;
        anyhow::ensure!(status.success(), "invalid update signature");
        let status = crate::process::command("/usr/sbin/spctl")
            .args(["--assess", "--type", "execute"])
            .arg(app).status().context("failed to assess notarization")?;
        anyhow::ensure!(status.success(), "update rejected by Gatekeeper");
        Ok(())
    }

    pub(super) fn install_app(exe: &Path, extract_dir: &Path) -> Result<()> {
        let current = app_bundle(exe)?;
        let team = signing_team(current)?;
        let source = extract_dir.join("NotMux.app");
        verify_app(&source, &team)?;
        // Stage on the same filesystem so replacing the bundle uses renames, not a merge.
        let staging = current.parent().context("missing app parent")?
            .join(format!(".notmux-update-{}", std::process::id()));
        std::fs::create_dir(&staging).context("cannot stage update next to app")?;
        let staged_app = staging.join("NotMux.app");
        let result = (|| {
            let status = crate::process::command("/usr/bin/ditto")
                .arg(&source).arg(&staged_app).status()
                .context("failed to stage app bundle")?;
            anyhow::ensure!(status.success(), "failed to copy app bundle");
            verify_app(&staged_app, &team)?;
            validate_binary(&staged_app.join("Contents/MacOS/notmux"))?;
            replace_bundle(current, &staged_app)
        })();
        let _ = std::fs::remove_dir_all(&staging);
        result
    }

    fn replace_bundle(current: &Path, staged: &Path) -> Result<()> {
        let old = current.with_extension("app.old");
        if old.exists() {
            std::fs::remove_dir_all(&old).context("cannot remove previous app backup")?;
        }
        std::fs::rename(current, &old).context("cannot move installed app to backup")?;
        if let Err(error) = std::fs::rename(staged, current) {
            std::fs::rename(&old, current)
                .with_context(|| format!("update failed ({error}); restore app manually from {}", old.display()))?;
            return Err(error).context("app replacement failed; previous app restored");
        }
        // The running process may still use old resources until it quits.
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn app_paths_require_the_standard_bundle_layout() {
            let path = Path::new("/Applications/NotMux.app/Contents/MacOS/notmux");
            assert_eq!(app_bundle(path).unwrap(), Path::new("/Applications/NotMux.app"));
            assert!(app_bundle(Path::new("/tmp/notmux")).is_err());
            assert!(app_bundle(Path::new("/tmp/NotMux.app/Other/MacOS/notmux")).is_err());
        }

        #[test]
        fn unsigned_bundle_is_rejected() {
            let root = std::env::temp_dir().join(format!("notmux-unsigned-test-{}", std::process::id()));
            let app = root.join("NotMux.app");
            std::fs::create_dir_all(&app).unwrap();
            assert!(verify_app(&app, "EXPECTED_TEAM").is_err());
            assert!(app.is_dir());
            std::fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn bundle_replacement_preserves_resources_and_rolls_back_on_failure() {
            let root = std::env::temp_dir().join(format!("notmux-bundle-test-{}", std::process::id()));
            std::fs::create_dir(&root).unwrap();
            let current = root.join("NotMux.app");
            let staged = root.join("new.app");
            for (app, version) in [(&current, "old"), (&staged, "new")] {
                std::fs::create_dir_all(app.join("Contents/Resources")).unwrap();
                std::fs::write(app.join("Contents/Resources/version"), version).unwrap();
            }
            replace_bundle(&current, &staged).unwrap();
            assert_eq!(std::fs::read_to_string(current.join("Contents/Resources/version")).unwrap(), "new");
            assert_eq!(std::fs::read_to_string(current.with_extension("app.old").join("Contents/Resources/version")).unwrap(), "old");
            assert!(replace_bundle(&current, &root.join("missing.app")).is_err());
            assert_eq!(std::fs::read_to_string(current.join("Contents/Resources/version")).unwrap(), "new");
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}
