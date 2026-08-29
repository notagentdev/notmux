//! Per-project registry of live editor viewers, keyed by layout slot id.
//!
//! `LayoutContainer`s are cached by layout path, so any change that shifts
//! paths — opening a terminal (`add_terminal` re-roots the whole tree),
//! grouping an editor into tabs, reordering or closing a tab, dragging a pane —
//! drops the container and builds a new one. A viewer owned by the container
//! died with it, taking the unsaved buffer along: the editor silently reloaded
//! from disk.
//!
//! Terminals do not have that problem because the `Terminal` lives in the
//! shared `TerminalsRegistry` and the pane is only a view onto it. Editors work
//! the same way here: the `FileViewer` belongs to the slot, the pane looks it
//! up. A leaf's `(slot_id, file_path)` never changes once created, so the slot
//! id identifies the viewer exactly — including untitled buffers, which all
//! carry an empty path.
//!
//! Entries are pruned by [`retain_slots`] once a slot is gone from both the
//! project layout and its pinned arrangement.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::Entity;
use notmux_files::file_viewer::FileViewer;

/// Live editor viewers of one project, keyed by layout slot id.
#[derive(Clone, Default)]
pub struct EditorRegistry(Rc<RefCell<HashMap<String, Entity<FileViewer>>>>);

impl EditorRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// The viewer for `slot_id`, if one has been built.
    pub fn get(&self, slot_id: &str) -> Option<Entity<FileViewer>> {
        self.0.borrow().get(slot_id).cloned()
    }

    /// Store the viewer for `slot_id` (replacing any previous entry).
    pub fn insert(&self, slot_id: &str, viewer: Entity<FileViewer>) {
        self.0.borrow_mut().insert(slot_id.to_string(), viewer);
    }

    /// Drop every viewer whose slot is not in `live` — the editor was closed.
    /// Keeping them would leak one buffer per closed editor for the life of
    /// the app.
    pub fn retain_slots(&self, live: &std::collections::HashSet<String>) {
        self.0.borrow_mut().retain(|slot, _| live.contains(slot));
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.0.borrow().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::AppContext as _;
    use std::collections::HashSet;

    /// A viewer is addressed by slot, so the same slot hands back the same
    /// entity no matter how often the pane around it is rebuilt — that is what
    /// keeps an unsaved buffer alive across a layout change.
    #[gpui::test]
    fn same_slot_returns_the_same_viewer(cx: &mut gpui::TestAppContext) {
        let registry = EditorRegistry::new();
        let viewer = cx.update(|cx| cx.new(|cx| test_viewer(cx)));
        registry.insert("slot-a", viewer.clone());

        assert_eq!(registry.get("slot-a"), Some(viewer));
        assert!(registry.get("slot-b").is_none());
    }

    /// Closing an editor must drop its viewer, or every closed file leaks its
    /// buffer for the life of the app.
    #[gpui::test]
    fn retain_drops_closed_slots(cx: &mut gpui::TestAppContext) {
        let registry = EditorRegistry::new();
        for slot in ["slot-a", "slot-b"] {
            let viewer = cx.update(|cx| cx.new(|cx| test_viewer(cx)));
            registry.insert(slot, viewer);
        }
        assert_eq!(registry.len(), 2);

        let live: HashSet<String> = ["slot-b".to_string()].into_iter().collect();
        registry.retain_slots(&live);

        assert_eq!(registry.len(), 1);
        assert!(registry.get("slot-a").is_none());
        assert!(registry.get("slot-b").is_some());
    }

    fn test_viewer(cx: &mut gpui::Context<FileViewer>) -> FileViewer {
        let fs = std::sync::Arc::new(notmux_files::project_fs::LocalProjectFs::new(
            std::path::PathBuf::from("/tmp"),
        ));
        FileViewer::new_embedded(
            std::path::PathBuf::from("/tmp/a.rs"),
            fs,
            14.0,
            true,
            notmux_core::theme::DARK_THEME,
            false,
            cx,
        )
    }
}
