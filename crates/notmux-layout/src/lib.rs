//! notmux-layout — Layout tree algorithms.
//!
//! The `LayoutNode` recursive enum models terminal panes as a tree of
//! `Terminal`, `Split`, and `Tabs` nodes. This crate owns the type and all
//! pure tree algorithms (navigation, mutation, normalization, structure
//! merging) — no GPUI, no workspace state, no hook execution.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use notmux_terminal::shell_config::ShellType;

pub use notmux_core::types::SplitDirection;

fn default_zoom_level() -> f32 {
    1.0
}

fn default_slot_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Recursive layout tree node
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum LayoutNode {
    Terminal {
        #[serde(default = "default_slot_id")]
        slot_id: String,
        terminal_id: Option<String>,
        #[serde(default)]
        minimized: bool,
        #[serde(default)]
        detached: bool,
        #[serde(default)]
        shell_type: ShellType,
        #[serde(default = "default_zoom_level")]
        zoom_level: f32,
    },
    Split {
        direction: SplitDirection,
        sizes: Vec<f32>,
        children: Vec<LayoutNode>,
    },
    Tabs {
        children: Vec<LayoutNode>,
        #[serde(default)]
        active_tab: usize,
    },
    /// A file/code editor leaf — a draggable tab in the middle panel, moved and
    /// split exactly like a `Terminal` leaf.
    Editor {
        #[serde(default = "default_slot_id")]
        slot_id: String,
        #[serde(default)]
        file_path: String,
        /// When true, the pane renders as an editable diff of `file_path`
        /// against HEAD (green additions, red deletions) instead of a plain
        /// editor. Persisted so a restored layout keeps the diff view.
        #[serde(default)]
        diff: bool,
    },
    /// An embedded web-browser leaf — a draggable tab/pane like `Editor`,
    /// persisting its last URL.
    Browser {
        #[serde(default = "default_slot_id")]
        slot_id: String,
        #[serde(default)]
        url: String,
    },
}

