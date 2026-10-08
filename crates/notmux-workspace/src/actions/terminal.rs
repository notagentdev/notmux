//! Terminal-specific workspace actions
//!
//! Actions for managing individual terminals within projects.

use crate::state::{LayoutNode, Workspace};
use gpui::*;
use notmux_layout::GRID_MAX_COLS;
use notmux_terminal::shell_config::ShellType;

impl Workspace {
    /// Insert a pane for a managed agent run: the slot and terminal IDs are
    /// already allocated and the PTY already exists, so the pane adopts it
    /// instead of spawning a shell. The leaf joins the project's grid; focus
    /// is left where it was. Returns the leaf's layout path.
    pub fn insert_managed_terminal(
        &mut self,
        project_id: &str,
        slot_id: &str,
        terminal_id: &str,
        run_id: &str,
        name: &str,
        cx: &mut Context<Self>,
    ) -> Result<Vec<usize>, String> {
        if self.is_managed_slot(slot_id) {
            return Err(format!("slot {slot_id} is already managed"));
        }
        let project = self
            .project_mut(project_id)
            .ok_or_else(|| format!("project {project_id} not found"))?;
        if project.is_remote {
            return Err("managed panes cannot open in a remote project".to_string());
        }
        let leaf = LayoutNode::new_terminal_with_ids(slot_id, terminal_id);
        match project.layout.as_mut() {
            Some(layout) => layout.append_leaf_grid(leaf, GRID_MAX_COLS),
            None => project.layout = Some(leaf),
        }
        project
            .managed_runs
            .insert(slot_id.to_string(), run_id.to_string());
        if !name.trim().is_empty() {
            project
                .terminal_names
                .insert(terminal_id.to_string(), name.to_string());
        }
        let path = project
            .layout
            .as_ref()
            .and_then(|l| l.find_path_by_slot_id(slot_id))
            .ok_or_else(|| "inserted pane not found in layout".to_string())?;
        self.notify_data(cx);
        Ok(path)
    }

    /// Remove a managed pane (launch failure or explicit close). Returns
    /// the terminal ID the leaf carried, if any.
    pub fn remove_managed_terminal(
        &mut self,
        project_id: &str,
        slot_id: &str,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        let (path, terminal_id) = {
            let project = self.project(project_id)?;
            let layout = project.layout.as_ref()?;
            let path = layout.find_path_by_slot_id(slot_id)?;
            let terminal_id = layout.get_at_path(&path).and_then(|leaf| match leaf {
                LayoutNode::Terminal { terminal_id, .. } => terminal_id.clone(),
                _ => None,
            });
            (path, terminal_id)
        };
        if path.is_empty() {
            // The managed pane is the whole layout: keep the project usable
            // with a fresh shell instead of turning it into a bookmark.
            if let Some(project) = self.project_mut(project_id) {
                project.layout = Some(LayoutNode::new_terminal());
                project.managed_runs.remove(slot_id);
                if let Some(id) = &terminal_id {
                    project.terminal_names.remove(id);
                }
            }
            self.notify_data(cx);
        } else {
            self.close_terminal(project_id, &path, cx);
        }
        if let Some(project) = self.project_mut(project_id) {
            project.managed_runs.remove(slot_id);
        }
        terminal_id
    }

    /// Whether any project owns this slot as a managed pane.
    pub fn is_managed_slot(&self, slot_id: &str) -> bool {
        self.data()
            .projects
            .iter()
            .any(|p| p.managed_runs.contains_key(slot_id))
    }

    /// Whether the terminal belongs to a managed pane.
    pub fn is_managed_terminal(&self, terminal_id: &str) -> bool {
        self.data().projects.iter().any(|p| {
            !p.managed_runs.is_empty()
                && p
                    .layout
                    .as_ref()
                    .and_then(|l| l.find_terminal_slot_id(terminal_id))
                    .is_some_and(|slot| p.managed_runs.contains_key(&slot))
        })
    }

    /// The run behind a managed slot.
    pub fn managed_run_for_slot(&self, slot_id: &str) -> Option<String> {
        self.data()
            .projects
            .iter()
            .find_map(|p| p.managed_runs.get(slot_id).cloned())
    }

    /// Set terminal ID at a layout path
    pub fn set_terminal_id(
        &mut self,
        project_id: &str,
        path: &[usize],
        terminal_id: String,
        cx: &mut Context<Self>,
    ) {
        // Paths from the pinned view address the pinned arrangement — leaf
        // state lives in the main layout, so translate first
        let Some(path) = self.to_main_layout_path(project_id, path) else {
            return;
        };
        self.with_project(project_id, cx, |project| {
            if let Some(ref mut layout) = project.layout
                && let Some(node) = layout.get_at_path_mut(&path)
                && let LayoutNode::Terminal {
                    terminal_id: id, ..
                } = node
            {
                *id = Some(terminal_id);
                return true;
            }
            false
        });
    }

