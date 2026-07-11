//! Cross-platform native OS notifications.
//!
//! Posts fire-and-forget system notifications through each platform's own
//! mechanism: `osascript` on macOS, `notify-send` on Linux/BSD, and a WinRT
//! toast via PowerShell on Windows. No extra runtime dependencies; a failure
//! to post is logged and never blocks the caller.

use notmux_core::process::command;

/// Longest title/body forwarded to the OS — notification centers truncate
/// anyway, and terminal-derived payloads can be arbitrarily large.
const MAX_TEXT_LEN: usize = 400;

/// Strip control characters (a body extracted from PTY output can contain
/// stray escape bytes) and cap the length on a char boundary.
fn sanitize(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(MAX_TEXT_LEN)
        .collect()
}

/// Escape a string for embedding in a double-quoted AppleScript literal.
#[cfg(any(test, target_os = "macos"))]
fn escape_applescript(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Escape a string for embedding in a single-quoted PowerShell literal.
#[cfg(any(test, windows))]
fn escape_powershell_single(text: &str) -> String {
    text.replace('\'', "''")
}

/// Spawn the notification command detached, reaping the child off-thread so
/// no zombie processes accumulate.
fn spawn_reaped(mut cmd: std::process::Command) {
    match cmd
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => log::warn!("Failed to post native notification: {}", e),
    }
}

/// Post a native OS notification. Fire-and-forget: spawns the platform
/// command and returns immediately.
pub fn post(title: &str, body: &str) {
    let title = sanitize(title);
    let body = sanitize(body);
    if title.is_empty() && body.is_empty() {
        return;
    }
    let title = if title.is_empty() {
        "NotMux".to_string()
    } else {
        title
    };
    post_platform(&title, &body);
}

#[cfg(target_os = "macos")]
fn post_platform(title: &str, body: &str) {
    let script = format!(
        "display notification \"{}\" with title \"{}\"",
        escape_applescript(body),
        escape_applescript(title),
    );
    let mut cmd = command("osascript");
    cmd.args(["-e", &script]);
    spawn_reaped(cmd);
}

#[cfg(all(unix, not(target_os = "macos")))]
fn post_platform(title: &str, body: &str) {
    let mut cmd = command("notify-send");
    // `--` terminates option parsing so a title starting with `-` can't be
    // misread as a flag.
    cmd.args(["--app-name", "NotMux", "--", title, body]);
    spawn_reaped(cmd);
}

#[cfg(windows)]
fn post_platform(title: &str, body: &str) {
    let script = format!(
        "$null = [Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime];\
         $xml = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02);\
         $texts = $xml.GetElementsByTagName('text');\
         $null = $texts.Item(0).AppendChild($xml.CreateTextNode('{}'));\
         $null = $texts.Item(1).AppendChild($xml.CreateTextNode('{}'));\
         $toast = [Windows.UI.Notifications.ToastNotification]::new($xml);\
         [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('NotMux').Show($toast);",
        escape_powershell_single(title),
        escape_powershell_single(body),
    );
    let mut cmd = command("powershell");
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-WindowStyle",
        "Hidden",
        "-Command",
        &script,
    ]);
    spawn_reaped(cmd);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_control_chars() {
        assert_eq!(sanitize("a\x1b[31mb\x07c\r\nd"), "a[31mbcd");
    }

    #[test]
    fn sanitize_truncates_on_char_boundary() {
        let long = "ä".repeat(MAX_TEXT_LEN + 50);
        let out = sanitize(&long);
        assert_eq!(out.chars().count(), MAX_TEXT_LEN);
    }

    #[test]
    fn applescript_escapes_quotes_and_backslashes() {
        assert_eq!(
            escape_applescript(r#"say "hi" \ done"#),
            r#"say \"hi\" \\ done"#
        );
    }

    #[test]
    fn powershell_escapes_single_quotes() {
        assert_eq!(escape_powershell_single("it's done"), "it''s done");
    }
}
