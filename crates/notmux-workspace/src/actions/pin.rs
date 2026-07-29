//! Pane pinning and the pinned view (all pinned projects side by side).

use crate::state::{ProjectData, SplitDirection, Workspace};
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
        let mut gone = false;
        if let Some(pos) = project.pinned_slots.iter().position(|s| s == slot_id) {
            project.pinned_slots.remove(pos);
            // Removing the project's last pin drops it from the pinned view —
            // move focus along
            gone = !project
                .layout
                .as_ref()
                .is_some_and(|l| l.contains_pinned(&project.pinned_slots));
        } else {
            project.pinned_slots.push(slot_id.to_string());
        }
        if gone {
            self.refocus_pinned_view(project_id, cx);
        }
        self.notify_data(cx);
    }

    /// Unpin every pane of a project. If this empties the pinned view,
    /// leave it and zoom to the project instead of the all-projects overview.
    pub fn unpin_all_in_project(&mut self, project_id: &str, cx: &mut Context<Self>) {
        let Some(project) = self.data.projects.iter_mut().find(|p| p.id == project_id) else {
            return;
        };
        if project.pinned_slots.is_empty() {
            return;
        }
        project.pinned_slots.clear();
        self.refocus_pinned_view(project_id, cx);
        self.notify_data(cx);
    }

    /// Keep focus (and with it the git/files panels, which follow the focused
    /// terminal's project) on a visible project after `removed_project_id`
    /// dropped out of the pinned view — by unpinning or deletion.
    ///
    /// - Other pinned projects remain: focus the first one's first pinned pane.
    /// - Nothing pinned anymore: exit the pinned view and zoom to the removed
    ///   project (or the first remaining one if it was deleted).
    pub(crate) fn refocus_pinned_view(&mut self, removed_project_id: &str, cx: &mut Context<Self>) {
        if !self.data.pinned_view_active {
            return;
        }
        // Callers mutate pins right before this — reconcile the pinned
        // arrangements so the slot lookups below see the new state
        self.sync_pinned_layouts();
        if !self.has_pinned_panes() {
            self.data.pinned_view_active = false;
            let target = if self.project(removed_project_id).is_some() {
                Some(removed_project_id.to_string())
            } else {
                self.first_project_id_in_order()
            };
            self.focus_manager.set_focused_project_id(target.clone());
            if let Some(ref pid) = target {
                self.focus_first_terminal_in(pid);
            }
            self.persist_focus_state();
            return;
        }
        // Only steal focus if it still points at the removed project
        let focused = self
            .focus_manager
            .focused_terminal_state()
            .map(|f| f.project_id);
        if focused.as_deref() != Some(removed_project_id) {
            return;
        }
        // First project still in the pinned view, and the first pane of its
        // pinned arrangement
        let target = self.visible_projects().first().and_then(|p| {
            let arrangement = p.pinned_layout.as_ref()?;
            arrangement
                .collect_slot_ids()
                .into_iter()
                .next()
                .map(|s| (p.id.clone(), s))
        });
        if let Some((pid, slot)) = target {
            self.focus_pane_by_slot(&pid, &slot, cx);
        }
    }

    /// Reconcile every local project's pinned arrangement (`pinned_layout`)
    /// with its main layout: build it on first pin (mirroring the filtered
    /// project layout), append newly pinned panes, drop unpinned or closed
    /// ones, and refresh leaf content (terminal ids, file paths, urls) from
    /// the main layout — which stays the source of truth for pane content,
    /// while `pinned_layout` owns only the pinned view's arrangement.
    pub(crate) fn sync_pinned_layouts(&mut self) {
        for project in self.data.projects.iter_mut().filter(|p| !p.is_remote) {
            Self::sync_pinned_layout(project);
        }
    }

    fn sync_pinned_layout(project: &mut ProjectData) {
        let Some(layout) = project.layout.as_ref() else {
            project.pinned_layout = None;
            return;
        };
        // Pins that still exist as leaves in the main layout
        let pins: Vec<String> = project
            .pinned_slots
            .iter()
            .filter(|s| layout.find_path_by_slot_id(s).is_some())
            .cloned()
            .collect();
        if pins.is_empty() {
            project.pinned_layout = None;
            return;
        }
        let mut tree = project.pinned_layout.take();
        if let Some(t) = tree.as_mut() {
            // Drop leaves that are no longer pinned or left the main layout
            loop {
                let Some(stale) = t
                    .collect_slot_ids()
                    .into_iter()
                    .find(|s| !pins.contains(s))
                else {
                    break;
                };
                match t.find_path_by_slot_id(&stale) {
                    Some(path) if !path.is_empty() => {
                        t.remove_at_path(&path);
                    }
                    // The stale leaf is the root → nothing kept, rebuild below
                    _ => {
                        tree = None;
                        break;
                    }
                }
            }
        }
        let mut tree = match tree {
            Some(t) => t,
            // First pin (or full rebuild): mirror the filtered project layout
            None => {
                project.pinned_layout = layout.clone_filtered_by_slots(&pins);
                return;
            }
        };
        // Append pins the arrangement doesn't know yet (unless an action
        // already placed them, e.g. a split next to its source pane)
        let present = tree.collect_slot_ids();
        for slot in &pins {
            if !present.contains(slot)
                && let Some(path) = layout.find_path_by_slot_id(slot)
                && let Some(leaf) = layout.get_at_path(&path)
            {
                tree.append_leaf(leaf.clone());
            }
        }
        tree.refresh_leaves_from(layout);
        tree.normalize();
        project.pinned_layout = Some(tree);
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
        self.sync_pinned_layouts();
        // The previous focus path refers to the project layout — carry the
        // focus over to the same pane in the pinned arrangement when it is
        // pinned, otherwise to the first pane of the first pinned project.
        let target = self
            .focus_manager
            .focused_terminal_state()
            .and_then(|f| {
                let project = self.project(&f.project_id)?;
                let slot = project
                    .layout
                    .as_ref()?
                    .get_at_path(&f.layout_path)?
                    .slot_id()?
                    .to_string();
                project.pinned_layout.as_ref()?.find_path_by_slot_id(&slot)?;
                Some((f.project_id, slot))
            })
            .or_else(|| {
                self.visible_projects().first().and_then(|p| {
                    let arrangement = p.pinned_layout.as_ref()?;
                    arrangement
                        .collect_slot_ids()
                        .into_iter()
                        .next()
                        .map(|s| (p.id.clone(), s))
                })
            });
        if let Some((pid, slot)) = target {
            self.focus_pane_by_slot(&pid, &slot, cx);
        }
        self.persist_focus_state();
        self.notify_data(cx);
    }

    /// Focus a pinned pane by its slot id (any leaf kind), activating tabs
    /// along the way so it becomes visible.
    pub fn focus_pane_by_slot(&mut self, project_id: &str, slot_id: &str, cx: &mut Context<Self>) {
        // Resolve against the tree the current view renders (the independent
        // pinned arrangement while the pinned view is active)
        let Some(path) = self
            .view_layout(project_id)
            .and_then(|l| l.find_path_by_slot_id(slot_id))
        else {
            return;
        };
        if let Some(layout) = self.view_layout_mut(project_id) {
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

    /// Pin a freshly created main-layout pane at `main_path` and place it in
    /// the pinned arrangement next to `anchor_slot` — as a split sibling
    /// (`Some(direction)`) or as its tab (`None`) — then focus it. Without a
    /// usable anchor the pane is appended at the root by the sync pass.
    pub(crate) fn adopt_new_pane_into_pinned(
        &mut self,
        project_id: &str,
        main_path: &[usize],
        anchor_slot: Option<&str>,
        split_direction: Option<SplitDirection>,
        cx: &mut Context<Self>,
    ) {
        let Some((slot, leaf)) = self
            .project(project_id)
            .and_then(|p| p.layout.as_ref())
            .and_then(|l| l.get_at_path(main_path))
            .and_then(|n| n.slot_id().map(|s| (s.to_string(), n.clone())))
        else {
            return;
        };
        if let Some(project) = self.data.projects.iter_mut().find(|p| p.id == project_id) {
            if !project.pinned_slots.iter().any(|s| *s == slot) {
                project.pinned_slots.push(slot.clone());
            }
            if let (Some(anchor), Some(tree)) = (anchor_slot, project.pinned_layout.as_mut()) {
                let placed = match split_direction {
                    Some(direction) => tree.insert_leaf_next_to_slot(anchor, leaf, direction),
                    None => tree.insert_leaf_as_tab_of_slot(anchor, leaf),
                };
                if placed {
                    tree.normalize();
                }
            }
        }
        // The sync pass (via notify) appends the pane if it wasn't placed above
        self.notify_data(cx);
        if let Some(path) = self
            .view_layout(project_id)
            .and_then(|l| l.find_path_by_slot_id(&slot))
        {
            self.set_focused_terminal(project_id.to_string(), path, cx);
        }
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
            path: "/tmp/test".to_string(),            layout: Some(layout),
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
        let data = make_workspace_data(
            vec![
                make_project("p1", terminal_slot("s1")),
                make_project("p2", terminal_slot("s2")),
                remote,
            ],
            vec!["p1", "p2", "p3"],
        );
        let workspace = cx.new(|_cx| Workspace::new(data));

        workspace.update(cx, |ws: &mut Workspace, cx| {
            ws.toggle_pin("p2", "s2", cx);
            ws.enter_pinned_view(cx);
            // Only local projects with pinned panes are visible.
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