impl LayoutNode {
    /// Returns true if this node is effectively hidden (all terminals within it are minimized or detached).
    pub fn is_all_hidden(&self) -> bool {
        match self {
            LayoutNode::Terminal {
                minimized,
                detached,
                ..
            } => *minimized || *detached,
            LayoutNode::Editor { .. } | LayoutNode::Browser { .. } => false,
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                children.iter().all(|c| c.is_all_hidden())
            }
        }
    }

    /// The slot ID of a leaf node (None for Split/Tabs).
    pub fn slot_id(&self) -> Option<&str> {
        match self {
            LayoutNode::Terminal { slot_id, .. }
            | LayoutNode::Editor { slot_id, .. }
            | LayoutNode::Browser { slot_id, .. } => Some(slot_id),
            LayoutNode::Split { .. } | LayoutNode::Tabs { .. } => None,
        }
    }

    /// The terminal id of this leaf, if it is a `Terminal` with an assigned id.
    pub fn terminal_id(&self) -> Option<&str> {
        match self {
            LayoutNode::Terminal { terminal_id, .. } => terminal_id.as_deref(),
            _ => None,
        }
    }

    /// Returns true if this subtree contains at least one leaf whose slot_id is pinned.
    pub fn contains_pinned(&self, pinned: &[String]) -> bool {
        match self {
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                children.iter().any(|c| c.contains_pinned(pinned))
            }
            leaf => leaf
                .slot_id()
                .is_some_and(|s| pinned.iter().any(|p| p == s)),
        }
    }

    /// Find the path of the leaf with the given slot_id.
    pub fn find_path_by_slot_id(&self, slot: &str) -> Option<Vec<usize>> {
        match self {
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                children.iter().enumerate().find_map(|(i, c)| {
                    c.find_path_by_slot_id(slot).map(|mut path| {
                        path.insert(0, i);
                        path
                    })
                })
            }
            leaf => (leaf.slot_id() == Some(slot)).then(Vec::new),
        }
    }

    /// Collect the slot_ids of all leaves in tree order.
    pub fn collect_slot_ids(&self) -> Vec<String> {
        match self {
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                children.iter().flat_map(|c| c.collect_slot_ids()).collect()
            }
            leaf => leaf.slot_id().map(String::from).into_iter().collect(),
        }
    }

    /// Collect cloned leaf nodes whose slot_id is pinned, in tree order.
    pub fn collect_pinned_leaves(&self, pinned: &[String]) -> Vec<LayoutNode> {
        let mut leaves = Vec::new();
        self.collect_pinned_leaves_into(pinned, &mut leaves);
        leaves
    }

    fn collect_pinned_leaves_into(&self, pinned: &[String], out: &mut Vec<LayoutNode>) {
        match self {
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                for child in children {
                    child.collect_pinned_leaves_into(pinned, out);
                }
            }
            leaf => {
                if leaf
                    .slot_id()
                    .is_some_and(|s| pinned.iter().any(|p| p == s))
                {
                    out.push(leaf.clone());
                }
            }
        }
    }

    /// Clone the tree keeping only leaves whose slot_id is in `keep`. Empty
    /// containers are pruned, single-child containers unwrapped, split sizes
    /// keep their relative weights. Returns None when nothing remains.
    pub fn clone_filtered_by_slots(&self, keep: &[String]) -> Option<LayoutNode> {
        match self {
            LayoutNode::Split {
                direction,
                sizes,
                children,
            } => {
                let mut kept_children = Vec::new();
                let mut kept_sizes = Vec::new();
                for (i, child) in children.iter().enumerate() {
                    if let Some(kept) = child.clone_filtered_by_slots(keep) {
                        kept_children.push(kept);
                        kept_sizes.push(
                            sizes
                                .get(i)
                                .copied()
                                .unwrap_or(100.0 / children.len() as f32),
                        );
                    }
                }
                match kept_children.len() {
                    0 => None,
                    1 => Some(kept_children.remove(0)),
                    _ => Some(LayoutNode::Split {
                        direction: *direction,
                        sizes: kept_sizes,
                        children: kept_children,
                    }),
                }
            }
            LayoutNode::Tabs {
                children,
                active_tab,
            } => {
                let mut kept_children = Vec::new();
                let mut new_active = 0;
                for (i, child) in children.iter().enumerate() {
                    if let Some(kept) = child.clone_filtered_by_slots(keep) {
                        if i <= *active_tab {
                            new_active = kept_children.len();
                        }
                        kept_children.push(kept);
                    }
                }
                match kept_children.len() {
                    0 => None,
                    1 => Some(kept_children.remove(0)),
                    _ => Some(LayoutNode::Tabs {
                        children: kept_children,
                        active_tab: new_active,
                    }),
                }
            }
            leaf => leaf
                .slot_id()
                .is_some_and(|s| keep.iter().any(|k| k == s))
                .then(|| leaf.clone()),
        }
    }

    /// Append a leaf at the root level as a rightmost vertical-split sibling,
    /// giving it an average share of the existing sizes.
    pub fn append_leaf(&mut self, leaf: LayoutNode) {
        if let LayoutNode::Split {
            direction: SplitDirection::Vertical,
            sizes,
            children,
        } = self
        {
            let total: f32 = sizes.iter().sum();
            let avg = if children.is_empty() {
                100.0
            } else {
                total / children.len() as f32
            };
            sizes.push(avg);
            children.push(leaf);
        } else {
            let old = std::mem::replace(self, LayoutNode::new_terminal());
            *self = LayoutNode::Split {
                direction: SplitDirection::Vertical,
                sizes: vec![50.0, 50.0],
                children: vec![old, leaf],
            };
        }
    }

    /// Insert `leaf` as a 50/50 split sibling of the leaf with `anchor_slot`.
    /// Returns false when the anchor is not in the tree. Callers should
    /// normalize afterwards to flatten same-direction nesting.
    pub fn insert_leaf_next_to_slot(
        &mut self,
        anchor_slot: &str,
        leaf: LayoutNode,
        direction: SplitDirection,
    ) -> bool {
        let Some(path) = self.find_path_by_slot_id(anchor_slot) else {
            return false;
        };
        let Some(node) = self.get_at_path_mut(&path) else {
            return false;
        };
        let old = std::mem::replace(node, LayoutNode::new_terminal());
        *node = LayoutNode::Split {
            direction,
            sizes: vec![50.0, 50.0],
            children: vec![old, leaf],
        };
        true
    }

    /// Insert `leaf` as a tab sibling of the leaf with `anchor_slot` and make
    /// it the active tab. Returns false when the anchor is not in the tree.
    pub fn insert_leaf_as_tab_of_slot(&mut self, anchor_slot: &str, leaf: LayoutNode) -> bool {
        let Some(path) = self.find_path_by_slot_id(anchor_slot) else {
            return false;
        };
        // Anchor already inside a Tabs group → append there
        if !path.is_empty()
            && let Some(LayoutNode::Tabs {
                children,
                active_tab,
            }) = self.get_at_path_mut(&path[..path.len() - 1])
        {
            children.push(leaf);
            *active_tab = children.len() - 1;
            return true;
        }
        let Some(node) = self.get_at_path_mut(&path) else {
            return false;
        };
        let old = std::mem::replace(node, LayoutNode::new_terminal());
        *node = LayoutNode::Tabs {
            children: vec![old, leaf],
            active_tab: 1,
        };
        true
    }

    /// Replace every leaf with a fresh clone of the same-slot leaf in `source`.
    /// Leaves without a counterpart are left untouched (the caller removes
    /// them); container structure (splits, sizes, tabs) is preserved.
    pub fn refresh_leaves_from(&mut self, source: &LayoutNode) {
        match self {
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                for child in children {
                    child.refresh_leaves_from(source);
                }
            }
            leaf => {
                if let Some(slot) = leaf.slot_id().map(str::to_string)
                    && let Some(path) = source.find_path_by_slot_id(&slot)
                    && let Some(src) = source.get_at_path(&path)
                {
                    *leaf = src.clone();
                }
            }
        }
    }

    /// Replace a terminal ID in the layout tree (for hook rerun).
    pub fn replace_terminal_id(&mut self, old_id: &str, new_id: &str) {
        match self {
            LayoutNode::Terminal { terminal_id, .. } => {
                if terminal_id.as_deref() == Some(old_id) {
                    *terminal_id = Some(new_id.to_string());
                }
            }
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                for child in children {
                    child.replace_terminal_id(old_id, new_id);
                }
            }
            LayoutNode::Editor { .. } | LayoutNode::Browser { .. } => {}
        }
    }

    /// Create a new empty terminal node
    pub fn new_terminal() -> Self {
        LayoutNode::Terminal {
            slot_id: default_slot_id(),
            terminal_id: None,
            minimized: false,
            detached: false,
            shell_type: ShellType::Default,
            zoom_level: 1.0,
        }
    }

    /// Create a new editor node for a file path.
    pub fn new_editor(file_path: impl Into<String>) -> Self {
        LayoutNode::Editor {
            slot_id: default_slot_id(),
            file_path: file_path.into(),
            diff: false,
        }
    }

    /// Create a new editable diff-editor node for a file path (diff vs HEAD).
    pub fn new_diff_editor(file_path: impl Into<String>) -> Self {
        LayoutNode::Editor {
            slot_id: default_slot_id(),
            file_path: file_path.into(),
            diff: true,
        }
    }

    /// Create a new browser node for a URL.
    pub fn new_browser(url: impl Into<String>) -> Self {
        LayoutNode::Browser {
            slot_id: default_slot_id(),
            url: url.into(),
        }
    }

    /// Collect (slot_id, url) for every browser leaf in this tree.
    pub fn collect_browsers(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        self.collect_browsers_recursive(&mut out);
        out
    }

    fn collect_browsers_recursive(&self, out: &mut Vec<(String, String)>) {
        match self {
            LayoutNode::Browser { slot_id, url, .. } => {
                out.push((slot_id.clone(), url.clone()))
            }
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                for c in children {
                    c.collect_browsers_recursive(out);
                }
            }
            LayoutNode::Terminal { .. } | LayoutNode::Editor { .. } => {}
        }
    }

    /// Collect (slot_id, file_path) for every editor leaf in this tree.
    pub fn collect_editors(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        self.collect_editors_recursive(&mut out);
        out
    }

    fn collect_editors_recursive(&self, out: &mut Vec<(String, String)>) {
        match self {
            LayoutNode::Editor {
                slot_id, file_path, ..
            } => out.push((slot_id.clone(), file_path.clone())),
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                for c in children {
                    c.collect_editors_recursive(out);
                }
            }
            LayoutNode::Terminal { .. } | LayoutNode::Browser { .. } => {}
        }
    }

    /// Create a terminal node that runs a specific command with env vars
    pub fn new_terminal_with_command(
        command: &str,
        env_vars: &std::collections::HashMap<String, String>,
    ) -> Self {
        let env_prefix = env_vars
            .iter()
            .map(|(k, v)| format!("{}='{}'", k, v.replace('\'', "'\\''")))
            .collect::<Vec<_>>()
            .join(" ");
        let full_cmd = if env_prefix.is_empty() {
            command.to_string()
        } else {
            format!("{} {}", env_prefix, command)
        };

        LayoutNode::Terminal {
            slot_id: default_slot_id(),
            terminal_id: None,
            minimized: false,
            detached: false,
            shell_type: ShellType::for_command(full_cmd),
            zoom_level: 1.0,
        }
    }

    /// Get the layout node at a given path
    pub fn get_at_path(&self, path: &[usize]) -> Option<&LayoutNode> {
        if path.is_empty() {
            return Some(self);
        }

        match self {
            LayoutNode::Terminal { .. } | LayoutNode::Editor { .. } | LayoutNode::Browser { .. } => None,
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                children.get(path[0])?.get_at_path(&path[1..])
            }
        }
    }

    /// Get a mutable reference to the layout node at a given path
    pub fn get_at_path_mut(&mut self, path: &[usize]) -> Option<&mut LayoutNode> {
        if path.is_empty() {
            return Some(self);
        }

        match self {
            LayoutNode::Terminal { .. } | LayoutNode::Editor { .. } | LayoutNode::Browser { .. } => None,
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                children.get_mut(path[0])?.get_at_path_mut(&path[1..])
            }
        }
    }

    /// Collect all terminal IDs in this layout tree
    pub fn collect_terminal_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();
        self.collect_terminal_ids_recursive(&mut ids);
        ids
    }

    fn collect_terminal_ids_recursive(&self, ids: &mut Vec<String>) {
        match self {
            LayoutNode::Terminal { terminal_id, .. } => {
                if let Some(id) = terminal_id {
                    ids.push(id.clone());
                }
            }
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                for child in children {
                    child.collect_terminal_ids_recursive(ids);
                }
            }
            LayoutNode::Editor { .. } | LayoutNode::Browser { .. } => {}
        }
    }

    /// Clear terminal IDs except those in the `keep` set (e.g. hook terminals).
    /// Kept terminals preserve their ID, minimized, and detached state.
    pub fn clear_terminal_ids_except(&mut self, keep: &HashSet<&str>) {
        match self {
            LayoutNode::Terminal {
                terminal_id,
                minimized,
                detached,
                ..
            } => {
                let should_keep = terminal_id.as_deref().is_some_and(|id| keep.contains(id));
                if !should_keep {
                    *terminal_id = None;
                    *minimized = false;
                    *detached = false;
                }
            }
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                for child in children {
                    child.clear_terminal_ids_except(keep);
                }
            }
            LayoutNode::Editor { .. } | LayoutNode::Browser { .. } => {}
        }
    }

    /// Find the layout path to a terminal by its ID
    pub fn find_terminal_path(&self, target_id: &str) -> Option<Vec<usize>> {
        self.find_terminal_path_recursive(target_id, vec![])
    }

    pub fn terminal_slot_id_at_path(&self, path: &[usize]) -> Option<String> {
        match self.get_at_path(path)? {
            LayoutNode::Terminal { slot_id, .. } => Some(slot_id.clone()),
            _ => None,
        }
    }

    pub fn find_terminal_slot_id(&self, target_id: &str) -> Option<String> {
        match self {
            LayoutNode::Terminal {
                slot_id,
                terminal_id,
                ..
            } => {
                if terminal_id.as_deref() == Some(target_id) {
                    Some(slot_id.clone())
                } else {
                    None
                }
            }
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                children
                    .iter()
                    .find_map(|child| child.find_terminal_slot_id(target_id))
            }
            LayoutNode::Editor { .. } | LayoutNode::Browser { .. } => None,
        }
    }

    /// Layout path of the editor leaf with this slot id.
    pub fn find_editor_path_by_slot(&self, slot: &str) -> Option<Vec<usize>> {
        self.find_editor_path(&|slot_id, _| slot_id == slot, vec![])
    }

    /// Layout path of the first editor leaf showing this file.
    pub fn find_editor_path_by_file(&self, file: &str) -> Option<Vec<usize>> {
        self.find_editor_path(&|_, file_path| file_path == file, vec![])
    }

    /// Layout path of an editor leaf for `file` whose diff mode equals `diff`,
    /// so a plain editor and a diff editor of the same file are distinct panes.
    pub fn find_editor_path_by_file_diff(&self, file: &str, diff: bool) -> Option<Vec<usize>> {
        self.find_editor_path_diff(file, diff, vec![])
    }

    fn find_editor_path_diff(
        &self,
        file: &str,
        diff: bool,
        current_path: Vec<usize>,
    ) -> Option<Vec<usize>> {
        match self {
            LayoutNode::Editor {
                file_path,
                diff: d,
                ..
            } => (file_path == file && *d == diff).then_some(current_path),
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                children.iter().enumerate().find_map(|(i, child)| {
                    let mut child_path = current_path.clone();
                    child_path.push(i);
                    child.find_editor_path_diff(file, diff, child_path)
                })
            }
            LayoutNode::Terminal { .. } | LayoutNode::Browser { .. } => None,
        }
    }

    fn find_editor_path(
        &self,
        matches: &dyn Fn(&str, &str) -> bool,
        current_path: Vec<usize>,
    ) -> Option<Vec<usize>> {
        match self {
            LayoutNode::Editor {
                slot_id, file_path, ..
            } => matches(slot_id, file_path).then_some(current_path),
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                for (i, child) in children.iter().enumerate() {
                    let mut child_path = current_path.clone();
                    child_path.push(i);
                    if let Some(found) = child.find_editor_path(matches, child_path) {
                        return Some(found);
                    }
                }
                None
            }
            LayoutNode::Terminal { .. } | LayoutNode::Browser { .. } => None,
        }
    }

    /// Layout path of the browser leaf with this slot id.
    pub fn find_browser_path_by_slot(&self, slot: &str) -> Option<Vec<usize>> {
        self.find_browser_path_recursive(slot, vec![])
    }

    /// Layout path of the first browser leaf in this tree.
    pub fn find_first_browser_path(&self) -> Option<Vec<usize>> {
        self.collect_browsers()
            .first()
            .and_then(|(slot, _)| self.find_browser_path_by_slot(slot))
    }

    fn find_browser_path_recursive(
        &self,
        slot: &str,
        current_path: Vec<usize>,
    ) -> Option<Vec<usize>> {
        match self {
            LayoutNode::Browser { slot_id, .. } => (slot_id == slot).then_some(current_path),
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                for (i, child) in children.iter().enumerate() {
                    let mut child_path = current_path.clone();
                    child_path.push(i);
                    if let Some(found) = child.find_browser_path_recursive(slot, child_path) {
                        return Some(found);
                    }
                }
                None
            }
            LayoutNode::Terminal { .. } | LayoutNode::Editor { .. } => None,
        }
    }

    /// Layout path of a pane by its shared pane id: terminals are addressed by
    /// terminal id, editors and browsers by slot id. This is the addressing
    /// used by tab drag/close so editor/browser panes move through the same
    /// pipeline as terminals.
    pub fn find_pane_path(&self, pane_id: &str) -> Option<Vec<usize>> {
        self.find_terminal_path(pane_id)
            .or_else(|| self.find_editor_path_by_slot(pane_id))
            .or_else(|| self.find_browser_path_by_slot(pane_id))
    }

    /// All pane ids in this subtree: terminal ids plus editor/browser slot ids.
    pub fn collect_pane_ids(&self) -> Vec<String> {
        let mut ids = self.collect_terminal_ids();
        ids.extend(self.collect_editors().into_iter().map(|(slot, _)| slot));
        ids.extend(self.collect_browsers().into_iter().map(|(slot, _)| slot));
        ids
    }

    /// True if this subtree contains only editor/browser leaves (a single leaf
    /// or a Tabs group of them) — the shape of the project's editor area.
    pub fn is_editor_area(&self) -> bool {
        match self {
            LayoutNode::Editor { .. } | LayoutNode::Browser { .. } => true,
            LayoutNode::Tabs { children, .. } => {
                !children.is_empty() && children.iter().all(|c| c.is_editor_area())
            }
            LayoutNode::Split { .. } | LayoutNode::Terminal { .. } => false,
        }
    }

    fn find_terminal_path_recursive(
        &self,
        target_id: &str,
        current_path: Vec<usize>,
    ) -> Option<Vec<usize>> {
        match self {
            LayoutNode::Terminal { terminal_id, .. } => {
                if terminal_id.as_deref() == Some(target_id) {
                    Some(current_path)
                } else {
                    None
                }
            }
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                for (i, child) in children.iter().enumerate() {
                    let mut child_path = current_path.clone();
                    child_path.push(i);
                    if let Some(found_path) =
                        child.find_terminal_path_recursive(target_id, child_path)
                    {
                        return Some(found_path);
                    }
                }
                None
            }
            LayoutNode::Editor { .. } | LayoutNode::Browser { .. } => None,
        }
    }

    /// True when the node at `path` is not hidden behind an inactive tab:
    /// every `Tabs` ancestor along the way has its `active_tab` pointing into
    /// the path. Used by panes hosting native overlays (webviews) that must
    /// hide themselves when their tab is not the active one.
    pub fn is_path_visible(&self, path: &[usize]) -> bool {
        if path.is_empty() {
            return true;
        }
        match self {
            LayoutNode::Tabs {
                children,
                active_tab,
            } => {
                *active_tab == path[0]
                    && children
                        .get(path[0])
                        .is_some_and(|c| c.is_path_visible(&path[1..]))
            }
            LayoutNode::Split { children, .. } => children
                .get(path[0])
                .is_some_and(|c| c.is_path_visible(&path[1..])),
            _ => false,
        }
    }

    /// Collect terminal IDs that are behind a non-active tab.
    /// A terminal is "inactive" if any ancestor Tabs node has it in a non-active child.
    pub fn collect_inactive_tab_terminal_ids(&self) -> HashSet<String> {
        let mut result = HashSet::new();
        self.collect_inactive_tabs_recursive(&mut result, false);
        result
    }

    fn collect_inactive_tabs_recursive(
        &self,
        result: &mut HashSet<String>,
        is_behind_inactive_tab: bool,
    ) {
        match self {
            LayoutNode::Terminal { terminal_id, .. } => {
                if is_behind_inactive_tab && let Some(id) = terminal_id {
                    result.insert(id.clone());
                }
            }
            LayoutNode::Split { children, .. } => {
                for child in children {
                    child.collect_inactive_tabs_recursive(result, is_behind_inactive_tab);
                }
            }
            LayoutNode::Tabs {
                children,
                active_tab,
            } => {
                for (i, child) in children.iter().enumerate() {
                    let inactive = is_behind_inactive_tab || i != *active_tab;
                    child.collect_inactive_tabs_recursive(result, inactive);
                }
            }
            LayoutNode::Editor { .. } | LayoutNode::Browser { .. } => {}
        }
    }

    /// Collect terminal IDs that belong to a Tabs node with 2+ children.
    /// These terminals are visually grouped in the sidebar with a vertical line.
    pub fn collect_tab_group_terminal_ids(&self) -> HashSet<String> {
        let mut result = HashSet::new();
        self.collect_tab_group_recursive(&mut result, false);
        result
    }

    fn collect_tab_group_recursive(&self, result: &mut HashSet<String>, inside_tab_group: bool) {
        match self {
            LayoutNode::Terminal { terminal_id, .. } => {
                if inside_tab_group && let Some(id) = terminal_id {
                    result.insert(id.clone());
                }
            }
            LayoutNode::Split { children, .. } => {
                for child in children {
                    child.collect_tab_group_recursive(result, inside_tab_group);
                }
            }
            LayoutNode::Tabs { children, .. } => {
                let is_group = children.len() >= 2;
                for child in children {
                    child.collect_tab_group_recursive(result, is_group || inside_tab_group);
                }
            }
            LayoutNode::Editor { .. } | LayoutNode::Browser { .. } => {}
        }
    }

    /// Activate tabs along the given path so the target terminal becomes visible.
    /// For each Tabs node encountered along the path, sets its active_tab to the
    /// path index that leads toward the target.
    pub fn activate_tabs_along_path(&mut self, path: &[usize]) {
        if path.is_empty() {
            return;
        }
        match self {
            LayoutNode::Terminal { .. } | LayoutNode::Editor { .. } | LayoutNode::Browser { .. } => {}
            LayoutNode::Split { children, .. } => {
                if let Some(child) = children.get_mut(path[0]) {
                    child.activate_tabs_along_path(&path[1..]);
                }
            }
            LayoutNode::Tabs {
                children,
                active_tab,
            } => {
                *active_tab = path[0];
                if let Some(child) = children.get_mut(path[0]) {
                    child.activate_tabs_along_path(&path[1..]);
                }
            }
        }
    }

    /// Collect all minimized terminal IDs in this layout tree
    pub fn collect_minimized_terminals(&self) -> Vec<(String, Vec<usize>)> {
        let mut result = Vec::new();
        self.collect_minimized_recursive(&mut result, vec![]);
        result
    }

    fn collect_minimized_recursive(
        &self,
        result: &mut Vec<(String, Vec<usize>)>,
        current_path: Vec<usize>,
    ) {
        match self {
            LayoutNode::Terminal {
                terminal_id,
                minimized,
                ..
            } => {
                if *minimized && let Some(id) = terminal_id {
                    result.push((id.clone(), current_path));
                }
            }
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                for (i, child) in children.iter().enumerate() {
                    let mut child_path = current_path.clone();
                    child_path.push(i);
                    child.collect_minimized_recursive(result, child_path);
                }
            }
            LayoutNode::Editor { .. } | LayoutNode::Browser { .. } => {}
        }
    }

    /// Collect all detached terminal IDs in this layout tree
    pub fn collect_detached_terminals(&self) -> Vec<(String, Vec<usize>)> {
        let mut result = Vec::new();
        self.collect_detached_recursive(&mut result, vec![]);
        result
    }

    fn collect_detached_recursive(
        &self,
        result: &mut Vec<(String, Vec<usize>)>,
        current_path: Vec<usize>,
    ) {
        match self {
            LayoutNode::Terminal {
                terminal_id,
                detached,
                ..
            } => {
                if *detached && let Some(id) = terminal_id {
                    result.push((id.clone(), current_path));
                }
            }
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                for (i, child) in children.iter().enumerate() {
                    let mut child_path = current_path.clone();
                    child_path.push(i);
                    child.collect_detached_recursive(result, child_path);
                }
            }
            LayoutNode::Editor { .. } | LayoutNode::Browser { .. } => {}
        }
    }

    /// Find the path to the first uninitialized terminal (terminal_id: None) in this subtree.
    pub fn find_uninitialized_terminal_path(&self) -> Option<Vec<usize>> {
        self.find_uninitialized_terminal_path_recursive(vec![])
    }

    fn find_uninitialized_terminal_path_recursive(
        &self,
        current_path: Vec<usize>,
    ) -> Option<Vec<usize>> {
        match self {
            LayoutNode::Terminal {
                terminal_id: None, ..
            } => Some(current_path),
            LayoutNode::Terminal { .. } | LayoutNode::Editor { .. } | LayoutNode::Browser { .. } => None,
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                for (i, child) in children.iter().enumerate() {
                    let mut child_path = current_path.clone();
                    child_path.push(i);
                    if let Some(path) = child.find_uninitialized_terminal_path_recursive(child_path)
                    {
                        return Some(path);
                    }
                }
                None
            }
        }
    }

    /// Find the path to the first terminal in this layout subtree
    pub fn find_first_terminal_path(&self) -> Vec<usize> {
        self.find_terminal_path_by_strategy(false)
    }

    /// Find path to the first visible terminal (follows active tabs).
    pub fn find_visible_terminal_path(&self) -> Vec<usize> {
        self.find_terminal_path_by_strategy(true)
    }

    /// Shared implementation: when `follow_active_tab` is true, Tabs nodes
    /// pick the active child; otherwise they always pick child 0.
    fn find_terminal_path_by_strategy(&self, follow_active_tab: bool) -> Vec<usize> {
        self.find_terminal_path_recursive_impl(vec![], follow_active_tab)
    }

    fn find_terminal_path_recursive_impl(
        &self,
        current_path: Vec<usize>,
        follow_active_tab: bool,
    ) -> Vec<usize> {
        match self {
            LayoutNode::Terminal { .. } | LayoutNode::Editor { .. } | LayoutNode::Browser { .. } => current_path,
            LayoutNode::Split { children, .. } => {
                if let Some(first_child) = children.first() {
                    let mut child_path = current_path;
                    child_path.push(0);
                    first_child.find_terminal_path_recursive_impl(child_path, follow_active_tab)
                } else {
                    current_path
                }
            }
            LayoutNode::Tabs {
                children,
                active_tab,
                ..
            } => {
                let idx = if follow_active_tab {
                    (*active_tab).min(children.len().saturating_sub(1))
                } else {
                    0
                };
                if let Some(child) = children.get(idx) {
                    let mut child_path = current_path;
                    child_path.push(idx);
                    child.find_terminal_path_recursive_impl(child_path, follow_active_tab)
                } else {
                    current_path
                }
            }
        }
    }

    /// Remove a child node at the given path.
    /// If the parent has only one child left after removal, collapses the parent to that child.
    /// Returns the removed node, or None if the path is invalid.
    pub fn remove_at_path(&mut self, path: &[usize]) -> Option<LayoutNode> {
        if path.is_empty() {
            return None;
        }

        let parent_path = &path[..path.len() - 1];
        let child_index = path[path.len() - 1];

        let parent = self.get_at_path_mut(parent_path)?;

        match parent {
            LayoutNode::Terminal { .. } | LayoutNode::Editor { .. } | LayoutNode::Browser { .. } => None,
            LayoutNode::Split {
                children, sizes, ..
            } => {
                if child_index >= children.len() {
                    return None;
                }
                let removed = children.remove(child_index);
                if child_index < sizes.len() {
                    sizes.remove(child_index);
                }
                if children.len() == 1 {
                    let remaining = children.remove(0);
                    *parent = remaining;
                }
                Some(removed)
            }
            LayoutNode::Tabs {
                children,
                active_tab,
            } => {
                if child_index >= children.len() {
                    return None;
                }
                let removed = children.remove(child_index);
                if *active_tab >= children.len() {
                    *active_tab = children.len().saturating_sub(1);
                }
                if children.len() == 1 {
                    let remaining = children.remove(0);
                    *parent = remaining;
                }
                Some(removed)
            }
        }
    }

    /// Normalize the layout tree in-place:
    /// - Flatten nested splits with the same direction (merging sizes proportionally)
    /// - Unwrap splits/tabs with a single child
    /// - Remove empty containers
    pub fn normalize(&mut self) {
        match self {
            LayoutNode::Terminal { .. } | LayoutNode::Editor { .. } | LayoutNode::Browser { .. } => return,
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                for child in children.iter_mut() {
                    child.normalize();
                }
            }
        }

        if let LayoutNode::Split {
            sizes, children, ..
        } = self
            && sizes.len() != children.len()
        {
            sizes.truncate(children.len());
            while sizes.len() < children.len() {
                sizes.push(100.0 / children.len() as f32);
            }
        }

        // Sizes are relative weights — the tiny-pair threshold is 10% of the total
        // sum so the check works regardless of overall scale.
        if let LayoutNode::Split {
            sizes, children, ..
        } = self
        {
            let has_invalid = sizes.iter().any(|s| *s <= 0.0 || !s.is_finite());
            let total: f32 = sizes.iter().sum();
            let min_resize = total * 0.1;
            let has_tiny_pair = sizes.windows(2).any(|w| w[0] + w[1] <= min_resize);
            if has_invalid || has_tiny_pair {
                log::warn!(
                    "Layout has invalid/too-small sizes {:?}, resetting to equal",
                    sizes
                );
                let equal = 100.0 / children.len() as f32;
                for s in sizes.iter_mut() {
                    *s = equal;
                }
            }
        }

        // Tabs must not nest: a Tabs child inside a Tabs renders as an opaque
        // "Tab n" with a second tab bar. Inline the inner group's children
        // (the recursion above already flattened deeper levels, so one pass
        // suffices). This also repairs nested groups in persisted layouts —
        // normalize runs on load.
        if let LayoutNode::Tabs {
            children,
            active_tab,
        } = self
            && children.iter().any(|c| matches!(c, LayoutNode::Tabs { .. }))
        {
            let old_active = *active_tab;
            let mut new_children = Vec::new();
            let mut new_active = 0;
            for (i, child) in children.drain(..).enumerate() {
                match child {
                    LayoutNode::Tabs {
                        children: inner,
                        active_tab: inner_active,
                    } => {
                        if i == old_active {
                            new_active = new_children.len()
                                + inner_active.min(inner.len().saturating_sub(1));
                        }
                        new_children.extend(inner);
                    }
                    other => {
                        if i == old_active {
                            new_active = new_children.len();
                        }
                        new_children.push(other);
                    }
                }
            }
            *children = new_children;
            *active_tab = new_active;
        }

        let should_unwrap = match self {
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                children.len() <= 1
            }
            _ => false,
        };
        if should_unwrap {
            match self {
                LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                    if children.len() == 1 {
                        *self = children.remove(0);
                    } else {
                        *self = LayoutNode::new_terminal();
                    }
                }
                _ => {}
            }
            return;
        }

        if let LayoutNode::Split {
            direction,
            sizes,
            children,
        } = self
        {
            let has_same_dir_child = children
                .iter()
                .any(|c| matches!(c, LayoutNode::Split { direction: d, .. } if d == direction));
            if has_same_dir_child {
                let dir = *direction;
                let mut new_children = Vec::new();
                let mut new_sizes = Vec::new();

                for (i, child) in children.drain(..).enumerate() {
                    let parent_size = sizes[i];
                    match child {
                        LayoutNode::Split {
                            direction: child_dir,
                            sizes: child_sizes,
                            children: grandchildren,
                        } if child_dir == dir => {
                            let child_total: f32 = child_sizes.iter().sum();
                            for (j, grandchild) in grandchildren.into_iter().enumerate() {
                                new_children.push(grandchild);
                                new_sizes.push(parent_size * child_sizes[j] / child_total);
                            }
                        }
                        other => {
                            new_children.push(other);
                            new_sizes.push(parent_size);
                        }
                    }
                }

                *children = new_children;
                *sizes = new_sizes;
            }
        }
    }

    /// Clone the layout structure but clear all terminal IDs.
    /// Used when creating worktree projects to duplicate layout with fresh terminals.
    pub fn clone_structure(&self) -> Self {
        match self {
            LayoutNode::Terminal {
                shell_type,
                zoom_level,
                ..
            } => LayoutNode::Terminal {
                slot_id: default_slot_id(),
                terminal_id: None,
                minimized: false,
                detached: false,
                shell_type: shell_type.clone(),
                zoom_level: *zoom_level,
            },
            LayoutNode::Editor {
                file_path, diff, ..
            } => LayoutNode::Editor {
                slot_id: default_slot_id(),
                file_path: file_path.clone(),
                diff: *diff,
            },
            LayoutNode::Browser { url, .. } => LayoutNode::Browser {
                slot_id: default_slot_id(),
                url: url.clone(),
            },
            LayoutNode::Split {
                direction,
                sizes,
                children,
            } => LayoutNode::Split {
                direction: *direction,
                sizes: sizes.clone(),
                children: children.iter().map(|c| c.clone_structure()).collect(),
            },
            LayoutNode::Tabs {
                children,
                active_tab,
            } => LayoutNode::Tabs {
                children: children.iter().map(|c| c.clone_structure()).collect(),
                active_tab: *active_tab,
            },
        }
    }

    /// Merge server layout structure with locally-preserved visual state.
    ///
    /// Takes the structural layout from `server` (terminals, splits, tabs) but
    /// preserves local visual state from `local` where the structure matches:
    /// - **Terminal** with same ID → keep local `minimized` and `detached`
    /// - **Split** with same direction + child count → keep local `sizes`, recurse children
    /// - **Tabs** with same child count → keep local `active_tab`, recurse children
    /// - **Mismatch** → use server's structure but apply visual state from matching terminals
    pub fn merge_visual_state(server: &LayoutNode, local: &LayoutNode) -> LayoutNode {
        match (server, local) {
            (
                LayoutNode::Terminal {
                    slot_id,
                    terminal_id: s_id,
                    shell_type,
                    zoom_level,
                    ..
                },
                LayoutNode::Terminal {
                    terminal_id: l_id,
                    minimized,
                    detached,
                    ..
                },
            ) if s_id == l_id => LayoutNode::Terminal {
                slot_id: slot_id.clone(),
                terminal_id: s_id.clone(),
                minimized: *minimized,
                detached: *detached,
                shell_type: shell_type.clone(),
                zoom_level: *zoom_level,
            },
            (
                LayoutNode::Split {
                    direction: s_dir,
                    children: s_children,
                    ..
                },
                LayoutNode::Split {
                    direction: l_dir,
                    sizes: l_sizes,
                    children: l_children,
                    ..
                },
            ) if s_dir == l_dir && s_children.len() == l_children.len() => {
                let merged_children: Vec<LayoutNode> = s_children
                    .iter()
                    .zip(l_children.iter())
                    .map(|(sc, lc)| LayoutNode::merge_visual_state(sc, lc))
                    .collect();
                LayoutNode::Split {
                    direction: *s_dir,
                    sizes: l_sizes.clone(),
                    children: merged_children,
                }
            }
            (
                LayoutNode::Tabs {
                    children: s_children,
                    ..
                },
                LayoutNode::Tabs {
                    children: l_children,
                    active_tab: l_active,
                    ..
                },
            ) if s_children.len() == l_children.len() => {
                let merged_children: Vec<LayoutNode> = s_children
                    .iter()
                    .zip(l_children.iter())
                    .map(|(sc, lc)| LayoutNode::merge_visual_state(sc, lc))
                    .collect();
                LayoutNode::Tabs {
                    children: merged_children,
                    active_tab: *l_active,
                }
            }
            _ => {
                let mut visual_states = HashMap::new();
                local.collect_terminal_visual_state(&mut visual_states);
                let mut result = server.clone();
                result.apply_terminal_visual_state(&visual_states);
                result
            }
        }
    }

    /// Collect visual state (minimized, detached) from all terminals in this tree.
    fn collect_terminal_visual_state(&self, states: &mut HashMap<String, (bool, bool)>) {
        match self {
            LayoutNode::Terminal {
                terminal_id: Some(id),
                minimized,
                detached,
                ..
            } => {
                states.insert(id.clone(), (*minimized, *detached));
            }
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                for child in children {
                    child.collect_terminal_visual_state(states);
                }
            }
            _ => {}
        }
    }

    /// Apply visual state from a map of terminal_id → (minimized, detached) to matching terminals.
    fn apply_terminal_visual_state(&mut self, states: &HashMap<String, (bool, bool)>) {
        match self {
            LayoutNode::Terminal {
                terminal_id: Some(id),
                minimized,
                detached,
                ..
            } => {
                if let Some(&(m, d)) = states.get(id) {
                    *minimized = m;
                    *detached = d;
                }
            }
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                for child in children {
                    child.apply_terminal_visual_state(states);
                }
            }
            _ => {}
        }
    }

    /// Convert from API layout node.
    #[allow(dead_code)]
    pub fn from_api(api: &notmux_core::api::ApiLayoutNode) -> Self {
        match api {
            notmux_core::api::ApiLayoutNode::Terminal {
                terminal_id,
                minimized,
                detached,
            } => LayoutNode::Terminal {
                slot_id: default_slot_id(),
                terminal_id: terminal_id.clone(),
                minimized: *minimized,
                detached: *detached,
                shell_type: Default::default(),
                zoom_level: 1.0,
            },
            notmux_core::api::ApiLayoutNode::Split {
                direction,
                sizes,
                children,
            } => LayoutNode::Split {
                direction: *direction,
                sizes: sizes.clone(),
                children: children.iter().map(LayoutNode::from_api).collect(),
            },
            notmux_core::api::ApiLayoutNode::Tabs {
                children,
                active_tab,
            } => LayoutNode::Tabs {
                children: children.iter().map(LayoutNode::from_api).collect(),
                active_tab: *active_tab,
            },
        }
    }

    /// Convert from API, prefixing all terminal IDs with the given prefix.
    /// Used for remote projects where terminals are registered with prefixed IDs.
    pub fn from_api_prefixed(api: &notmux_core::api::ApiLayoutNode, prefix: &str) -> Self {
        match api {
            notmux_core::api::ApiLayoutNode::Terminal {
                terminal_id,
                minimized,
                detached,
            } => LayoutNode::Terminal {
                slot_id: default_slot_id(),
                terminal_id: terminal_id.as_ref().map(|id| format!("{}:{}", prefix, id)),
                minimized: *minimized,
                detached: *detached,
                shell_type: Default::default(),
                zoom_level: 1.0,
            },
            notmux_core::api::ApiLayoutNode::Split {
                direction,
                sizes,
                children,
            } => LayoutNode::Split {
                direction: *direction,
                sizes: sizes.clone(),
                children: children
                    .iter()
                    .map(|c| LayoutNode::from_api_prefixed(c, prefix))
                    .collect(),
            },
            notmux_core::api::ApiLayoutNode::Tabs {
                children,
                active_tab,
            } => LayoutNode::Tabs {
                children: children
                    .iter()
                    .map(|c| LayoutNode::from_api_prefixed(c, prefix))
                    .collect(),
                active_tab: *active_tab,
            },
        }
    }

    /// Convert to API layout node.
    pub fn to_api(&self) -> notmux_core::api::ApiLayoutNode {
        match self {
            LayoutNode::Terminal {
                terminal_id,
                minimized,
                detached,
                ..
            } => notmux_core::api::ApiLayoutNode::Terminal {
                terminal_id: terminal_id.clone(),
                minimized: *minimized,
                detached: *detached,
            },
            // Editors/browsers are local-only; remote clients see an empty placeholder.
            LayoutNode::Editor { .. } | LayoutNode::Browser { .. } => {
                notmux_core::api::ApiLayoutNode::Terminal {
                    terminal_id: None,
                    minimized: false,
                    detached: false,
                }
            }
            LayoutNode::Split {
                direction,
                sizes,
                children,
            } => notmux_core::api::ApiLayoutNode::Split {
                direction: *direction,
                sizes: sizes.clone(),
                children: children.iter().map(LayoutNode::to_api).collect(),
            },
            LayoutNode::Tabs {
                children,
                active_tab,
            } => notmux_core::api::ApiLayoutNode::Tabs {
                children: children.iter().map(LayoutNode::to_api).collect(),
                active_tab: *active_tab,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{default_slot_id, LayoutNode, SplitDirection};
    use std::collections::HashSet;
    use notmux_terminal::shell_config::ShellType;

    fn terminal(id: &str) -> LayoutNode {
        LayoutNode::Terminal {
            slot_id: default_slot_id(),
            terminal_id: Some(id.to_string()),
            minimized: false,
            detached: false,
            shell_type: ShellType::Default,
            zoom_level: 1.0,
        }
    }

    fn terminal_minimized(id: &str) -> LayoutNode {
        LayoutNode::Terminal {
            slot_id: default_slot_id(),
            terminal_id: Some(id.to_string()),
            minimized: true,
            detached: false,
            shell_type: ShellType::Default,
            zoom_level: 1.0,
        }
    }

    fn terminal_detached(id: &str) -> LayoutNode {
        LayoutNode::Terminal {
            slot_id: default_slot_id(),
            terminal_id: Some(id.to_string()),
            minimized: false,
            detached: true,
            shell_type: ShellType::Default,
            zoom_level: 1.0,
        }
    }

    fn hsplit(children: Vec<LayoutNode>) -> LayoutNode {
        let count = children.len();
        LayoutNode::Split {
            direction: SplitDirection::Horizontal,
            sizes: vec![100.0 / count as f32; count],
            children,
        }
    }

    fn vsplit(children: Vec<LayoutNode>) -> LayoutNode {
        let count = children.len();
        LayoutNode::Split {
            direction: SplitDirection::Vertical,
            sizes: vec![100.0 / count as f32; count],
            children,
        }
    }

    fn tabs(children: Vec<LayoutNode>) -> LayoutNode {
        LayoutNode::Tabs {
            children,
            active_tab: 0,
        }
    }

    #[test]
    fn get_at_path_empty_returns_self() {
        let node = terminal("t1");
        assert!(node.get_at_path(&[]).is_some());
    }

    #[test]
    fn get_at_path_terminal_with_non_empty_returns_none() {
        let node = terminal("t1");
        assert!(node.get_at_path(&[0]).is_none());
    }

    #[test]
    fn get_at_path_single_index() {
        let node = hsplit(vec![terminal("t1"), terminal("t2")]);
        let child = node.get_at_path(&[1]).unwrap();
        match child {
            LayoutNode::Terminal { terminal_id, .. } => {
                assert_eq!(terminal_id.as_deref(), Some("t2"));
            }
            _ => panic!("Expected terminal"),
        }
    }

    #[test]
    fn get_at_path_nested() {
        let node = hsplit(vec![
            terminal("t1"),
            vsplit(vec![terminal("t2"), terminal("t3")]),
        ]);
        let child = node.get_at_path(&[1, 0]).unwrap();
        match child {
            LayoutNode::Terminal { terminal_id, .. } => {
                assert_eq!(terminal_id.as_deref(), Some("t2"));
            }
            _ => panic!("Expected terminal"),
        }
    }

    #[test]
    fn get_at_path_out_of_bounds() {
        let node = hsplit(vec![terminal("t1")]);
        assert!(node.get_at_path(&[5]).is_none());
    }

    #[test]
    fn collect_terminal_ids_single() {
        let node = terminal("t1");
        assert_eq!(node.collect_terminal_ids(), vec!["t1"]);
    }

    #[test]
    fn collect_terminal_ids_nested() {
        let node = hsplit(vec![
            terminal("t1"),
            vsplit(vec![terminal("t2"), terminal("t3")]),
        ]);
        let ids = node.collect_terminal_ids();
        assert_eq!(ids, vec!["t1", "t2", "t3"]);
    }

    #[test]
    fn collect_terminal_ids_tabs() {
        let node = tabs(vec![terminal("a"), terminal("b")]);
        assert_eq!(node.collect_terminal_ids(), vec!["a", "b"]);
    }

    #[test]
    fn collect_terminal_ids_skips_none() {
        let node = hsplit(vec![LayoutNode::new_terminal(), terminal("t1")]);
        assert_eq!(node.collect_terminal_ids(), vec!["t1"]);
    }

    #[test]
    fn clear_terminal_ids_resets_all() {
        let mut node = hsplit(vec![terminal_minimized("t1"), terminal_detached("t2")]);
        node.clear_terminal_ids_except(&HashSet::new());
        assert!(node.collect_terminal_ids().is_empty());
        match &node {
            LayoutNode::Split { children, .. } => {
                for child in children {
                    if let LayoutNode::Terminal {
                        minimized,
                        detached,
                        ..
                    } = child
                    {
                        assert!(!minimized);
                        assert!(!detached);
                    }
                }
            }
            _ => panic!("Expected split"),
        }
    }

    #[test]
    fn find_terminal_path_existing() {
        let node = hsplit(vec![
            terminal("t1"),
            vsplit(vec![terminal("t2"), terminal("t3")]),
        ]);
        assert_eq!(node.find_terminal_path("t3"), Some(vec![1, 1]));
    }

    #[test]
    fn find_terminal_path_root() {
        let node = terminal("t1");
        assert_eq!(node.find_terminal_path("t1"), Some(vec![]));
    }

    #[test]
    fn find_terminal_path_missing() {
        let node = terminal("t1");
        assert_eq!(node.find_terminal_path("nonexistent"), None);
    }

    #[test]
    fn is_all_hidden_single_terminal() {
        assert!(!terminal("t1").is_all_hidden());
        assert!(terminal_minimized("t1").is_all_hidden());
        assert!(terminal_detached("t1").is_all_hidden());
    }

    #[test]
    fn is_all_hidden_split_mixed() {
        let node = hsplit(vec![terminal("t1"), terminal_minimized("t2")]);
        assert!(!node.is_all_hidden());
    }

    #[test]
    fn is_all_hidden_split_all_minimized() {
        let node = hsplit(vec![terminal_minimized("t1"), terminal_minimized("t2")]);
        assert!(node.is_all_hidden());
    }

    #[test]
    fn is_all_hidden_nested_split() {
        let node = hsplit(vec![
            terminal("t1"),
            vsplit(vec![terminal_minimized("t2"), terminal_minimized("t3")]),
        ]);
        assert!(!node.is_all_hidden());
    }

    #[test]
    fn is_all_hidden_nested_all_hidden() {
        let node = hsplit(vec![
            terminal_minimized("t1"),
            vsplit(vec![terminal_minimized("t2"), terminal_detached("t3")]),
        ]);
        assert!(node.is_all_hidden());
    }

    #[test]
    fn collect_minimized_terminals_finds_correct() {
        let node = hsplit(vec![
            terminal("t1"),
            terminal_minimized("t2"),
            terminal("t3"),
        ]);
        let minimized = node.collect_minimized_terminals();
        assert_eq!(minimized.len(), 1);
        assert_eq!(minimized[0].0, "t2");
        assert_eq!(minimized[0].1, vec![1]);
    }

    #[test]
    fn collect_detached_terminals_finds_correct() {
        let node = hsplit(vec![terminal_detached("t1"), terminal("t2")]);
        let detached = node.collect_detached_terminals();
        assert_eq!(detached.len(), 1);
        assert_eq!(detached[0].0, "t1");
        assert_eq!(detached[0].1, vec![0]);
    }

    #[test]
    fn find_first_terminal_path_terminal() {
        let node = terminal("t1");
        let empty: Vec<usize> = vec![];
        assert_eq!(node.find_first_terminal_path(), empty);
    }

    #[test]
    fn find_first_terminal_path_split() {
        let node = hsplit(vec![terminal("t1"), terminal("t2")]);
        assert_eq!(node.find_first_terminal_path(), vec![0]);
    }

    #[test]
    fn find_first_terminal_path_nested() {
        let node = hsplit(vec![
            vsplit(vec![terminal("t1"), terminal("t2")]),
            terminal("t3"),
        ]);
        assert_eq!(node.find_first_terminal_path(), vec![0, 0]);
    }

    #[test]
    fn find_first_terminal_path_tabs() {
        let node = tabs(vec![terminal("t1"), terminal("t2")]);
        assert_eq!(node.find_first_terminal_path(), vec![0]);
    }

    #[test]
    fn normalize_flattens_nested_tabs() {
        // Tabs{ t1, Tabs{t2, t3}, t4 } with the inner group active (and t3
        // active inside it) — the nested-group shape a center-drop used to
        // create.
        let mut node = LayoutNode::Tabs {
            children: vec![
                terminal("t1"),
                LayoutNode::Tabs {
                    children: vec![terminal("t2"), terminal("t3")],
                    active_tab: 1,
                },
                terminal("t4"),
            ],
            active_tab: 1,
        };
        node.normalize();
        assert_eq!(node.collect_terminal_ids(), vec!["t1", "t2", "t3", "t4"]);
        match &node {
            LayoutNode::Tabs {
                children,
                active_tab,
            } => {
                assert_eq!(children.len(), 4);
                assert!(
                    children
                        .iter()
                        .all(|c| matches!(c, LayoutNode::Terminal { .. }))
                );
                // Active pointed at the inner group → now its active leaf (t3).
                assert_eq!(*active_tab, 2);
            }
            other => panic!("expected flat tabs, got {:?}", other),
        }
    }

    #[test]
    fn normalize_flattens_nested_tabs_active_after_group() {
        // Active tab sits AFTER the inner group — its index must shift by the
        // inner group's extra children.
        let mut node = LayoutNode::Tabs {
            children: vec![
                LayoutNode::Tabs {
                    children: vec![terminal("t1"), terminal("t2")],
                    active_tab: 0,
                },
                terminal("t3"),
            ],
            active_tab: 1,
        };
        node.normalize();
        match &node {
            LayoutNode::Tabs {
                children,
                active_tab,
            } => {
                assert_eq!(children.len(), 3);
                assert_eq!(*active_tab, 2, "active leaf t3 shifted right");
            }
            other => panic!("expected flat tabs, got {:?}", other),
        }
    }

    #[test]
    fn normalize_flattens_deeply_nested_tabs() {
        // Two levels of nesting — recursion flattens bottom-up in one pass.
        let mut node = LayoutNode::Tabs {
            children: vec![
                terminal("t1"),
                LayoutNode::Tabs {
                    children: vec![
                        terminal("t2"),
                        LayoutNode::Tabs {
                            children: vec![terminal("t3"), terminal("t4")],
                            active_tab: 0,
                        },
                    ],
                    active_tab: 0,
                },
            ],
            active_tab: 0,
        };
        node.normalize();
        assert_eq!(node.collect_terminal_ids(), vec!["t1", "t2", "t3", "t4"]);
        match &node {
            LayoutNode::Tabs { children, .. } => {
                assert!(
                    children
                        .iter()
                        .all(|c| matches!(c, LayoutNode::Terminal { .. }))
                );
            }
            other => panic!("expected flat tabs, got {:?}", other),
        }
    }

    #[test]
    fn normalize_single_child_split_unwraps() {
        let mut node = hsplit(vec![terminal("t1")]);
        node.normalize();
        match &node {
            LayoutNode::Terminal { terminal_id, .. } => {
                assert_eq!(terminal_id.as_deref(), Some("t1"));
            }
            _ => panic!("Expected terminal after normalizing single-child split"),
        }
    }

    #[test]
    fn normalize_empty_split_becomes_terminal() {
        let mut node = LayoutNode::Split {
            direction: SplitDirection::Horizontal,
            sizes: vec![],
            children: vec![],
        };
        node.normalize();
        assert!(matches!(node, LayoutNode::Terminal { .. }));
    }

    #[test]
    fn normalize_nested_same_direction_flattens() {
        let mut node = LayoutNode::Split {
            direction: SplitDirection::Horizontal,
            sizes: vec![50.0, 50.0],
            children: vec![
                LayoutNode::Split {
                    direction: SplitDirection::Horizontal,
                    sizes: vec![50.0, 50.0],
                    children: vec![terminal("t1"), terminal("t2")],
                },
                terminal("t3"),
            ],
        };
        node.normalize();
        if let LayoutNode::Split {
            children,
            direction,
            sizes,
        } = &node
        {
            assert_eq!(*direction, SplitDirection::Horizontal);
            assert_eq!(children.len(), 3);
            assert_eq!(sizes.len(), 3);
            assert!((sizes[0] - 25.0).abs() < 0.01);
            assert!((sizes[1] - 25.0).abs() < 0.01);
            assert!((sizes[2] - 50.0).abs() < 0.01);
        } else {
            panic!("Expected flattened horizontal split");
        }
    }

    #[test]
    fn normalize_different_direction_preserved() {
        let mut node = LayoutNode::Split {
            direction: SplitDirection::Horizontal,
            sizes: vec![50.0, 50.0],
            children: vec![vsplit(vec![terminal("t1"), terminal("t2")]), terminal("t3")],
        };
        node.normalize();
        if let LayoutNode::Split {
            children,
            direction,
            ..
        } = &node
        {
            assert_eq!(*direction, SplitDirection::Horizontal);
            assert_eq!(children.len(), 2);
            assert!(matches!(
                &children[0],
                LayoutNode::Split {
                    direction: SplitDirection::Vertical,
                    ..
                }
            ));
        } else {
            panic!("Expected horizontal split with nested vertical");
        }
    }

    #[test]
    fn normalize_single_child_tabs_unwraps() {
        let mut node = tabs(vec![terminal("t1")]);
        node.normalize();
        assert!(matches!(node, LayoutNode::Terminal { .. }));
    }

    #[test]
    fn normalize_deep_recursive() {
        let mut node = hsplit(vec![hsplit(vec![hsplit(vec![terminal("t1")])])]);
        node.normalize();
        match &node {
            LayoutNode::Terminal { terminal_id, .. } => {
                assert_eq!(terminal_id.as_deref(), Some("t1"));
            }
            _ => panic!("Expected terminal after deep normalize"),
        }
    }

    #[test]
    fn normalize_negative_sizes_reset_to_equal() {
        let mut node = LayoutNode::Split {
            direction: SplitDirection::Horizontal,
            sizes: vec![5.0, 2.5, 2.5, -12.0],
            children: vec![
                terminal("t1"),
                terminal("t2"),
                terminal("t3"),
                terminal("t4"),
            ],
        };
        node.normalize();
        if let LayoutNode::Split { sizes, .. } = &node {
            assert_eq!(sizes.len(), 4);
            let expected = 100.0 / 4.0;
            for s in sizes {
                assert!((*s - expected).abs() < f32::EPSILON);
            }
        } else {
            panic!("Expected split");
        }
    }

    #[test]
    fn normalize_zero_size_reset_to_equal() {
        let mut node = LayoutNode::Split {
            direction: SplitDirection::Horizontal,
            sizes: vec![5.0, 0.0],
            children: vec![terminal("t1"), terminal("t2")],
        };
        node.normalize();
        if let LayoutNode::Split { sizes, .. } = &node {
            assert_eq!(sizes.len(), 2);
            assert!((sizes[0] - 50.0).abs() < f32::EPSILON);
            assert!((sizes[1] - 50.0).abs() < f32::EPSILON);
        } else {
            panic!("Expected split");
        }
    }

    #[test]
    fn normalize_tiny_adjacent_sizes_reset_to_equal() {
        let mut node = LayoutNode::Split {
            direction: SplitDirection::Horizontal,
            sizes: vec![90.0, 1.0, 9.0],
            children: vec![terminal("t1"), terminal("t2"), terminal("t3")],
        };
        node.normalize();
        if let LayoutNode::Split { sizes, .. } = &node {
            assert_eq!(sizes.len(), 3);
            let expected = 100.0 / 3.0;
            for s in sizes {
                assert!((*s - expected).abs() < f32::EPSILON);
            }
        } else {
            panic!("Expected split");
        }
    }

    #[test]
    fn normalize_valid_sizes_untouched() {
        let mut node = LayoutNode::Split {
            direction: SplitDirection::Horizontal,
            sizes: vec![50.0, 50.0],
            children: vec![terminal("t1"), terminal("t2")],
        };
        node.normalize();
        if let LayoutNode::Split { sizes, .. } = &node {
            assert!((sizes[0] - 50.0).abs() < f32::EPSILON);
            assert!((sizes[1] - 50.0).abs() < f32::EPSILON);
        } else {
            panic!("Expected split");
        }
    }

    #[test]
    fn normalize_relative_sizes_untouched() {
        let mut node = LayoutNode::Split {
            direction: SplitDirection::Horizontal,
            sizes: vec![26.8, 9.47, 17.6],
            children: vec![terminal("t1"), terminal("t2"), terminal("t3")],
        };
        node.normalize();
        if let LayoutNode::Split { sizes, .. } = &node {
            assert!((sizes[0] - 26.8).abs() < f32::EPSILON);
            assert!((sizes[1] - 9.47).abs() < f32::EPSILON);
            assert!((sizes[2] - 17.6).abs() < f32::EPSILON);
        } else {
            panic!("Expected split");
        }
    }

    #[test]
    fn clone_structure_clears_ids_preserves_shape() {
        let node = hsplit(vec![
            terminal("t1"),
            tabs(vec![terminal("t2"), terminal("t3")]),
        ]);
        let cloned = node.clone_structure();
        assert!(cloned.collect_terminal_ids().is_empty());
        match &cloned {
            LayoutNode::Split { children, .. } => {
                assert_eq!(children.len(), 2);
                assert!(matches!(&children[0], LayoutNode::Terminal { .. }));
                assert!(
                    matches!(&children[1], LayoutNode::Tabs { children, .. } if children.len() == 2)
                );
            }
            _ => panic!("Expected split"),
        }
    }

    #[test]
    fn remove_at_path_from_2_child_split_collapses() {
        let mut node = hsplit(vec![terminal("t1"), terminal("t2")]);
        let removed = node.remove_at_path(&[0]);
        assert!(removed.is_some());
        match &node {
            LayoutNode::Terminal { terminal_id, .. } => {
                assert_eq!(terminal_id.as_deref(), Some("t2"));
            }
            _ => panic!("Expected terminal after collapsing 2-child split"),
        }
    }

    #[test]
    fn remove_at_path_from_3_child_split_keeps_2() {
        let mut node = LayoutNode::Split {
            direction: SplitDirection::Horizontal,
            sizes: vec![33.0, 33.0, 34.0],
            children: vec![terminal("t1"), terminal("t2"), terminal("t3")],
        };
        let removed = node.remove_at_path(&[1]);
        assert!(removed.is_some());
        match &node {
            LayoutNode::Split {
                children, sizes, ..
            } => {
                assert_eq!(children.len(), 2);
                assert_eq!(sizes.len(), 2);
            }
            _ => panic!("Expected split with 2 children"),
        }
    }

    #[test]
    fn remove_at_path_from_tabs_collapses_if_1() {
        let mut node = tabs(vec![terminal("t1"), terminal("t2")]);
        let removed = node.remove_at_path(&[0]);
        assert!(removed.is_some());
        match &node {
            LayoutNode::Terminal { terminal_id, .. } => {
                assert_eq!(terminal_id.as_deref(), Some("t2"));
            }
            _ => panic!("Expected terminal after collapsing 2-child tabs"),
        }
    }

    #[test]
    fn remove_at_path_invalid_index_returns_none() {
        let mut node = hsplit(vec![terminal("t1"), terminal("t2")]);
        let removed = node.remove_at_path(&[5]);
        assert!(removed.is_none());
    }

    #[test]
    fn remove_at_path_empty_returns_none() {
        let mut node = terminal("t1");
        let removed = node.remove_at_path(&[]);
        assert!(removed.is_none());
    }

    #[test]
    fn remove_at_path_nested() {
        let mut node = hsplit(vec![
            terminal("t1"),
            vsplit(vec![terminal("t2"), terminal("t3")]),
        ]);
        let removed = node.remove_at_path(&[1, 0]);
        assert!(removed.is_some());
        match &node {
            LayoutNode::Split { children, .. } => {
                assert_eq!(children.len(), 2);
                match &children[1] {
                    LayoutNode::Terminal { terminal_id, .. } => {
                        assert_eq!(terminal_id.as_deref(), Some("t3"));
                    }
                    _ => panic!("Expected terminal t3"),
                }
            }
            _ => panic!("Expected split"),
        }
    }

    #[test]
    fn serde_round_trip_terminal() {
        let node = terminal("t1");
        let json = serde_json::to_string(&node).unwrap();
        let deserialized: LayoutNode = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.collect_terminal_ids(), vec!["t1"]);
    }

    #[test]
    fn serde_round_trip_complex() {
        let node = hsplit(vec![
            terminal("t1"),
            vsplit(vec![terminal("t2"), terminal("t3")]),
            tabs(vec![terminal("t4"), terminal("t5")]),
        ]);
        let json = serde_json::to_string(&node).unwrap();
        let deserialized: LayoutNode = serde_json::from_str(&json).unwrap();
        assert_eq!(
            deserialized.collect_terminal_ids(),
            vec!["t1", "t2", "t3", "t4", "t5"]
        );
    }

    #[test]
    fn merge_matching_terminals_preserves_visual_flags() {
        let server = terminal("t1");
        let local = LayoutNode::Terminal {
            slot_id: default_slot_id(),
            terminal_id: Some("t1".to_string()),
            minimized: true,
            detached: true,
            shell_type: ShellType::Default,
            zoom_level: 1.0,
        };
        let merged = LayoutNode::merge_visual_state(&server, &local);
        match merged {
            LayoutNode::Terminal {
                minimized,
                detached,
                terminal_id,
                ..
            } => {
                assert_eq!(terminal_id.as_deref(), Some("t1"));
                assert!(minimized, "local minimized should be preserved");
                assert!(detached, "local detached should be preserved");
            }
            _ => panic!("Expected terminal"),
        }
    }

    #[test]
    fn merge_different_terminals_uses_server() {
        let server = terminal("t1");
        let local = terminal_minimized("t2");
        let merged = LayoutNode::merge_visual_state(&server, &local);
        match merged {
            LayoutNode::Terminal {
                terminal_id,
                minimized,
                ..
            } => {
                assert_eq!(terminal_id.as_deref(), Some("t1"));
                assert!(!minimized, "server state should win on ID mismatch");
            }
            _ => panic!("Expected terminal"),
        }
    }

    #[test]
    fn merge_matching_split_preserves_sizes() {
        let server = LayoutNode::Split {
            direction: SplitDirection::Horizontal,
            sizes: vec![50.0, 50.0],
            children: vec![terminal("t1"), terminal("t2")],
        };
        let local = LayoutNode::Split {
            direction: SplitDirection::Horizontal,
            sizes: vec![30.0, 70.0],
            children: vec![terminal("t1"), terminal("t2")],
        };
        let merged = LayoutNode::merge_visual_state(&server, &local);
        match merged {
            LayoutNode::Split { sizes, .. } => {
                assert!(
                    (sizes[0] - 30.0).abs() < f32::EPSILON,
                    "local sizes should be preserved"
                );
                assert!((sizes[1] - 70.0).abs() < f32::EPSILON);
            }
            _ => panic!("Expected split"),
        }
    }

    #[test]
    fn merge_split_child_count_mismatch_uses_server() {
        let server = LayoutNode::Split {
            direction: SplitDirection::Horizontal,
            sizes: vec![33.0, 33.0, 34.0],
            children: vec![terminal("t1"), terminal("t2"), terminal("t3")],
        };
        let local = LayoutNode::Split {
            direction: SplitDirection::Horizontal,
            sizes: vec![30.0, 70.0],
            children: vec![terminal("t1"), terminal("t2")],
        };
        let merged = LayoutNode::merge_visual_state(&server, &local);
        match merged {
            LayoutNode::Split {
                children, sizes, ..
            } => {
                assert_eq!(children.len(), 3, "server child count should win");
                assert!(
                    (sizes[0] - 33.0).abs() < f32::EPSILON,
                    "server sizes should be used"
                );
            }
            _ => panic!("Expected split"),
        }
    }

    #[test]
    fn merge_matching_tabs_preserves_active_tab() {
        let server = LayoutNode::Tabs {
            children: vec![terminal("t1"), terminal("t2")],
            active_tab: 0,
        };
        let local = LayoutNode::Tabs {
            children: vec![terminal("t1"), terminal("t2")],
            active_tab: 1,
        };
        let merged = LayoutNode::merge_visual_state(&server, &local);
        match merged {
            LayoutNode::Tabs { active_tab, .. } => {
                assert_eq!(active_tab, 1, "local active_tab should be preserved");
            }
            _ => panic!("Expected tabs"),
        }
    }

    #[test]
    fn merge_type_mismatch_uses_server() {
        let server = hsplit(vec![terminal("t1"), terminal("t2")]);
        let local = terminal("t1");
        let merged = LayoutNode::merge_visual_state(&server, &local);
        match merged {
            LayoutNode::Split { children, .. } => {
                assert_eq!(
                    children.len(),
                    2,
                    "server structure should win on type mismatch"
                );
            }
            _ => panic!("Expected split"),
        }
    }

    #[test]
    fn merge_recursive_preserves_nested_state() {
        let server = hsplit(vec![
            terminal("t1"),
            LayoutNode::Tabs {
                children: vec![terminal("t2"), terminal("t3")],
                active_tab: 0,
            },
        ]);
        let local = LayoutNode::Split {
            direction: SplitDirection::Horizontal,
            sizes: vec![25.0, 75.0],
            children: vec![
                LayoutNode::Terminal {
                    slot_id: default_slot_id(),
                    terminal_id: Some("t1".to_string()),
                    minimized: true,
                    detached: false,
                    shell_type: ShellType::Default,
                    zoom_level: 1.0,
                },
                LayoutNode::Tabs {
                    children: vec![terminal("t2"), terminal("t3")],
                    active_tab: 1,
                },
            ],
        };
        let merged = LayoutNode::merge_visual_state(&server, &local);
        match &merged {
            LayoutNode::Split {
                sizes, children, ..
            } => {
                assert!((sizes[0] - 25.0).abs() < f32::EPSILON);
                assert!((sizes[1] - 75.0).abs() < f32::EPSILON);
                match &children[0] {
                    LayoutNode::Terminal { minimized, .. } => assert!(*minimized),
                    _ => panic!("Expected terminal"),
                }
                match &children[1] {
                    LayoutNode::Tabs { active_tab, .. } => assert_eq!(*active_tab, 1),
                    _ => panic!("Expected tabs"),
                }
            }
            _ => panic!("Expected split"),
        }
    }

    #[test]
    fn merge_split_from_terminal_preserves_minimized() {
        let server = hsplit(vec![terminal("t1"), terminal("t2")]);
        let local = terminal_minimized("t1");
        let merged = LayoutNode::merge_visual_state(&server, &local);
        match &merged {
            LayoutNode::Split { children, .. } => {
                assert_eq!(children.len(), 2);
                match &children[0] {
                    LayoutNode::Terminal {
                        terminal_id,
                        minimized,
                        ..
                    } => {
                        assert_eq!(terminal_id.as_deref(), Some("t1"));
                        assert!(
                            *minimized,
                            "minimized state should be preserved after split"
                        );
                    }
                    _ => panic!("Expected terminal"),
                }
                match &children[1] {
                    LayoutNode::Terminal {
                        terminal_id,
                        minimized,
                        ..
                    } => {
                        assert_eq!(terminal_id.as_deref(), Some("t2"));
                        assert!(!*minimized, "new terminal should not be minimized");
                    }
                    _ => panic!("Expected terminal"),
                }
            }
            _ => panic!("Expected split"),
        }
    }

    #[test]
    fn merge_structure_change_preserves_detached() {
        let server = hsplit(vec![terminal("t1"), terminal("t2")]);
        let local = terminal_detached("t1");
        let merged = LayoutNode::merge_visual_state(&server, &local);
        match &merged {
            LayoutNode::Split { children, .. } => match &children[0] {
                LayoutNode::Terminal { detached, .. } => {
                    assert!(*detached, "detached state should be preserved");
                }
                _ => panic!("Expected terminal"),
            },
            _ => panic!("Expected split"),
        }
    }

    #[test]
    fn merge_split_child_count_change_preserves_visual_state() {
        let server = hsplit(vec![terminal("t1"), terminal("t2"), terminal("t3")]);
        let local = LayoutNode::Split {
            direction: SplitDirection::Horizontal,
            sizes: vec![30.0, 70.0],
            children: vec![terminal_minimized("t1"), terminal("t2")],
        };
        let merged = LayoutNode::merge_visual_state(&server, &local);
        match &merged {
            LayoutNode::Split { children, .. } => {
                assert_eq!(children.len(), 3);
                match &children[0] {
                    LayoutNode::Terminal { minimized, .. } => {
                        assert!(*minimized, "t1 minimized should be preserved");
                    }
                    _ => panic!("Expected terminal"),
                }
            }
            _ => panic!("Expected split"),
        }
    }

    #[test]
    fn browser_pane_addressing_and_area() {
        let browser = LayoutNode::new_browser("https://example.com");
        let slot = match &browser {
            LayoutNode::Browser { slot_id, .. } => slot_id.clone(),
            _ => unreachable!(),
        };
        let layout = LayoutNode::Split {
            direction: SplitDirection::Horizontal,
            sizes: vec![50.0, 50.0],
            children: vec![terminal("t1"), browser],
        };

        // Browsers are addressed by slot id through the shared pane pipeline.
        assert_eq!(layout.find_pane_path(&slot), Some(vec![1]));
        assert_eq!(layout.find_browser_path_by_slot(&slot), Some(vec![1]));
        assert_eq!(layout.find_first_browser_path(), Some(vec![1]));
        assert!(layout.collect_pane_ids().contains(&slot));
        assert_eq!(
            layout.collect_browsers(),
            vec![(slot, "https://example.com".to_string())]
        );
        // A browser leaf counts as editor area so it joins the right dock.
        assert!(LayoutNode::new_browser("x").is_editor_area());
        // Terminal collection ignores browsers.
        assert!(layout.collect_terminal_ids() == vec!["t1".to_string()]);
    }

    #[test]
    fn is_path_visible_respects_active_tabs() {
        let layout = LayoutNode::Split {
            direction: SplitDirection::Horizontal,
            sizes: vec![50.0, 50.0],
            children: vec![
                terminal("t1"),
                LayoutNode::Tabs {
                    children: vec![terminal("t2"), LayoutNode::new_browser("u")],
                    active_tab: 0,
                },
            ],
        };
        // Split children are always visible; tab children only when active.
        assert!(layout.is_path_visible(&[0]));
        assert!(layout.is_path_visible(&[1, 0]));
        assert!(!layout.is_path_visible(&[1, 1]));
        // Out-of-bounds paths are not visible.
        assert!(!layout.is_path_visible(&[2]));
    }

    #[test]
    fn browser_serde_round_trip() {
        let node = LayoutNode::new_browser("https://example.com/x?q=1");
        let json = serde_json::to_string(&node).unwrap();
        assert!(json.contains(r#""type":"browser""#));
        let back: LayoutNode = serde_json::from_str(&json).unwrap();
        match back {
            LayoutNode::Browser { url, .. } => assert_eq!(url, "https://example.com/x?q=1"),
            other => panic!("expected browser, got {:?}", other),
        }
    }

    // === pin helpers ===

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

    fn editor_slot(slot: &str) -> LayoutNode {
        LayoutNode::Editor {
            slot_id: slot.to_string(),
            file_path: "/tmp/a.rs".to_string(),
            diff: false,
        }
    }

    fn browser_slot(slot: &str) -> LayoutNode {
        LayoutNode::Browser {
            slot_id: slot.to_string(),
            url: "https://example.com".to_string(),
        }
    }

    /// Split[Tabs[term(a), editor(b)], browser(c)]
    fn pin_tree() -> LayoutNode {
        LayoutNode::Split {
            direction: SplitDirection::Vertical,
            sizes: vec![50.0, 50.0],
            children: vec![
                LayoutNode::Tabs {
                    children: vec![terminal_slot("a"), editor_slot("b")],
                    active_tab: 0,
                },
                browser_slot("c"),
            ],
        }
    }

    fn pins(slots: &[&str]) -> Vec<String> {
        slots.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn contains_pinned_finds_nested_leaves() {
        let tree = pin_tree();
        assert!(tree.contains_pinned(&pins(&["b"])));
        assert!(tree.contains_pinned(&pins(&["c"])));
        assert!(!tree.contains_pinned(&pins(&["x"])));
        assert!(!tree.contains_pinned(&[]));
        // Leaf root matches its own slot
        assert!(terminal_slot("a").contains_pinned(&pins(&["a"])));
    }

    #[test]
    fn collect_pinned_leaves_in_tree_order() {
        let tree = pin_tree();
        let leaves = tree.collect_pinned_leaves(&pins(&["c", "a"]));
        let slots: Vec<_> = leaves.iter().filter_map(|l| l.slot_id()).collect();
        assert_eq!(slots, vec!["a", "c"]);
        assert!(tree.collect_pinned_leaves(&[]).is_empty());
    }

    #[test]
    fn collect_slot_ids_all_leaves() {
        assert_eq!(pin_tree().collect_slot_ids(), vec!["a", "b", "c"]);
        assert_eq!(terminal_slot("a").collect_slot_ids(), vec!["a"]);
    }

    #[test]
    fn find_path_by_slot_id_nested() {
        let tree = pin_tree();
        assert_eq!(tree.find_path_by_slot_id("a"), Some(vec![0, 0]));
        assert_eq!(tree.find_path_by_slot_id("b"), Some(vec![0, 1]));
        assert_eq!(tree.find_path_by_slot_id("c"), Some(vec![1]));
        assert_eq!(tree.find_path_by_slot_id("x"), None);
        // Leaf root resolves to the empty path
        assert_eq!(browser_slot("c").find_path_by_slot_id("c"), Some(vec![]));
    }
}
