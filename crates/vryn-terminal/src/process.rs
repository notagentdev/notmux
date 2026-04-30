pub use vryn_core::process::{command, safe_output};

/// Read the current working directory of a running process.
///
/// Used by the scrollback-snapshot path at quit time to capture where the
/// user's shell was so the next launch can revive the PTY in the same
/// directory. Returns `None` if the lookup fails for any reason
/// (process gone, permissions, platform unsupported).
///
/// - **Linux**: `readlink /proc/{pid}/cwd`
/// - **macOS**: `lsof -a -p {pid} -d cwd -Fn` (parses `n<path>` lines)
/// - **Windows**: not implemented
pub fn read_process_cwd(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let link = format!("/proc/{}/cwd", pid);
        std::fs::read_link(&link)
            .ok()
            .and_then(|p| p.to_str().map(|s| s.to_string()))
    }
    #[cfg(target_os = "macos")]
    {
        // `lsof -a -p PID -d cwd -Fn` outputs records terminated by NUL-style
        // field markers. The cwd path appears on a line beginning with `n`.
        let out = std::process::Command::new("lsof")
            .args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix('n') {
                let p = rest.trim();
                if !p.is_empty() {
                    return Some(p.to_string());
                }
            }
        }
        None
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}