    /// Set shell type for a terminal at a layout path
    pub fn set_terminal_shell(
        &mut self,
        project_id: &str,
        path: &[usize],
        shell_type: ShellType,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self.to_main_layout_path(project_id, path) else {
            return;
        };
        // A managed agent pane runs an executable, not a shell: switching
        // its shell would only matter for a respawn that never happens.
        let managed = self
            .project(project_id)
            .and_then(|p| p.layout.as_ref())
            .and_then(|l| l.get_at_path(&path))
            .and_then(|leaf| leaf.slot_id().map(str::to_string))
            .is_some_and(|slot| self.is_managed_slot(&slot));
        if managed {
            return;
        }
        self.with_layout_node(project_id, &path, cx, |node| {
            if let LayoutNode::Terminal { shell_type: st, .. } = node {
                *st = shell_type;
                return true;
            }
            false
        });
    }

    /// Get shell type for a terminal at a layout path
    pub fn get_terminal_shell(&self, project_id: &str, path: &[usize]) -> Option<ShellType> {
        if let Some(LayoutNode::Terminal { shell_type, .. }) =
            self.view_layout(project_id).and_then(|l| l.get_at_path(path))
        {
            Some(shell_type.clone())
        } else {
            None
        }
    }

    /// Rename a terminal
    pub fn rename_terminal(
        &mut self,
        project_id: &str,
        terminal_id: &str,
        new_name: String,
        cx: &mut Context<Self>,
    ) {
        let terminal_id = terminal_id.to_string();
        self.with_project(project_id, cx, |project| {
            project.terminal_names.insert(terminal_id, new_name);
            true
        });
    }

    /// Set terminal hidden state
    #[allow(dead_code)] // API for future terminal visibility control
    pub fn set_terminal_hidden(
        &mut self,
        project_id: &str,
        terminal_id: &str,
        hidden: bool,
        cx: &mut Context<Self>,
    ) {
        let terminal_id = terminal_id.to_string();
        self.with_project(project_id, cx, |project| {
            project.hidden_terminals.insert(terminal_id, hidden);
            true
        });
    }

    /// Restore (un-minimize) a terminal at a path
    pub fn restore_terminal(&mut self, project_id: &str, path: &[usize], cx: &mut Context<Self>) {
        let Some(path) = self.to_main_layout_path(project_id, path) else {
            return;
        };
        self.with_layout_node(project_id, &path, cx, |node| {
            if let LayoutNode::Terminal { minimized, .. } = node {
                *minimized = false;
                true
            } else {
                false
            }
        });
    }

    /// Toggle terminal minimized state by terminal ID (finds path automatically)
    pub fn toggle_terminal_minimized_by_id(
        &mut self,
        project_id: &str,
        terminal_id: &str,
        cx: &mut Context<Self>,
    ) {
        if let Some(project) = self.project_mut(project_id)
            && let Some(ref mut layout) = project.layout
            && let Some(path) = layout.find_terminal_path(terminal_id)
            && let Some(node) = layout.get_at_path_mut(&path)
            && let LayoutNode::Terminal { minimized, .. } = node
        {
            *minimized = !*minimized;
            self.notify_data(cx);
        }
    }

    /// Check if a terminal is minimized by ID
    pub fn is_terminal_minimized(&self, project_id: &str, terminal_id: &str) -> bool {
        if let Some(project) = self.project(project_id)
            && let Some(ref layout) = project.layout
            && let Some(path) = layout.find_terminal_path(terminal_id)
            && let Some(LayoutNode::Terminal { minimized, .. }) = layout.get_at_path(&path)
        {
            return *minimized;
        }
        false
    }

    /// Detach a terminal to a separate window.
    /// Sets the detached flag on the layout node (single source of truth).
    /// Returns true if the terminal was successfully detached.
    pub fn detach_terminal(
        &mut self,
        project_id: &str,
        path: &[usize],
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(path) = self.to_main_layout_path(project_id, path) else {
            return false;
        };
        self.with_layout_node(project_id, &path, cx, |node| {
            if let LayoutNode::Terminal {
                terminal_id: Some(_),
                detached,
                ..
            } = node
                && !*detached
            {
                *detached = true;
                return true;
            }
            false
        })
    }

