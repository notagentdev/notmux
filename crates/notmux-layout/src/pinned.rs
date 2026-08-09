//! Pinned-view arrangement tree — the project-container layer, one level
//! above the pane layer. Mirrors `LayoutNode`'s Split/Tabs semantics with
//! whole projects as leaves: containers split both ways, group as tabs, and
//! move via the same drop-zone model as the panes below them.

use crate::SplitDirection;
use serde::{Deserialize, Serialize};

/// Recursive arrangement node of the pinned view.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PinnedNode {
    /// A whole project column (its panes render their own `pinned_layout`).
    Project { project_id: String },
    Split {
        direction: SplitDirection,
        sizes: Vec<f32>,
        children: Vec<PinnedNode>,
    },
    Tabs {
        children: Vec<PinnedNode>,
        #[serde(default)]
        active_tab: usize,
    },
}

impl PinnedNode {
    pub fn project(id: impl Into<String>) -> Self {
        PinnedNode::Project {
            project_id: id.into(),
        }
    }

    /// Project ids of all leaves in tree order.
    pub fn collect_project_ids(&self) -> Vec<String> {
        match self {
            PinnedNode::Split { children, .. } | PinnedNode::Tabs { children, .. } => children
                .iter()
                .flat_map(|c| c.collect_project_ids())
                .collect(),
            PinnedNode::Project { project_id } => vec![project_id.clone()],
        }
    }

    /// Find the path of the leaf for `project_id`.
    pub fn find_project_path(&self, project_id: &str) -> Option<Vec<usize>> {
        match self {
            PinnedNode::Split { children, .. } | PinnedNode::Tabs { children, .. } => {
                children.iter().enumerate().find_map(|(i, c)| {
                    c.find_project_path(project_id).map(|mut path| {
                        path.insert(0, i);
                        path
                    })
                })
            }
            PinnedNode::Project { project_id: id } => (id == project_id).then(Vec::new),
        }
    }

    pub fn get_at_path(&self, path: &[usize]) -> Option<&PinnedNode> {
        let mut node = self;
        for &i in path {
            node = match node {
                PinnedNode::Split { children, .. } | PinnedNode::Tabs { children, .. } => {
                    children.get(i)?
                }
                PinnedNode::Project { .. } => return None,
            };
        }
        Some(node)
    }

    pub fn get_at_path_mut(&mut self, path: &[usize]) -> Option<&mut PinnedNode> {
        let mut node = self;
        for &i in path {
            node = match node {
                PinnedNode::Split { children, .. } | PinnedNode::Tabs { children, .. } => {
                    children.get_mut(i)?
                }
                PinnedNode::Project { .. } => return None,
            };
        }
        Some(node)
    }

