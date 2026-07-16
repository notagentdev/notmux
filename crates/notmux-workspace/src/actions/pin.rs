//! Pane pinning and the pinned view (all pinned projects side by side).

use crate::state::Workspace;
use gpui::*;

impl Workspace {
    /// Toggle the pinned state of a pane (by leaf slot_id). Local projects only.
    pub fn toggle_pin(&mut self, project_id: &str, slot_id: &str, cx: &mut Context<Self>) {
        let Some(project) = self.data.projects.iter_mut().find(|p| p.id == project_id) else {
            return;
        };
        if project.is_remote {
            return;
        }
        if let Some(pos) = project.pinned_slots.iter().position(|s| s == slot_id) {
            project.pinned_slots.remove(pos);
        } else {
            project.pinned_slots.push(slot_id.to_string());
        }
        self.notify_data(cx);
    }

    /// Whether a pane is pinned.
    pub fn is_pinned(&self, project_id: &str, slot_id: &str) -> bool {
        self.project(project_id)
            .is_some_and(|p| p.pinned_slots.iter().any(|s| s == slot_id))
    }

    /// Enter the pinned view: all projects with pinned panes shown side by side.
    /// No-op when nothing is pinned.
    pub fn enter_pinned_view(&mut self, cx: &mut Context<Self>) {
        if !self.has_pinned_panes() {
            return;
        }
        self.data.pinned_view_active = true;
        self.focus_manager.clear_fullscreen_without_restore();
        self.focus_manager.set_focused_project_id(None);
        self.persist_focus_state();
        self.notify_data(cx);
    }

    /// Focus a pinned pane by its slot id (any leaf kind), activating tabs
    /// along the way so it becomes visible.
    pub fn focus_pane_by_slot(&mut self, project_id: &str, slot_id: &str, cx: &mut Context<Self>) {
        let Some(path) = self
            .project(project_id)
            .and_then(|p| p.layout.as_ref())
            .and_then(|l| l.find_path_by_slot_id(slot_id))
        else {
            return;
        };
        if let Some(project) = self.data.projects.iter_mut().find(|p| p.id == project_id)
            && let Some(ref mut layout) = project.layout
        {
            layout.activate_tabs_along_path(&path);
        }
        self.notify_data(cx);
        self.set_focused_terminal(project_id.to_string(), path, cx);
    }

    /// Focus a freshly-created leaf and, while the pinned view is active,
    /// auto-pin it so new splits / editors / browsers stay visible.
    pub fn focus_new_pane(&mut self, project_id: &str, path: Vec<usize>, cx: &mut Context<Self>) {
        if self.data.pinned_view_active
            && let Some(slot) = self
                .project(project_id)
                .and_then(|p| p.layout.as_ref())
                .and_then(|l| l.get_at_path(&path))
                .and_then(|n| n.slot_id())
                .map(str::to_string)
            && let Some(project) = self.data.projects.iter_mut().find(|p| p.id == project_id)
            && !project.is_remote
            && !project.pinned_slots.iter().any(|s| *s == slot)
        {
            project.pinned_slots.push(slot);
        }
        self.set_focused_terminal(project_id.to_string(), path, cx);
    }

    /// Pinned slots of a project while the pinned view is active — the render
    /// filter for panes. None means no filtering (normal view).
    pub fn active_pin_filter(&self, project_id: &str) -> Option<Vec<String>> {
        if !self.data.pinned_view_active {
            return None;
        }
        self.project(project_id).map(|p| p.pinned_slots.clone())
    }

