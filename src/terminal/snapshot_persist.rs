use crate::settings::GlobalSettings;
use crate::workspace::state::GlobalWorkspace;
use gpui::App;
use std::collections::HashSet;
use vryn_terminal::terminal::{SnapshotReason, Terminal};

fn collect_terminal_keys(
    project_path: &str,
    node: &vryn_workspace::state::LayoutNode,
    path: &mut Vec<usize>,
    out: &mut Vec<(String, String, String)>,
) {
    match node {
        vryn_workspace::state::LayoutNode::Terminal {
            slot_id,
            terminal_id: Some(tid),
            ..
        } => {
            let key = vryn_terminal::scrollback_snapshot::snapshot_key(slot_id);
            out.push((project_path.to_string(), key, tid.clone()));
        }
        vryn_workspace::state::LayoutNode::Terminal { .. } => {}
        vryn_workspace::state::LayoutNode::Split { children, .. }
        | vryn_workspace::state::LayoutNode::Tabs { children, .. } => {
            for (i, child) in children.iter().enumerate() {
                path.push(i);
                collect_terminal_keys(project_path, child, path, out);
                path.pop();
            }
        }
    }
}

fn collect_terminal_snapshot_keys(
    node: &vryn_workspace::state::LayoutNode,
    path: &mut Vec<usize>,
    out: &mut HashSet<String>,
) {
    match node {
        vryn_workspace::state::LayoutNode::Terminal { slot_id, .. } => {
            out.insert(vryn_terminal::scrollback_snapshot::snapshot_key(slot_id));
        }
        vryn_workspace::state::LayoutNode::Split { children, .. }
        | vryn_workspace::state::LayoutNode::Tabs { children, .. } => {
            for (i, child) in children.iter().enumerate() {
                path.push(i);
                collect_terminal_snapshot_keys(child, path, out);
                path.pop();
            }
        }
    }
}

pub fn save_terminal_snapshot(
    project_path: &str,
    key: &str,
    terminal: &Terminal,
    max_lines: u32,
    reason: SnapshotReason,
) -> bool {
    let dir = vryn_terminal::scrollback_snapshot::project_snapshot_dir(project_path);
    let cwd = terminal
        .shell_pid()
        .and_then(vryn_terminal::process::read_process_cwd);
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

    match vryn_terminal::scrollback_snapshot::save(&dir, key, &bytes) {
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
    let dir = vryn_terminal::scrollback_snapshot::project_snapshot_dir(project_path);
    let key = vryn_terminal::scrollback_snapshot::snapshot_key(slot_id);
    vryn_terminal::scrollback_snapshot::clear(&dir, &key);
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
        let dir = vryn_terminal::scrollback_snapshot::project_snapshot_dir(&project_path);
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

pub fn save_all_snapshots(cx: &mut App, reason: SnapshotReason) {
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

    let Some(registry) = vryn_terminal::global_registry() else {
        return;
    };
    let Some(gw) = cx.try_global::<GlobalWorkspace>() else {
        return;
    };

    let pairs: Vec<(String, String, String)> = {
        let data = gw.0.read(cx).data().clone();
        let mut out: Vec<(String, String, String)> = Vec::new();
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
        for (project_path, key, terminal_id) in pairs {
            let Some(term) = registry_map.get(&terminal_id) else {
                continue;
            };
            to_save.push((project_path, key, term.clone()));
        }
        to_save
    };

    let mut saved = 0usize;
    let mut considered = 0usize;
    for (project_path, key, term) in &to_save {
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
