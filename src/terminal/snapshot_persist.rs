use crate::settings::GlobalSettings;
use crate::workspace::state::GlobalWorkspace;
use gpui::App;
use std::collections::HashSet;
use notmux_terminal::terminal::{SnapshotReason, Terminal};

fn collect_terminal_keys(
    project_path: &str,
    node: &notmux_workspace::state::LayoutNode,
    path: &mut Vec<usize>,
    out: &mut Vec<(String, String, String, String)>,
) {
    match node {
        notmux_workspace::state::LayoutNode::Terminal {
            slot_id,
            terminal_id: Some(tid),
            ..
        } => {
            let key = notmux_terminal::scrollback_snapshot::snapshot_key(slot_id);
            out.push((project_path.to_string(), key, tid.clone(), slot_id.clone()));
        }
        notmux_workspace::state::LayoutNode::Terminal { .. }
        | notmux_workspace::state::LayoutNode::Editor { .. }
        | notmux_workspace::state::LayoutNode::Browser { .. } => {}
        notmux_workspace::state::LayoutNode::Split { children, .. }
        | notmux_workspace::state::LayoutNode::Tabs { children, .. } => {
            for (i, child) in children.iter().enumerate() {
                path.push(i);
                collect_terminal_keys(project_path, child, path, out);
                path.pop();
            }
        }
    }
}

fn collect_terminal_snapshot_keys(
    node: &notmux_workspace::state::LayoutNode,
    path: &mut Vec<usize>,
    out: &mut HashSet<String>,
) {
    match node {
        notmux_workspace::state::LayoutNode::Terminal { slot_id, .. } => {
            out.insert(notmux_terminal::scrollback_snapshot::snapshot_key(slot_id));
        }
        notmux_workspace::state::LayoutNode::Split { children, .. }
        | notmux_workspace::state::LayoutNode::Tabs { children, .. } => {
            for (i, child) in children.iter().enumerate() {
                path.push(i);
                collect_terminal_snapshot_keys(child, path, out);
                path.pop();
            }
        }
        notmux_workspace::state::LayoutNode::Editor { .. }
        | notmux_workspace::state::LayoutNode::Browser { .. } => {}
    }
}

pub fn save_terminal_snapshot(
    project_path: &str,
    key: &str,
    terminal: &Terminal,
    max_lines: u32,
    reason: SnapshotReason,
) -> bool {
    let dir = notmux_terminal::scrollback_snapshot::project_snapshot_dir(project_path);
    let cwd = terminal
        .shell_pid()
        .and_then(notmux_terminal::process::read_process_cwd);
    let (bytes, stats) =
        terminal.capture_scrollback_snapshot(&dir, key, max_lines, cwd.as_deref(), reason);

    log::info!(
        "Scrollback snapshot capture key={} reason={:?} size={} cols={} rows={} pending={} replay={} restored={} restored_from_snapshot={} had_input={} gen={} resize_stable={}",
        key,
        reason,
        bytes.len(),
        stats.cols,
        stats.rows,
        stats.pending_output_len,
        stats.replay_buffer_len,
        stats.restored_replay_buffer_len,
        stats.restored_from_snapshot,
        stats.had_user_input,
        stats.content_generation,
        stats.resize_stable,
    );

    if bytes.is_empty() {
        log::warn!(
            "Skipping empty scrollback snapshot for {} ({:?}); preserving existing snapshot",
            key,
            reason
        );
        return false;
    }

    match notmux_terminal::scrollback_snapshot::save(&dir, key, &bytes) {
        Ok(()) => {
            log::info!("Saved scrollback snapshot {} ({} bytes)", key, bytes.len());
            true
        }
        Err(e) => {
            log::warn!("Failed to persist scrollback for {}: {}", key, e);
            false
        }
    }
}

pub fn clear_slot_snapshot(project_path: &str, slot_id: &str) {
    let dir = notmux_terminal::scrollback_snapshot::project_snapshot_dir(project_path);
    let key = notmux_terminal::scrollback_snapshot::snapshot_key(slot_id);
    notmux_terminal::scrollback_snapshot::clear(&dir, &key);
    log::info!("Cleared scrollback snapshot for terminal slot {}", key);
}

pub fn purge_stale_snapshots(cx: &mut App) {
    let Some(gs) = cx.try_global::<GlobalSettings>() else {
        return;
    };
    let settings = gs.0.read(cx).get();
    if !settings.persist_scrollback || settings.persist_scrollback_lines == 0 {
        return;
    }

    let Some(gw) = cx.try_global::<GlobalWorkspace>() else {
        return;
    };

    let projects: Vec<(String, String, HashSet<String>)> = {
        let data = gw.0.read(cx).data().clone();
        let mut projects = Vec::new();
        for project in &data.projects {
            let mut keys = HashSet::new();
            if let Some(root) = project.layout.as_ref() {
                collect_terminal_snapshot_keys(root, &mut Vec::new(), &mut keys);
            }
            projects.push((project.id.clone(), project.path.clone(), keys));
        }
        projects
    };

    for (_project_id, project_path, live_keys) in projects {
        let dir = notmux_terminal::scrollback_snapshot::project_snapshot_dir(&project_path);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("snap") {
                continue;
            }
            let Some(key) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if live_keys.contains(key) {
                continue;
            }
            match std::fs::remove_file(&path) {
                Ok(()) => log::info!("Purged stale scrollback snapshot {}", key),
                Err(e) => log::warn!("Failed to purge stale scrollback snapshot {}: {}", key, e),
            }
        }
    }
}

