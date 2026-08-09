//! Pane pinning and the pinned view (all pinned projects side by side).

use crate::state::{DropZone, PinnedNode, ProjectData, SplitDirection, Workspace};
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
        // Reconcile the pinned-view arrangement (whole project columns as
        // leaves): drop stale projects (unpinned, deleted, remote), append
        // newly pinned ones, rebuild flat when it vanished entirely — the
        // exact lifecycle `pinned_layout` follows one level down.
        let ids = self.pinned_ids_in_base_order();
        if ids.is_empty() {
            self.data.pinned_arrangement = None;
            return;
        }
        let mut tree = self.data.pinned_arrangement.take();
        if let Some(t) = tree.as_mut() {
            // Drop leaves whose project is no longer pinned
            loop {
                let Some(stale) = t
                    .collect_project_ids()
                    .into_iter()
                    .find(|id| !ids.contains(id))
                else {
                    break;
                };
                match t.find_project_path(&stale) {
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
            // First pin (or full rebuild): flat side-by-side columns
            None => {
                let mut it = ids.iter();
                let mut t = PinnedNode::project(it.next().expect("ids not empty").clone());
                for id in it {
                    t.append_project(id);
                }
                self.data.pinned_arrangement = Some(t);
                return;
            }
        };
        // Append pinned projects the arrangement doesn't know yet
        let present = tree.collect_project_ids();
        for id in &ids {
            if !present.iter().any(|p| p == id) {
                tree.append_project(id);
            }
        }
        tree.normalize();
        self.data.pinned_arrangement = Some(tree);
    }

    /// Local projects with at least one pinned pane, in base order
    /// (`project_order` with folders expanded) — the seed order for the
    /// pinned arrangement.
    fn pinned_ids_in_base_order(&self) -> Vec<String> {
        let has_pins = |p: &ProjectData| {
            !p.is_remote
                && p.layout
                    .as_ref()
                    .is_some_and(|l| l.contains_pinned(&p.pinned_slots))
        };
        let mut result: Vec<String> = Vec::new();
        for id in &self.data.project_order {
            if let Some(folder) = self.data.folders.iter().find(|f| f.id == *id) {
                for pid in &folder.project_ids {
                    if let Some(p) = self.data.projects.iter().find(|p| &p.id == pid)
                        && has_pins(p)
                    {
                        result.push(p.id.clone());
                    }
                }
            } else if let Some(p) = self.data.projects.iter().find(|p| p.id == *id)
                && has_pins(p)
            {
                result.push(p.id.clone());
            }
        }
        // Safety net for projects missing from project_order
        for p in &self.data.projects {
            if has_pins(p) && !result.iter().any(|r| r == &p.id) {
                result.push(p.id.clone());
            }
        }
        result
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

    /// Move a pinned-view project container relative to another via a drop
    /// zone — the exact semantics of the pane layer one level down
    /// (Left/Right = vertical split, Top/Bottom = horizontal split,
    /// Center = tab group). Only the pinned arrangement changes;
    /// `project_order` stays untouched.
    pub fn move_pinned_project_zone(
        &mut self,
        source_id: &str,
        target_id: &str,
        zone: DropZone,
        cx: &mut Context<Self>,
    ) {
        if !self.data.pinned_view_active || source_id == target_id {
            return;
        }
        {
            let Some(tree) = self.data.pinned_arrangement.as_mut() else {
                return;
            };
            if tree.collect_project_ids().len() <= 1 {
                return;
            }
            let Some(source_path) = tree.find_project_path(source_id) else {
                return;
            };
            if source_path.is_empty() || tree.find_project_path(target_id).is_none() {
                return;
            }
            let Some(source_node) = tree.remove_at_path(&source_path) else {
                return;
            };
            Self::insert_pinned_at_drop_zone(tree, source_node, target_id, zone);
            tree.normalize();
        }
        self.notify_data(cx);
        self.focus_first_pinned_pane_of(source_id, cx);
    }

    /// Insert `source` relative to the target project's leaf (mirror of the
    /// pane layer's `insert_node_at_drop_zone`). Center on a target already
    /// inside a Tabs group appends to that group instead of nesting.
    fn insert_pinned_at_drop_zone(
        tree: &mut PinnedNode,
        source: PinnedNode,
        target_id: &str,
        zone: DropZone,
    ) {
        let Some(target_path) = tree.find_project_path(target_id) else {
            // Safety net — never lose a project column
            for id in source.collect_project_ids() {
                tree.append_project(&id);
            }
            return;
        };
        if zone == DropZone::Center
            && !target_path.is_empty()
            && let Some(PinnedNode::Tabs {
                children,
                active_tab,
            }) = tree.get_at_path_mut(&target_path[..target_path.len() - 1])
        {
            let insert_at = target_path[target_path.len() - 1] + 1;
            children.insert(insert_at, source);
            *active_tab = insert_at;
            return;
        }
        let Some(node) = tree.get_at_path_mut(&target_path) else {
            return;
        };
        let target_node = std::mem::replace(node, PinnedNode::project(String::new()));
        *node = match zone {
            DropZone::Top => PinnedNode::Split {
                direction: SplitDirection::Horizontal,
                sizes: vec![50.0, 50.0],
                children: vec![source, target_node],
            },
            DropZone::Bottom => PinnedNode::Split {
                direction: SplitDirection::Horizontal,
                sizes: vec![50.0, 50.0],
                children: vec![target_node, source],
            },
            DropZone::Left => PinnedNode::Split {
                direction: SplitDirection::Vertical,
                sizes: vec![50.0, 50.0],
                children: vec![source, target_node],
            },
            DropZone::Right => PinnedNode::Split {
                direction: SplitDirection::Vertical,
                sizes: vec![50.0, 50.0],
                children: vec![target_node, source],
            },
            DropZone::Center => PinnedNode::Tabs {
                children: vec![target_node, source],
                active_tab: 1,
            },
        };
    }

    /// Move a project into or within the tab strip of the group at
    /// `tabs_path` (mirror of the pane layer's tab-strip drops: same group =
    /// reorder with MoveTab semantics, cross-group = insert at position or
    /// append).
    pub fn move_pinned_project_to_tab_group(
        &mut self,
        source_id: &str,
        tabs_path: &[usize],
        insert_index: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        if !self.data.pinned_view_active {
            return;
        }
        {
            let Some(tree) = self.data.pinned_arrangement.as_mut() else {
                return;
            };
            let Some(source_path) = tree.find_project_path(source_id) else {
                return;
            };
            if source_path.is_empty() {
                return;
            }
            if source_path[..source_path.len() - 1] == *tabs_path {
                // Same group → reorder: remove, insert at raw index (moving
                // right lands after the target, left before it)
                let from = source_path[source_path.len() - 1];
                let Some(PinnedNode::Tabs {
                    children,
                    active_tab,
                }) = tree.get_at_path_mut(tabs_path)
                else {
                    return;
                };
                let to = insert_index.unwrap_or(children.len());
                if from == to {
                    return;
                }
                let node = children.remove(from);
                let to = to.min(children.len());
                children.insert(to, node);
                *active_tab = to;
            } else {
                // Reference member to re-locate the group after removal may
                // have shifted paths (mirror of move_terminal_to_tab_group)
                let Some(reference) = tree
                    .get_at_path(tabs_path)
                    .map(|n| n.collect_project_ids())
                    .and_then(|ids| ids.into_iter().find(|id| id != source_id))
                else {
                    return;
                };
                let Some(source_node) = tree.remove_at_path(&source_path) else {
                    return;
                };
                let group_path = tree
                    .find_project_path(&reference)
                    .filter(|p| !p.is_empty())
                    .map(|p| p[..p.len() - 1].to_vec());
                match group_path.and_then(|p| tree.get_at_path_mut(&p)) {
                    Some(PinnedNode::Tabs {
                        children,
                        active_tab,
                    }) => {
                        let idx = insert_index.unwrap_or(children.len()).min(children.len());
                        children.insert(idx, source_node);
                        *active_tab = idx;
                    }
                    _ => {
                        // Group dissolved during removal — never lose a column
                        for id in source_node.collect_project_ids() {
                            tree.append_project(&id);
                        }
                    }
                }
            }
            tree.normalize();
        }
        self.notify_data(cx);
        self.focus_first_pinned_pane_of(source_id, cx);
    }

    /// Activate a project-level tab by index (tab strip click).
    pub fn set_pinned_active_tab(
        &mut self,
        tabs_path: &[usize],
        index: usize,
        cx: &mut Context<Self>,
    ) {
        let focus_target = {
            let Some(tree) = self.data.pinned_arrangement.as_mut() else {
                return;
            };
            let Some(PinnedNode::Tabs {
                children,
                active_tab,
            }) = tree.get_at_path_mut(tabs_path)
            else {
                return;
            };
            if index >= children.len() {
                return;
            }
            *active_tab = index;
            children[index].collect_project_ids().into_iter().next()
        };
        self.notify_data(cx);
        if let Some(pid) = focus_target {
            self.focus_first_pinned_pane_of(&pid, cx);
        }
    }

    /// Focus the first pane of a project's pinned arrangement.
    fn focus_first_pinned_pane_of(&mut self, project_id: &str, cx: &mut Context<Self>) {
        let slot = self
            .project(project_id)
            .and_then(|p| p.pinned_layout.as_ref())
            .and_then(|l| l.collect_slot_ids().into_iter().next());
        if let Some(slot) = slot {
            self.focus_pane_by_slot(project_id, &slot, cx);
        }
    }

    /// Resize a pinned-arrangement split during a divider drag (UI-only —
    /// the final sizes are persisted on mouse-up via notify_data).
    pub fn update_pinned_split_sizes_ui_only(
        &mut self,
        path: &[usize],
        sizes: Vec<f32>,
        cx: &mut Context<Self>,
    ) {
        if let Some(tree) = self.data.pinned_arrangement.as_mut()
            && let Some(PinnedNode::Split { sizes: s, .. }) = tree.get_at_path_mut(path)
        {
            *s = sizes;
            cx.notify();
        }
    }

    /// Whether a pane is pinned.
    pub fn is_pinned(&self, project_id: &str, slot_id: &str) -> bool {
        self.project(project_id)
            .is_some_and(|p| p.pinned_slots.iter().any(|s| s == slot_id))
    }

    /// True when the project's pinned-view column sits directly inside a
    /// project-level tab group (its title bar is then replaced by the strip).
    pub fn pinned_project_in_tab_group(&self, project_id: &str) -> bool {
        self.data.pinned_view_active
            && self
                .data
                .pinned_arrangement
                .as_ref()
                .is_some_and(|t| t.project_in_tab_group(project_id))
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
        // Reveal the project's own column when it hides behind a
        // project-level tab in the pinned arrangement
        if self.data.pinned_view_active
            && let Some(tree) = self.data.pinned_arrangement.as_mut()
        {
            tree.activate_project(project_id);
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
    use crate::state::{
        DropZone, LayoutNode, PinnedNode, ProjectData, SplitDirection, Workspace, WorkspaceData,
    };
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
            pinned_arrangement: None,
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
            // Pinning appends columns to the arrangement in pin order (the
            // same append lifecycle as `pinned_layout` one level down), and
            // folder projects are included like top-level ones.
            ws.toggle_pin("p1", "s1", cx);
            ws.toggle_pin("p2", "s2", cx);
            ws.enter_pinned_view(cx);
            let visible: Vec<_> = ws.visible_projects().iter().map(|p| p.id.clone()).collect();
            assert_eq!(visible, vec!["p1", "p2"]);
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

    /// Pin s1..sN of the given projects and enter the pinned view.
    fn pin_all_and_enter(
        ws: &mut Workspace,
        pairs: &[(&str, &str)],
        cx: &mut gpui::Context<Workspace>,
    ) {
        for (pid, slot) in pairs {
            ws.toggle_pin(pid, slot, cx);
        }
        ws.enter_pinned_view(cx);
    }

    fn three_project_data() -> WorkspaceData {
        make_workspace_data(
            vec![
                make_project("p1", terminal_slot("s1")),
                make_project("p2", terminal_slot("s2")),
                make_project("p3", terminal_slot("s3")),
            ],
            vec!["p1", "p2", "p3"],
        )
    }

    #[gpui::test]
    fn test_sync_builds_flat_arrangement(cx: &mut gpui::TestAppContext) {
        let workspace = cx.new(|_cx| Workspace::new(three_project_data()));
        workspace.update(cx, |ws: &mut Workspace, cx| {
            pin_all_and_enter(ws, &[("p1", "s1"), ("p2", "s2"), ("p3", "s3")], cx);
            let tree = ws.data.pinned_arrangement.as_ref().unwrap();
            assert_eq!(tree.collect_project_ids(), vec!["p1", "p2", "p3"]);
            assert!(matches!(
                tree,
                PinnedNode::Split {
                    direction: SplitDirection::Vertical,
                    ..
                }
            ));
        });
    }

    #[gpui::test]
    fn test_zone_split_moves_column(cx: &mut gpui::TestAppContext) {
        let workspace = cx.new(|_cx| Workspace::new(three_project_data()));
        workspace.update(cx, |ws: &mut Workspace, cx| {
            pin_all_and_enter(ws, &[("p1", "s1"), ("p2", "s2"), ("p3", "s3")], cx);

            // Drop p1 on the RIGHT zone of p3 → lands after p3
            ws.move_pinned_project_zone("p1", "p3", DropZone::Right, cx);
            let visible: Vec<_> = ws.visible_projects().iter().map(|p| p.id.clone()).collect();
            assert_eq!(visible, vec!["p2", "p3", "p1"]);

            // Drop p3 on the LEFT zone of p2 → lands before p2
            ws.move_pinned_project_zone("p3", "p2", DropZone::Left, cx);
            let visible: Vec<_> = ws.visible_projects().iter().map(|p| p.id.clone()).collect();
            assert_eq!(visible, vec!["p3", "p2", "p1"]);

            // The isolated layer: the sidebar/global order is untouched
            assert_eq!(ws.data.project_order, vec!["p1", "p2", "p3"]);
        });
    }

    #[gpui::test]
    fn test_zone_split_top_creates_horizontal_split(cx: &mut gpui::TestAppContext) {
        let workspace = cx.new(|_cx| Workspace::new(three_project_data()));
        workspace.update(cx, |ws: &mut Workspace, cx| {
            pin_all_and_enter(ws, &[("p1", "s1"), ("p2", "s2"), ("p3", "s3")], cx);

            ws.move_pinned_project_zone("p1", "p3", DropZone::Top, cx);
            let tree = ws.data.pinned_arrangement.as_ref().unwrap();
            // Root stays vertical [p2, stacked(p1 over p3)]
            let stacked = tree.get_at_path(&[1]).unwrap();
            match stacked {
                PinnedNode::Split {
                    direction: SplitDirection::Horizontal,
                    children,
                    ..
                } => {
                    assert_eq!(children[0], PinnedNode::project("p1"));
                    assert_eq!(children[1], PinnedNode::project("p3"));
                }
                other => panic!("expected horizontal split, got {other:?}"),
            }
        });
    }

    #[gpui::test]
    fn test_zone_center_creates_tab_group(cx: &mut gpui::TestAppContext) {
        let workspace = cx.new(|_cx| Workspace::new(three_project_data()));
        workspace.update(cx, |ws: &mut Workspace, cx| {
            pin_all_and_enter(ws, &[("p1", "s1"), ("p2", "s2"), ("p3", "s3")], cx);

            ws.move_pinned_project_zone("p1", "p2", DropZone::Center, cx);
            let tree = ws.data.pinned_arrangement.as_ref().unwrap();
            match tree.get_at_path(&[0]) {
                Some(PinnedNode::Tabs {
                    children,
                    active_tab,
                }) => {
                    assert_eq!(children[0], PinnedNode::project("p2"));
                    assert_eq!(children[1], PinnedNode::project("p1"));
                    assert_eq!(*active_tab, 1);
                }
                other => panic!("expected tabs, got {other:?}"),
            }
            assert!(ws.pinned_project_in_tab_group("p1"));
            assert!(ws.pinned_project_in_tab_group("p2"));
            assert!(!ws.pinned_project_in_tab_group("p3"));

            // Center on a member of an existing group appends (no nesting)
            ws.move_pinned_project_zone("p3", "p2", DropZone::Center, cx);
            match ws.data.pinned_arrangement.as_ref().unwrap() {
                PinnedNode::Tabs { children, .. } => assert_eq!(children.len(), 3),
                other => panic!("expected flat tabs root, got {other:?}"),
            }
        });
    }

    #[gpui::test]
    fn test_tab_group_reorder_and_leave(cx: &mut gpui::TestAppContext) {
        let workspace = cx.new(|_cx| Workspace::new(three_project_data()));
        workspace.update(cx, |ws: &mut Workspace, cx| {
            pin_all_and_enter(ws, &[("p1", "s1"), ("p2", "s2"), ("p3", "s3")], cx);
            ws.move_pinned_project_zone("p1", "p2", DropZone::Center, cx);
            // Tree: [Tabs[p2, p1], p3]; drag p3 into the group at index 0
            ws.move_pinned_project_to_tab_group("p3", &[0], Some(0), cx);
            match ws.data.pinned_arrangement.as_ref().unwrap() {
                // The vertical root collapsed to just the tab group
                PinnedNode::Tabs {
                    children,
                    active_tab,
                } => {
                    assert_eq!(children[0], PinnedNode::project("p3"));
                    assert_eq!(*active_tab, 0);
                }
                other => panic!("expected tabs root, got {other:?}"),
            }

            // Reorder within the group: move p3 onto p1's index (2) → after it
            ws.move_pinned_project_to_tab_group("p3", &[], Some(2), cx);
            match ws.data.pinned_arrangement.as_ref().unwrap() {
                PinnedNode::Tabs { children, .. } => {
                    let ids: Vec<_> =
                        children.iter().flat_map(|c| c.collect_project_ids()).collect();
                    assert_eq!(ids, vec!["p2", "p1", "p3"]);
                }
                other => panic!("expected tabs root, got {other:?}"),
            }

            // Zone-drop on a group member splits INSIDE that tab child
            // (exact pane-layer semantics): tab 0 becomes p3|p2, p1 stays a
            // direct tab of the group.
            ws.move_pinned_project_zone("p3", "p2", DropZone::Left, cx);
            let tree = ws.data.pinned_arrangement.as_ref().unwrap();
            assert_eq!(tree.collect_project_ids(), vec!["p3", "p2", "p1"]);
            assert!(!ws.pinned_project_in_tab_group("p3"));
            assert!(!ws.pinned_project_in_tab_group("p2"));
            assert!(ws.pinned_project_in_tab_group("p1"));
        });
    }

    #[gpui::test]
    fn test_arrangement_pruned_on_unpin_and_reappended_on_repin(
        cx: &mut gpui::TestAppContext,
    ) {
        let workspace = cx.new(|_cx| Workspace::new(three_project_data()));
        workspace.update(cx, |ws: &mut Workspace, cx| {
            pin_all_and_enter(ws, &[("p1", "s1"), ("p2", "s2"), ("p3", "s3")], cx);
            ws.move_pinned_project_zone("p1", "p3", DropZone::Right, cx);

            // Unpinning drops the project's column from the arrangement
            ws.toggle_pin("p3", "s3", cx);
            let tree = ws.data.pinned_arrangement.as_ref().unwrap();
            assert_eq!(tree.collect_project_ids(), vec!["p2", "p1"]);

            // Re-pinning appends its column at the end
            ws.toggle_pin("p3", "s3", cx);
            let tree = ws.data.pinned_arrangement.as_ref().unwrap();
            assert_eq!(tree.collect_project_ids(), vec!["p2", "p1", "p3"]);
        });
    }

    #[gpui::test]
    fn test_zone_move_noop_outside_pinned_view(cx: &mut gpui::TestAppContext) {
        let workspace = cx.new(|_cx| Workspace::new(three_project_data()));
        workspace.update(cx, |ws: &mut Workspace, cx| {
            ws.toggle_pin("p1", "s1", cx);
            ws.toggle_pin("p2", "s2", cx);
            ws.move_pinned_project_zone("p1", "p2", DropZone::Right, cx);
            let tree = ws.data.pinned_arrangement.as_ref().unwrap();
            assert_eq!(tree.collect_project_ids(), vec!["p1", "p2"]);
        });
    }

    #[gpui::test]
    fn test_focus_activates_project_tab(cx: &mut gpui::TestAppContext) {
        let workspace = cx.new(|_cx| Workspace::new(three_project_data()));
        workspace.update(cx, |ws: &mut Workspace, cx| {
            pin_all_and_enter(ws, &[("p1", "s1"), ("p2", "s2"), ("p3", "s3")], cx);
            ws.move_pinned_project_zone("p1", "p2", DropZone::Center, cx);
            // p1 is the active tab of the group; focusing p2's pane reveals p2
            ws.focus_pane_by_slot("p2", "s2", cx);
            match ws.data.pinned_arrangement.as_ref().unwrap().get_at_path(&[0]) {
                Some(PinnedNode::Tabs { active_tab, .. }) => assert_eq!(*active_tab, 0),
                other => panic!("expected tabs, got {other:?}"),
            }
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