    /// True when any local project has a pinned pane still present in its layout.
    pub fn has_pinned_panes(&self) -> bool {
        self.data.projects.iter().any(|p| {
            !p.is_remote
                && p.layout
                    .as_ref()
                    .is_some_and(|l| l.contains_pinned(&p.pinned_slots))
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::settings::HooksConfig;
    use crate::state::{LayoutNode, ProjectData, SplitDirection, Workspace, WorkspaceData};
    use gpui::AppContext as _;
    use std::collections::HashMap;
    use notmux_core::theme::FolderColor;
    use notmux_terminal::shell_config::ShellType;

    fn terminal_slot(slot: &str) -> LayoutNode {
        LayoutNode::Terminal {
            slot_id: slot.to_string(),
            terminal_id: Some(format!("tid-{slot}")),
            minimized: false,
            detached: false,
            shell_type: ShellType::Default,
            zoom_level: 1.0,
        }
    }

    fn make_project(id: &str, layout: LayoutNode) -> ProjectData {
        ProjectData {
            id: id.to_string(),
            name: format!("Project {}", id),
            path: "/tmp/test".to_string(),
            show_in_overview: true,
            layout: Some(layout),
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
        }
    }

    #[gpui::test]
    fn test_toggle_pin_add_remove(cx: &mut gpui::TestAppContext) {
        let data = make_workspace_data(vec![make_project("p1", terminal_slot("s1"))], vec!["p1"]);
        let workspace = cx.new(|_cx| Workspace::new(data));

        workspace.update(cx, |ws: &mut Workspace, cx| {
            ws.toggle_pin("p1", "s1", cx);
            assert!(ws.is_pinned("p1", "s1"));
            ws.toggle_pin("p1", "s1", cx);
            assert!(!ws.is_pinned("p1", "s1"));
        });
    }

    #[gpui::test]
    fn test_toggle_pin_remote_is_noop(cx: &mut gpui::TestAppContext) {
        let mut project = make_project("p1", terminal_slot("s1"));
        project.is_remote = true;
        let data = make_workspace_data(vec![project], vec!["p1"]);
        let workspace = cx.new(|_cx| Workspace::new(data));

        workspace.update(cx, |ws: &mut Workspace, cx| {
            ws.toggle_pin("p1", "s1", cx);
            assert!(!ws.is_pinned("p1", "s1"));
        });
    }

    #[gpui::test]
    fn test_enter_pinned_view_requires_pins(cx: &mut gpui::TestAppContext) {
        let data = make_workspace_data(vec![make_project("p1", terminal_slot("s1"))], vec!["p1"]);
        let workspace = cx.new(|_cx| Workspace::new(data));

        workspace.update(cx, |ws: &mut Workspace, cx| {
            // No pins → no-op
            ws.enter_pinned_view(cx);
            assert!(!ws.data.pinned_view_active);

            ws.toggle_pin("p1", "s1", cx);
            ws.enter_pinned_view(cx);
            assert!(ws.data.pinned_view_active);

            // Focusing a single project leaves the pinned view
            ws.set_focused_project(Some("p1".to_string()), cx);
            assert!(!ws.data.pinned_view_active);
        });
    }

    #[gpui::test]
    fn test_visible_projects_in_pinned_view(cx: &mut gpui::TestAppContext) {
        let mut remote = make_project("p3", terminal_slot("s3"));
        remote.is_remote = true;
        remote.pinned_slots = vec!["s3".to_string()];
        // p2 is hidden from the old overview — pins must still make it visible
        let mut p2 = make_project("p2", terminal_slot("s2"));
        p2.show_in_overview = false;
        let data = make_workspace_data(
            vec![make_project("p1", terminal_slot("s1")), p2, remote],
            vec!["p1", "p2", "p3"],
        );
        let workspace = cx.new(|_cx| Workspace::new(data));

        workspace.update(cx, |ws: &mut Workspace, cx| {
            ws.toggle_pin("p2", "s2", cx);
            ws.enter_pinned_view(cx);
            // Only local projects with pinned panes are visible,
            // regardless of show_in_overview
            let visible: Vec<_> = ws.visible_projects().iter().map(|p| p.id.clone()).collect();
            assert_eq!(visible, vec!["p2"]);
        });
    }

    #[gpui::test]
    fn test_pinned_view_order_and_folder_expansion(cx: &mut gpui::TestAppContext) {
        // p2 lives in a folder; order in project_order: folder, p1
        let mut data = make_workspace_data(
            vec![
                make_project("p1", terminal_slot("s1")),
                make_project("p2", terminal_slot("s2")),
            ],
            vec!["f1", "p1"],
        );
        data.folders.push(crate::state::FolderData {
            id: "f1".to_string(),
            name: "Folder".to_string(),
            project_ids: vec!["p2".to_string()],
            collapsed: false,
            folder_color: Default::default(),
        });
        let workspace = cx.new(|_cx| Workspace::new(data));

        workspace.update(cx, |ws: &mut Workspace, cx| {
            ws.toggle_pin("p1", "s1", cx);
            ws.toggle_pin("p2", "s2", cx);
            ws.enter_pinned_view(cx);
            let visible: Vec<_> = ws.visible_projects().iter().map(|p| p.id.clone()).collect();
            // Folder project first (folder comes first in project_order)
            assert_eq!(visible, vec!["p2", "p1"]);
        });
    }

    #[gpui::test]
    fn test_split_auto_pins_in_pinned_view(cx: &mut gpui::TestAppContext) {
        let data = make_workspace_data(vec![make_project("p1", terminal_slot("s1"))], vec!["p1"]);
        let workspace = cx.new(|_cx| Workspace::new(data));

        workspace.update(cx, |ws: &mut Workspace, cx| {
            ws.toggle_pin("p1", "s1", cx);
            ws.enter_pinned_view(cx);
            assert!(ws.data.pinned_view_active);

            // Splitting the pinned pane creates a new terminal — it must auto-pin.
            ws.split_terminal("p1", &[], SplitDirection::Vertical, cx);
            let pinned = ws.project("p1").unwrap().pinned_slots.len();
            assert_eq!(pinned, 2, "new split pane should be auto-pinned");
        });
    }

    #[gpui::test]
    fn test_no_auto_pin_outside_pinned_view(cx: &mut gpui::TestAppContext) {
        let data = make_workspace_data(vec![make_project("p1", terminal_slot("s1"))], vec!["p1"]);
        let workspace = cx.new(|_cx| Workspace::new(data));

        workspace.update(cx, |ws: &mut Workspace, cx| {
            // Not in pinned view → splitting must not pin anything.
            ws.split_terminal("p1", &[], SplitDirection::Vertical, cx);
            assert!(ws.project("p1").unwrap().pinned_slots.is_empty());
        });
    }

    #[gpui::test]
    fn test_pin_removed_when_pane_closes(cx: &mut gpui::TestAppContext) {
        let layout = LayoutNode::Split {
            direction: SplitDirection::Vertical,
            sizes: vec![50.0, 50.0],
            children: vec![terminal_slot("s1"), terminal_slot("s2")],
        };
        let data = make_workspace_data(vec![make_project("p1", layout)], vec!["p1"]);
        let workspace = cx.new(|_cx| Workspace::new(data));

        workspace.update(cx, |ws: &mut Workspace, cx| {
            ws.toggle_pin("p1", "s1", cx);
            ws.toggle_pin("p1", "s2", cx);
            // Close the pane at path [0] (slot s1)
            ws.close_terminal("p1", &[0], cx);
            assert!(!ws.is_pinned("p1", "s1"));
            assert!(ws.is_pinned("p1", "s2"));
        });
    }
}