fn persist_agent_running_state(slot_id: &str, terminal: &Terminal) {
    if let Err(e) = notmux_terminal::agent_sessions::set_was_running_at_quit(
        slot_id,
        terminal.has_running_child(),
    ) {
        log::warn!("Failed to persist agent running state for slot {}: {}", slot_id, e);
    }
}
pub fn save_all_snapshots(cx: &mut App, reason: SnapshotReason) {
    let Some(registry) = notmux_terminal::global_registry() else {
        return;
    };
    let Some(gw) = cx.try_global::<GlobalWorkspace>() else {
        return;
    };
    let pairs: Vec<(String, String, String, String)> = {
        let data = gw.0.read(cx).data().clone();
        let mut out: Vec<(String, String, String, String)> = Vec::new();
        for project in &data.projects {
            if let Some(root) = project.layout.as_ref() {
                collect_terminal_keys(&project.path, root, &mut Vec::new(), &mut out);
            }
        }
        out
    };
    let to_save = {
        let registry_map = registry.lock();
        let mut to_save = Vec::new();
        for (project_path, key, terminal_id, slot_id) in pairs {
            let Some(term) = registry_map.get(&terminal_id) else {
                continue;
            };
            to_save.push((project_path, key, slot_id, term.clone()));
        }
        to_save
    };
    for (_, _, slot_id, term) in &to_save {
        persist_agent_running_state(slot_id, term);
    }
    let Some(gs) = cx.try_global::<GlobalSettings>() else {
        return;
    };
    let settings = gs.0.read(cx).get();
    if !settings.persist_scrollback || settings.persist_scrollback_lines == 0 {
        log::info!(
            "Scrollback persistence skipped (enabled={}, lines={})",
            settings.persist_scrollback,
            settings.persist_scrollback_lines,
        );
        return;
    }
    let mut saved = 0usize;
    let mut considered = 0usize;
    for (project_path, key, _, term) in &to_save {
        considered += 1;
        if save_terminal_snapshot(
            project_path.as_str(),
            key.as_str(),
            term.as_ref(),
            settings.persist_scrollback_lines,
            reason,
        ) {
            saved += 1;
        }
    }
    log::info!(
        "Scrollback snapshot pass reason={:?} considered={} saved={}",
        reason,
        considered,
        saved
    );
    }
    #[cfg(all(test, unix))]
    mod tests {
    use super::*;
    use notmux_terminal::agent_sessions::{self, AgentSessionRecord};
    use notmux_terminal::terminal::{TerminalSize, TerminalTransport};
    use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

    struct NullTransport;
    impl TerminalTransport for NullTransport {
        fn send_input(&self, _terminal_id: &str, _data: &[u8]) {}
        fn resize(&self, _terminal_id: &str, _cols: u16, _rows: u16) {}
        fn uses_mouse_backend(&self) -> bool {
            false
        }
    }

    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    fn with_store(test: impl FnOnce()) {
        let _lock = env_lock();
        let dir = std::env::temp_dir().join(format!(
            "notmux-agent-running-snapshot-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let previous = std::env::var("NOTMUX_CONFIG_DIR").ok();
        unsafe { std::env::set_var("NOTMUX_CONFIG_DIR", &dir) };
        test();
        match previous {
            Some(value) => unsafe { std::env::set_var("NOTMUX_CONFIG_DIR", value) },
            None => unsafe { std::env::remove_var("NOTMUX_CONFIG_DIR") },
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn session() -> AgentSessionRecord {
        AgentSessionRecord {
            kind: "codex".to_string(),
            session_id: "session-1".to_string(),
            cwd: None,
            transcript_path: None,
            pid: Some(0),
            ended: false,
            was_running_at_quit: None,
            updated_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        }
    }

    fn terminal_with_shell_pid(pid: u32) -> Terminal {
        let terminal = Terminal::new(
            "terminal-1".to_string(),
            TerminalSize::default(),
            Arc::new(NullTransport),
            "/tmp".to_string(),
        );
        terminal.set_shell_pid(pid);
        terminal
    }

    #[test]
    fn snapshot_records_running_shell_child() {
        with_store(|| {
            agent_sessions::record("slot-1", session()).unwrap();
            let mut shell = std::process::Command::new("sh")
                .arg("-c")
                .arg("sleep 30 & wait")
                .spawn()
                .unwrap();
            let terminal = terminal_with_shell_pid(shell.id());
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while !terminal.has_running_child() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(terminal.has_running_child());
            persist_agent_running_state("slot-1", &terminal);
            let record = agent_sessions::get("slot-1").unwrap();
            assert_eq!(record.was_running_at_quit, Some(true));
            assert!(agent_sessions::is_restorable(&record));
            let _ = shell.kill();
            let _ = shell.wait();
        });
    }

    #[test]
    fn snapshot_records_idle_shell_and_does_not_create_records() {
        with_store(|| {
            agent_sessions::record("slot-1", session()).unwrap();
            let mut shell = std::process::Command::new("sh")
                .arg("-c")
                .arg("sleep 0.05")
                .spawn()
                .unwrap();
            shell.wait().unwrap();
            let terminal = terminal_with_shell_pid(shell.id());
            persist_agent_running_state("slot-1", &terminal);
            persist_agent_running_state("shell-only-slot", &terminal);
            let record = agent_sessions::get("slot-1").unwrap();
            assert_eq!(record.was_running_at_quit, Some(false));
            assert!(!agent_sessions::is_restorable(&record));
            assert!(agent_sessions::get("shell-only-slot").is_none());
        });
    }
    }
