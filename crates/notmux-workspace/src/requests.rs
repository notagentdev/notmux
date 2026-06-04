//! UI request types for transient view-to-view communication.
//!
//! These types describe UI interactions (context menus, overlays, rename dialogs)
//! and are never persisted. They flow through `Workspace`'s request queues.

/// Target kind for `OverlayRequest::ExplorerContextMenu`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExplorerKind {
    /// Right-click on a file row.
    File,
    /// Right-click on a folder row.
    Folder,
    /// Right-click on the empty area below the tree (project root).
    Empty,
}

/// Request to show context menu at a position
#[derive(Clone, Debug)]
pub struct ContextMenuRequest {
    pub project_id: String,
    pub position: gpui::Point<gpui::Pixels>,
}

/// Request to show folder context menu at a position
#[derive(Clone, Debug)]
pub struct FolderContextMenuRequest {
    pub folder_id: String,
    pub folder_name: String,
    pub position: gpui::Point<gpui::Pixels>,
}

/// Requests consumed by RootView::process_pending_requests()
#[derive(Clone, Debug)]
pub enum OverlayRequest {
    ContextMenu {
        project_id: String,
        position: gpui::Point<gpui::Pixels>,
    },
    FolderContextMenu {
        folder_id: String,
        folder_name: String,
        position: gpui::Point<gpui::Pixels>,
    },
    ShellSelector {
        project_id: String,
        terminal_id: String,
        current_shell: notmux_terminal::shell_config::ShellType,
    },
    AddProjectDialog,
    DiffViewer {
        project_id: String,
        file: Option<String>,
        mode: Option<notmux_core::types::DiffMode>,
        commit_message: Option<String>,
        /// Commit list for navigation (prev/next) in the diff viewer.
        commits: Option<Vec<notmux_git::CommitLogEntry>>,
        /// Current index into the commits list.
        commit_index: Option<usize>,
    },
    MainDiffViewer {
        project_id: String,
        file: Option<String>,
        mode: Option<notmux_core::types::DiffMode>,
        commit_message: Option<String>,
        /// Commit list for navigation (prev/next) in the diff viewer.
        commits: Option<Vec<notmux_git::CommitLogEntry>>,
        /// Current index into the commits list.
        commit_index: Option<usize>,
    },
    MainFileViewer {
        project_id: String,
        file: String,
    },
    RemoteConnect,
    RemoteConnectionContextMenu {
        connection_id: String,
        connection_name: String,
        is_pairing: bool,
        position: gpui::Point<gpui::Pixels>,
    },
    TerminalContextMenu {
        terminal_id: String,
        project_id: String,
        layout_path: Vec<usize>,
        position: gpui::Point<gpui::Pixels>,
        has_selection: bool,
        link_url: Option<String>,
    },
    TabContextMenu {
        tab_index: usize,
        num_tabs: usize,
        project_id: String,
        layout_path: Vec<usize>,
        position: gpui::Point<gpui::Pixels>,
    },
    ShowServiceLog {
        project_id: String,
        service_name: String,
    },
    ShowHookTerminal {
        project_id: String,
        terminal_id: String,
    },
    FileSearch {
        project_id: String,
    },
    ContentSearch {
        project_id: String,
    },
    FileBrowser {
        project_id: String,
    },
    ColorPicker {
        project_id: String,
        position: gpui::Point<gpui::Pixels>,
    },
    FolderColorPicker {
        folder_id: String,
        position: gpui::Point<gpui::Pixels>,
    },
    WorktreeList {
        project_id: String,
        position: gpui::Point<gpui::Pixels>,
    },
    ToggleGitPanel {
        project_id: String,
    },
    GitFileContextMenu {
        project_id: String,
        file_path: String,
        is_staged: bool,
        is_untracked: bool,
        is_conflict: bool,
        position: gpui::Point<gpui::Pixels>,
    },
    GitOverflowMenu {
        project_id: String,
        position: gpui::Point<gpui::Pixels>,
        has_staged: bool,
        has_unstaged: bool,
        has_tracked: bool,
        has_untracked: bool,
        has_stash: bool,
    },
    GitStashList {
        project_id: String,
        position: gpui::Point<gpui::Pixels>,
    },
    ExplorerContextMenu {
        kind: ExplorerKind,
        /// Path of the clicked row, or the project root for `Empty`.
        path: std::path::PathBuf,
        /// Directory used as parent for New File / New Folder / Paste.
        parent_dir: std::path::PathBuf,
        /// Whether the explorer clipboard currently holds an entry (toggles
        /// the Paste visibility in the menu).
        has_clipboard: bool,
        position: gpui::Point<gpui::Pixels>,
    },
}

/// Requests consumed by Sidebar::render()
#[derive(Clone, Debug)]
pub enum SidebarRequest {
    RenameProject {
        project_id: String,
        project_name: String,
    },
    RenameFolder {
        folder_id: String,
        folder_name: String,
    },
    CreateFolder,
    QuickCreateWorktree {
        project_id: String,
    },
}