    /// Re-attach a detached terminal back to its original location.
    /// Scans all project layouts to find the terminal and clear the detached flag.
    pub fn attach_terminal(&mut self, terminal_id: &str, cx: &mut Context<Self>) {
        for project in &mut self.data.projects {
            if let Some(ref mut layout) = project.layout
                && let Some(path) = layout.find_terminal_path(terminal_id)
            {
                if let Some(node) = layout.get_at_path_mut(&path)
                    && let LayoutNode::Terminal { detached, .. } = node
                {
                    *detached = false;
                }
                self.notify_data(cx);
                return;
            }
        }
    }

    /// Check if a terminal is detached by scanning layout trees.
    pub fn is_terminal_detached(&self, terminal_id: &str) -> bool {
        for project in &self.data.projects {
            if let Some(ref layout) = project.layout
                && let Some(path) = layout.find_terminal_path(terminal_id)
                && let Some(LayoutNode::Terminal { detached, .. }) = layout.get_at_path(&path)
            {
                return *detached;
            }
        }
        false
    }

    /// Get the zoom level for a terminal at the given path
    pub fn get_terminal_zoom(&self, project_id: &str, path: &[usize]) -> f32 {
        self.view_layout(project_id)
            .and_then(|l| l.get_at_path(path))
            .and_then(|node| {
                if let LayoutNode::Terminal { zoom_level, .. } = node {
                    Some(*zoom_level)
                } else {
                    None
                }
            })
            .unwrap_or(1.0)
    }

    /// Set the zoom level for a terminal at the given path
    pub fn set_terminal_zoom(
        &mut self,
        project_id: &str,
        path: &[usize],
        zoom: f32,
        cx: &mut Context<Self>,
    ) {
        let clamped = zoom.clamp(0.5, 3.0);
        let Some(path) = self.to_main_layout_path(project_id, path) else {
            return;
        };
        self.with_layout_node(project_id, &path, cx, |node| {
            if let LayoutNode::Terminal { zoom_level, .. } = node {
                *zoom_level = clamped;
                true
            } else {
                false
            }
        });
    }
}

#[cfg(test)]
mod managed_tests {
    use crate::settings::HooksConfig;
    use crate::state::{LayoutNode, ProjectData, Workspace, WorkspaceData};
    use gpui::AppContext as _;
    use notmux_core::theme::FolderColor;
    use std::collections::HashMap;

    fn make_project(id: &str) -> ProjectData {
        ProjectData {
            id: id.to_string(),
            name: format!("Project {id}"),
            path: "/tmp/test".to_string(),
            layout: Some(LayoutNode::new_terminal_with_ids(
                format!("slot-{id}"),
                format!("term-{id}"),
            )),
            terminal_names: HashMap::new(),
            hidden_terminals: HashMap::new(),
            worktree_info: None,
            worktree_ids: Vec::new(),
            folder_color: FolderColor::default(),
            hooks: HooksConfig::default(),
            is_remote: false,
            connection_id: None,
            service_terminals: HashMap::new(),
            default_shell: None,
            hook_terminals: HashMap::new(),
            pinned_slots: Vec::new(),
            pinned_layout: None,
            managed_runs: HashMap::new(),
        }
    }

    fn make_workspace_data(projects: Vec<ProjectData>, order: Vec<&str>) -> WorkspaceData {
        WorkspaceData {
            version: 1,
            projects,
            project_order: order.into_iter().map(String::from).collect(),
            project_widths: HashMap::new(),
            service_panel_heights: HashMap::new(),
            hook_panel_heights: HashMap::new(),
            folders: vec![],
            focused_project_id: None,
            focus_project_individual: false,
            focused_terminal: None,
            pinned_view_active: false,
            pinned_arrangement: None,
        }
    }

    #[gpui::test]
    fn managed_pane_lands_in_the_callers_project_without_focus_theft(cx: &mut gpui::TestAppContext) {
        let data = make_workspace_data(vec![make_project("p1"), make_project("p2")], vec!["p1", "p2"]);
        let workspace = cx.new(|_cx| Workspace::new(data));
        workspace.update(cx, |ws: &mut Workspace, cx| {
            ws.set_focused_project_individual(Some("p1".to_string()), cx);
            ws.set_focused_terminal("p1".to_string(), vec![], cx);
        });
        workspace.update(cx, |ws: &mut Workspace, cx| {
            let path = ws
                .insert_managed_terminal("p2", "slot-w1", "term-w1", "run-1", "Worker A", cx)
                .unwrap();
            assert_eq!(path, vec![1], "second grid cell of p2");
            let p2 = ws.project("p2").unwrap();
            assert_eq!(p2.managed_runs.get("slot-w1").map(String::as_str), Some("run-1"));
            assert_eq!(p2.terminal_names.get("term-w1").map(String::as_str), Some("Worker A"));
            assert!(ws.is_managed_slot("slot-w1"));
            assert!(ws.is_managed_terminal("term-w1"));
            assert!(!ws.is_managed_terminal("term-p2"));
            // p1 untouched, focus unchanged.
            assert!(ws.project("p1").unwrap().managed_runs.is_empty());
            assert_eq!(ws.focused_project_id().map(String::as_str), Some("p1"));
            let focus = ws.focus_manager.focused_terminal_state().unwrap();
            assert_eq!(focus.project_id, "p1");
            assert!(focus.layout_path.is_empty());
        });
    }

