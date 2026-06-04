pub mod close_worktree_dialog;
pub mod diff_viewer;
pub mod git_header;
pub mod project_header;
pub mod settings;
pub mod simple_input;
pub mod watcher;
pub mod worktree_dialog;

gpui::actions!(notmux_views_git, [Cancel]);