    /// Remove the child at `path`. A parent left with a single child collapses
    /// to that child (same semantics as `LayoutNode::remove_at_path`).
    pub fn remove_at_path(&mut self, path: &[usize]) -> Option<PinnedNode> {
        if path.is_empty() {
            return None;
        }
        let parent_path = &path[..path.len() - 1];
        let child_index = path[path.len() - 1];
        let parent = self.get_at_path_mut(parent_path)?;
        match parent {
            PinnedNode::Project { .. } => None,
            PinnedNode::Split {
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
            PinnedNode::Tabs {
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

    /// Append a project at the root as a rightmost vertical-split sibling with
    /// an average share (mirror of `LayoutNode::append_leaf`).
    pub fn append_project(&mut self, project_id: &str) {
        let leaf = PinnedNode::project(project_id);
        if let PinnedNode::Split {
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
            let old = std::mem::replace(self, PinnedNode::project(""));
            *self = PinnedNode::Split {
                direction: SplitDirection::Vertical,
                sizes: vec![50.0, 50.0],
                children: vec![old, leaf],
            };
        }
    }

    /// Activate tabs along the path to `project_id` so its column is visible
    /// (mirror of `LayoutNode::activate_tabs_along_path`).
    pub fn activate_project(&mut self, project_id: &str) {
        let Some(path) = self.find_project_path(project_id) else {
            return;
        };
        let mut node = self;
        for &i in &path {
            if let PinnedNode::Tabs { active_tab, .. } = node {
                *active_tab = i;
            }
            node = match node {
                PinnedNode::Split { children, .. } | PinnedNode::Tabs { children, .. } => {
                    match children.get_mut(i) {
                        Some(c) => c,
                        None => return,
                    }
                }
                PinnedNode::Project { .. } => return,
            };
        }
    }

    /// True when the project's leaf sits directly inside a Tabs group (its
    /// column then renders no own title bar — the tab strip replaces it).
    pub fn project_in_tab_group(&self, project_id: &str) -> bool {
        let Some(path) = self.find_project_path(project_id) else {
            return false;
        };
        if path.is_empty() {
            return false;
        }
        matches!(
            self.get_at_path(&path[..path.len() - 1]),
            Some(PinnedNode::Tabs { .. })
        )
    }

    /// Normalize the tree in-place — same rules as `LayoutNode::normalize`:
    /// repair split sizes, flatten Tabs-in-Tabs, unwrap single-child
    /// containers, flatten nested same-direction splits.
    pub fn normalize(&mut self) {
        match self {
            PinnedNode::Project { .. } => return,
            PinnedNode::Split { children, .. } | PinnedNode::Tabs { children, .. } => {
                for child in children.iter_mut() {
                    child.normalize();
                }
            }
        }

        if let PinnedNode::Split {
            sizes, children, ..
        } = self
        {
            if sizes.len() != children.len() {
                sizes.truncate(children.len());
                while sizes.len() < children.len() {
                    sizes.push(100.0 / children.len() as f32);
                }
            }
            let has_invalid = sizes.iter().any(|s| *s <= 0.0 || !s.is_finite());
            let total: f32 = sizes.iter().sum();
            let min_resize = total * 0.1;
            let has_tiny_pair = sizes.windows(2).any(|w| w[0] + w[1] <= min_resize);
            if has_invalid || has_tiny_pair {
                let equal = 100.0 / children.len() as f32;
                for s in sizes.iter_mut() {
                    *s = equal;
                }
            }
        }

        // Tabs must not nest — inline an inner group's children.
        if let PinnedNode::Tabs {
            children,
            active_tab,
        } = self
            && children.iter().any(|c| matches!(c, PinnedNode::Tabs { .. }))
        {
            let old_active = *active_tab;
            let mut new_children = Vec::new();
            let mut new_active = 0;
            for (i, child) in children.drain(..).enumerate() {
                match child {
                    PinnedNode::Tabs {
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
            PinnedNode::Split { children, .. } | PinnedNode::Tabs { children, .. } => {
                children.len() == 1
            }
            PinnedNode::Project { .. } => false,
        };
        if should_unwrap {
            match self {
                PinnedNode::Split { children, .. } | PinnedNode::Tabs { children, .. } => {
                    *self = children.remove(0);
                }
                PinnedNode::Project { .. } => {}
            }
            return;
        }

        if let PinnedNode::Split {
            direction,
            sizes,
            children,
        } = self
        {
            let has_same_dir_child = children
                .iter()
                .any(|c| matches!(c, PinnedNode::Split { direction: d, .. } if d == direction));
            if has_same_dir_child {
                let dir = *direction;
                let mut new_children = Vec::new();
                let mut new_sizes = Vec::new();
                for (i, child) in children.drain(..).enumerate() {
                    let parent_size = sizes.get(i).copied().unwrap_or(100.0);
                    match child {
                        PinnedNode::Split {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(direction: SplitDirection, children: Vec<PinnedNode>) -> PinnedNode {
        let n = children.len();
        PinnedNode::Split {
            direction,
            sizes: vec![100.0 / n as f32; n],
            children,
        }
    }

    #[test]
    fn remove_collapses_two_child_split() {
        let mut tree = split(
            SplitDirection::Vertical,
            vec![PinnedNode::project("a"), PinnedNode::project("b")],
        );
        let removed = tree.remove_at_path(&[0]).unwrap();
        assert_eq!(removed, PinnedNode::project("a"));
        assert_eq!(tree, PinnedNode::project("b"));
    }

    #[test]
    fn append_wraps_leaf_into_vertical_split() {
        let mut tree = PinnedNode::project("a");
        tree.append_project("b");
        assert_eq!(tree.collect_project_ids(), vec!["a", "b"]);
        assert!(matches!(
            tree,
            PinnedNode::Split {
                direction: SplitDirection::Vertical,
                ..
            }
        ));
    }

    #[test]
    fn activate_project_sets_active_tabs_along_path() {
        let mut tree = split(
            SplitDirection::Vertical,
            vec![
                PinnedNode::project("a"),
                PinnedNode::Tabs {
                    children: vec![PinnedNode::project("b"), PinnedNode::project("c")],
                    active_tab: 0,
                },
            ],
        );
        tree.activate_project("c");
        match tree.get_at_path(&[1]) {
            Some(PinnedNode::Tabs { active_tab, .. }) => assert_eq!(*active_tab, 1),
            other => panic!("expected tabs, got {other:?}"),
        }
    }

    #[test]
    fn project_in_tab_group_only_for_direct_tab_children() {
        let tree = split(
            SplitDirection::Vertical,
            vec![
                PinnedNode::project("a"),
                PinnedNode::Tabs {
                    children: vec![PinnedNode::project("b"), PinnedNode::project("c")],
                    active_tab: 0,
                },
            ],
        );
        assert!(!tree.project_in_tab_group("a"));
        assert!(tree.project_in_tab_group("b"));
        assert!(tree.project_in_tab_group("c"));
    }

    #[test]
    fn normalize_flattens_same_direction_and_unwraps() {
        let mut tree = split(
            SplitDirection::Vertical,
            vec![
                split(
                    SplitDirection::Vertical,
                    vec![PinnedNode::project("a"), PinnedNode::project("b")],
                ),
                PinnedNode::project("c"),
            ],
        );
        tree.normalize();
        match &tree {
            PinnedNode::Split { children, .. } => assert_eq!(children.len(), 3),
            other => panic!("expected flattened split, got {other:?}"),
        }

        let mut single = PinnedNode::Tabs {
            children: vec![PinnedNode::project("a")],
            active_tab: 0,
        };
        single.normalize();
        assert_eq!(single, PinnedNode::project("a"));
    }

    #[test]
    fn normalize_flattens_nested_tabs() {
        let mut tree = PinnedNode::Tabs {
            children: vec![
                PinnedNode::project("a"),
                PinnedNode::Tabs {
                    children: vec![PinnedNode::project("b"), PinnedNode::project("c")],
                    active_tab: 1,
                },
            ],
            active_tab: 1,
        };
        tree.normalize();
        match &tree {
            PinnedNode::Tabs {
                children,
                active_tab,
            } => {
                assert_eq!(children.len(), 3);
                assert_eq!(*active_tab, 2);
            }
            other => panic!("expected flat tabs, got {other:?}"),
        }
    }
}