    #[gpui::test]
    fn simultaneous_launches_get_distinct_grid_cells(cx: &mut gpui::TestAppContext) {
        let data = make_workspace_data(vec![make_project("p1")], vec!["p1"]);
        let workspace = cx.new(|_cx| Workspace::new(data));
        workspace.update(cx, |ws: &mut Workspace, cx| {
            for i in 0..4 {
                let slot = format!("s{i}");
                let p = ws
                    .insert_managed_terminal("p1", &slot, &format!("t{i}"), &format!("r{i}"), "w", cx)
                    .unwrap();
                // The returned path addresses the new pane at insertion time.
                let leaf = ws.project("p1").unwrap().layout.as_ref().unwrap().get_at_path(&p).unwrap();
                assert_eq!(leaf.slot_id(), Some(slot.as_str()));
            }
            let layout = ws.project("p1").unwrap().layout.clone().unwrap();
            let slots = layout.collect_slot_ids();
            assert_eq!(slots.len(), 5);
            assert_eq!(slots.iter().collect::<std::collections::HashSet<_>>().len(), 5);
            // The five cells form a 4-column grid: one full row plus one.
            assert!(matches!(layout, LayoutNode::Split { .. }));
            let e = ws
                .insert_managed_terminal("p1", "s0", "again", "r9", "w", cx)
                .unwrap_err();
            assert!(e.contains("already managed"));
        });
    }

    #[gpui::test]
    fn failed_launch_rollback_removes_pane_and_map_entry(cx: &mut gpui::TestAppContext) {
        let data = make_workspace_data(vec![make_project("p1")], vec!["p1"]);
        let workspace = cx.new(|_cx| Workspace::new(data));
        workspace.update(cx, |ws: &mut Workspace, cx| {
            ws.insert_managed_terminal("p1", "slot-w", "term-w", "run", "w", cx).unwrap();
            let removed = ws.remove_managed_terminal("p1", "slot-w", cx);
            assert_eq!(removed.as_deref(), Some("term-w"));
            let p = ws.project("p1").unwrap();
            assert!(p.managed_runs.is_empty());
            assert!(!p.terminal_names.contains_key("term-w"));
            assert_eq!(p.layout.as_ref().unwrap().collect_slot_ids(), vec!["slot-p1".to_string()]);
            assert!(ws.remove_managed_terminal("p1", "slot-w", cx).is_none(), "idempotent");
        });
    }

    #[gpui::test]
    fn managed_root_pane_is_replaced_by_a_shell_when_removed(cx: &mut gpui::TestAppContext) {
        let mut project = make_project("p1");
        project.layout = None;
        let data = make_workspace_data(vec![project], vec!["p1"]);
        let workspace = cx.new(|_cx| Workspace::new(data));
        workspace.update(cx, |ws: &mut Workspace, cx| {
            let path = ws
                .insert_managed_terminal("p1", "slot-lead", "term-lead", "run", "Orchestrator", cx)
                .unwrap();
            assert!(path.is_empty());
            ws.remove_managed_terminal("p1", "slot-lead", cx);
            let layout = ws.project("p1").unwrap().layout.clone().unwrap();
            assert!(matches!(layout, LayoutNode::Terminal { terminal_id: None, .. }));
            assert!(!ws.is_managed_slot("slot-lead"));
        });
    }

    #[gpui::test]
    fn closing_a_managed_pane_by_hand_forgets_the_run(cx: &mut gpui::TestAppContext) {
        let data = make_workspace_data(vec![make_project("p1")], vec!["p1"]);
        let workspace = cx.new(|_cx| Workspace::new(data));
        workspace.update(cx, |ws: &mut Workspace, cx| {
            let path = ws.insert_managed_terminal("p1", "slot-w", "term-w", "run", "w", cx).unwrap();
            ws.close_terminal("p1", &path, cx);
            assert!(!ws.is_managed_slot("slot-w"));
        });
    }
}
