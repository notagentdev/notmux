//! Overlay management utilities and OverlayManager Entity.
//!
//! Provides traits, helpers, and a centralized manager for modal overlay components
//! with consistent toggle and close behavior.

use gpui::*;

use std::path::PathBuf;

use crate::remote::GlobalRemoteInfo;
use crate::remote_client::manager::RemoteConnectionManager;
use crate::terminal::shell_config::ShellType;
use crate::views::overlays::add_project_dialog::{AddProjectDialog, AddProjectDialogEvent};
use crate::views::overlays::close_worktree_dialog::{
    CloseWorktreeDialog, CloseWorktreeDialogEvent,
};
use crate::views::overlays::command_palette::{CommandPalette, CommandPaletteEvent};
use crate::views::overlays::content_search::{ContentSearchDialog, ContentSearchDialogEvent};
use crate::views::overlays::context_menu::{ContextMenu, ContextMenuEvent};
use crate::views::overlays::diff_viewer::{DiffViewer, DiffViewerEvent};
use crate::views::overlays::explorer_context_menu::{
    ExplorerContextMenu, ExplorerContextMenuEvent,
};
use crate::views::overlays::file_search::{FileSearchDialog, FileSearchDialogEvent};
use crate::views::overlays::file_viewer::{FileViewer, FileViewerEvent};
use crate::views::overlays::folder_context_menu::{FolderContextMenu, FolderContextMenuEvent};
use crate::views::overlays::git_file_context_menu::{GitFileContextMenu, GitFileContextMenuEvent};
use crate::views::overlays::git_overflow_menu::{GitOverflowMenu, GitOverflowMenuEvent};
use crate::views::overlays::git_stash_list::{GitStashList, GitStashListEvent};
use crate::views::overlays::hook_log::{HookLog, HookLogEvent};
use crate::views::overlays::keybindings_help::{KeybindingsHelp, KeybindingsHelpEvent};
use crate::views::overlays::pairing_dialog::{PairingDialog, PairingDialogEvent};
use crate::views::overlays::remote_connect_dialog::{
    RemoteConnectDialog, RemoteConnectDialogEvent,
};
use crate::views::overlays::remote_context_menu::{RemoteContextMenu, RemoteContextMenuEvent};
use crate::views::overlays::remote_pair_dialog::{RemotePairDialog, RemotePairDialogEvent};
use crate::views::overlays::rename_directory_dialog::{
    RenameDirectoryDialog, RenameDirectoryDialogEvent,
};
use crate::views::overlays::session_manager::{SessionManager, SessionManagerEvent};
use crate::views::overlays::settings_panel::{SettingsPanel, SettingsPanelEvent};
use crate::views::overlays::tab_context_menu::{TabContextMenu, TabContextMenuEvent};
use crate::views::overlays::terminal_context_menu::{
    TerminalContextMenu, TerminalContextMenuEvent,
};
use crate::views::overlays::theme_selector::{ThemeSelector, ThemeSelectorEvent};
use crate::views::overlays::worktree_dialog::{WorktreeDialog, WorktreeDialogEvent};
use crate::views::overlays::{
    ProjectSwitcher, ProjectSwitcherEvent, ShellSelectorOverlay, ShellSelectorOverlayEvent,
};
use crate::workspace::request_broker::RequestBroker;
use crate::workspace::requests::{
    ContextMenuRequest, FolderContextMenuRequest, OverlayRequest, SidebarRequest,
};
use crate::workspace::state::{Workspace, WorkspaceData};
use notmux_core::client::RemoteConnectionConfig;
use notmux_views_sidebar::{ColorPickerPopover, ColorPickerPopoverEvent, ColorPickerTarget};
use notmux_views_sidebar::{WorktreeListPopover, WorktreeListPopoverEvent};

// Re-export generic overlay utilities from notmux-ui
pub use notmux_ui::overlay::{CloseEvent, OverlaySlot};
pub use notmux_ui::toggle_overlay;

// CloseEvent impls for overlay events defined in src/ (local types)

impl CloseEvent for AddProjectDialogEvent {
    fn is_close(&self) -> bool {
        matches!(self, Self::Close)
    }
}
impl CloseEvent for KeybindingsHelpEvent {
    fn is_close(&self) -> bool {
        matches!(self, Self::Close)
    }
}
impl CloseEvent for ThemeSelectorEvent {
    fn is_close(&self) -> bool {
        matches!(self, Self::Close)
    }
}
impl CloseEvent for CommandPaletteEvent {
    fn is_close(&self) -> bool {
        matches!(self, Self::Close)
    }
}
impl CloseEvent for SettingsPanelEvent {
    fn is_close(&self) -> bool {
        matches!(self, Self::Close)
    }
}
impl CloseEvent for PairingDialogEvent {
    fn is_close(&self) -> bool {
        matches!(self, Self::Close)
    }
}

// ============================================================================
// OverlayManager Entity
// ============================================================================

/// Events emitted by OverlayManager that require handling by RootView.
///
/// These events are forwarded from individual overlays when they require
/// actions that need access to RootView's state (terminals, PTY manager, etc.)
#[derive(Clone)]
pub enum OverlayManagerEvent {
    /// Session manager requested workspace switch
    SwitchWorkspace(WorkspaceData),

    /// Worktree dialog created a new project
    WorktreeCreated(String),

    /// Shell selector selected a shell for a terminal
    ShellSelected {
        shell_type: ShellType,
        project_id: String,
        terminal_id: String,
    },

    /// Context menu: Add terminal to project
    AddTerminal { project_id: String },

    /// Context menu: Create worktree from project
    CreateWorktree {
        project_id: String,
        project_path: String,
    },

    /// Context menu: Rename project
    RenameProject {
        project_id: String,
        project_name: String,
    },

    /// Context menu: Rename directory on disk
    RenameDirectory {
        project_id: String,
        project_path: String,
    },

    /// Context menu: Close worktree project
    CloseWorktree { project_id: String },

    /// Context menu: Delete project
    DeleteProject { project_id: String },

    /// Context menu: Configure hooks for a project
    ConfigureHooks { project_id: String },

    /// Context menu: Quick create worktree (one-click)
    QuickCreateWorktree { project_id: String },

    /// Color picker: project color was changed (for remote sync)
    ProjectColorChanged {
        project_id: String,
        color: notmux_core::theme::FolderColor,
    },

    /// Context menu: Reload services (notmux.yaml) for a project
    ReloadServices { project_id: String },

    /// Context menu: Focus parent project of a worktree
    FocusParent { project_id: String },

    /// Project switcher: Focus a specific project
    FocusProject(String),

    /// Project switcher: Toggle project overview visibility
    ToggleProjectVisibility(String),

    /// Remote connect dialog: connection paired and ready
    RemoteConnected { config: RemoteConnectionConfig },

    /// Remote context menu: reconnect to a connection
    RemoteReconnect { connection_id: String },

    /// Remote context menu: open pair dialog
    RemotePair {
        connection_id: String,
        connection_name: String,
    },

    /// Remote pair dialog: user submitted a code
    RemotePaired { connection_id: String, code: String },

    /// Remote context menu: remove a connection
    RemoteRemoveConnection { connection_id: String },

    /// Terminal context menu: copy
    TerminalCopy { terminal_id: String },
    /// Terminal context menu: paste
    TerminalPaste { terminal_id: String },
    /// Terminal context menu: clear
    TerminalClear { terminal_id: String },
    /// Terminal context menu: select all
    TerminalSelectAll { terminal_id: String },
    /// Terminal context menu: split
    TerminalSplit {
        project_id: String,
        layout_path: Vec<usize>,
        direction: crate::workspace::state::SplitDirection,
    },
    /// Terminal context menu: close terminal
    TerminalClose {
        project_id: String,
        terminal_id: String,
    },

    /// Tab context menu: close tab
    TabClose {
        project_id: String,
        layout_path: Vec<usize>,
        tab_index: usize,
    },
    /// Tab context menu: close other tabs
    TabCloseOthers {
        project_id: String,
        layout_path: Vec<usize>,
        tab_index: usize,
    },
    /// Tab context menu: close tabs to the right
    TabCloseToRight {
        project_id: String,
        layout_path: Vec<usize>,
        tab_index: usize,
    },

    /// Git file context menu: stage a file
    GitFileStage {
        project_id: String,
        file_path: String,
    },
    /// Git file context menu: unstage a file
    GitFileUnstage {
        project_id: String,
        file_path: String,
    },
    /// Git file context menu: discard changes / trash untracked
    GitFileDiscard {
        project_id: String,
        file_path: String,
        is_untracked: bool,
    },
    /// Git file context menu: add to .gitignore
    GitFileAddToGitignore {
        project_id: String,
        file_path: String,
    },

    /// Git overflow menu: stage all
    GitStageAll { project_id: String },
    /// Git overflow menu: unstage all
    GitUnstageAll { project_id: String },
    /// Git overflow menu: stash all (tracked + untracked)
    GitStashAll { project_id: String },
    /// Git overflow menu: stash pop (top of stack)
    GitStashPop { project_id: String },
    /// Git overflow menu: discard all tracked (post-confirmation)
    GitDiscardAllTracked { project_id: String },
    /// Stash list overlay closed; the panel should refresh `has_stash`.
    GitStashRefresh { project_id: String },

    /// Explorer context menu: start inline input for a new file inside `parent`.
    ExplorerNewFile { parent: std::path::PathBuf },
    /// Explorer context menu: start inline input for a new folder inside `parent`.
    ExplorerNewFolder { parent: std::path::PathBuf },
    /// Explorer context menu: start inline rename on `target`.
    ExplorerRename { target: std::path::PathBuf },
    /// Explorer context menu: delete `path` (synchronous from disk).
    ExplorerDelete {
        path: std::path::PathBuf,
        is_dir: bool,
    },
    /// Explorer context menu: reveal `path` in the platform file manager.
    ExplorerReveal { path: std::path::PathBuf },
    /// Explorer context menu: paste the clipboard entry into `target_dir`.
    ExplorerPaste { target_dir: std::path::PathBuf },
    /// Explorer context menu: append the file's relative path to .gitignore.
    ExplorerAddToGitignore { path: std::path::PathBuf },
    /// Explorer context menu: closed (clear the per-row highlight in the tree).
    ExplorerContextMenuClosed,
}

/// Centralized overlay manager that handles all modal overlays.
///
/// Uses a single `active_modal` slot to enforce mutual exclusion -
/// only one modal can be open at a time. Context menus remain as
/// separate slots since they are positioned popups, not full-screen modals.
pub struct OverlayManager {
    workspace: Entity<Workspace>,
    request_broker: Entity<RequestBroker>,

    /// The single active modal overlay (only one can be open at a time).
    active_modal: Option<AnyView>,

    /// TypeId of the active modal for toggle detection.
    modal_type_id: Option<std::any::TypeId>,

    /// Settings panel rendered in the main content area, not as a modal overlay.
    settings_panel: Option<Entity<SettingsPanel>>,

    // Context menus remain separate (positioned popups, not full-screen modals)
    context_menu: OverlaySlot<ContextMenu>,
    folder_context_menu: OverlaySlot<FolderContextMenu>,
    explorer_context_menu: OverlaySlot<ExplorerContextMenu>,
    git_file_context_menu: OverlaySlot<GitFileContextMenu>,
    git_overflow_menu: OverlaySlot<GitOverflowMenu>,
    git_stash_list: OverlaySlot<GitStashList>,
    remote_context_menu: OverlaySlot<RemoteContextMenu>,
    terminal_context_menu: OverlaySlot<TerminalContextMenu>,
    tab_context_menu: OverlaySlot<TabContextMenu>,

    // Positioned popovers (like context menus, rendered at RootView level)
    worktree_list: OverlaySlot<WorktreeListPopover>,
    color_picker: OverlaySlot<ColorPickerPopover>,

    /// Cached file viewer entities per project name (survives close/reopen).
    cached_file_viewers: std::collections::HashMap<String, Entity<FileViewer>>,
}

impl OverlayManager {
    /// Create a new OverlayManager.
    pub fn new(workspace: Entity<Workspace>, request_broker: Entity<RequestBroker>) -> Self {
        Self {
            workspace,
            request_broker,
            active_modal: None,
            modal_type_id: None,
            settings_panel: None,
            cached_file_viewers: std::collections::HashMap::new(),
            context_menu: OverlaySlot::new(),
            folder_context_menu: OverlaySlot::new(),
            explorer_context_menu: OverlaySlot::new(),
            git_file_context_menu: OverlaySlot::new(),
            git_overflow_menu: OverlaySlot::new(),
            git_stash_list: OverlaySlot::new(),
            remote_context_menu: OverlaySlot::new(),
            terminal_context_menu: OverlaySlot::new(),
            tab_context_menu: OverlaySlot::new(),
            worktree_list: OverlaySlot::new(),
            color_picker: OverlaySlot::new(),
        }
    }

    // ========================================================================
    // Modal management helpers
    // ========================================================================

    /// Close the active modal, restoring terminal focus if needed.
    fn close_modal(&mut self, cx: &mut Context<Self>) {
        if self.active_modal.is_some() {
            self.active_modal = None;
            self.modal_type_id = None;
            self.workspace
                .update(cx, |ws, cx| ws.restore_focused_terminal(cx));
            cx.notify();
        }
    }

    /// Hide the active modal without dropping it (used for cached overlays like FileViewer).
    fn hide_modal(&mut self, cx: &mut Context<Self>) {
        if self.active_modal.is_some() {
            self.active_modal = None;
            self.modal_type_id = None;
            self.workspace
                .update(cx, |ws, cx| ws.restore_focused_terminal(cx));
            cx.notify();
        }
    }

    /// Check if the active modal is of a specific type.
    fn is_modal<T: 'static>(&self) -> bool {
        self.modal_type_id == Some(std::any::TypeId::of::<T>())
    }

    /// Open a modal, closing any existing one first.
    ///
    /// Automatically clears terminal focus so keyboard input goes to the modal.
    fn open_modal<T: Render + 'static>(&mut self, entity: Entity<T>, cx: &mut Context<Self>) {
        self.close_modal(cx);
        self.active_modal = Some(entity.into());
        self.modal_type_id = Some(std::any::TypeId::of::<T>());
        self.workspace
            .update(cx, |ws, cx| ws.clear_focused_terminal(cx));
        cx.notify();
    }

    /// Get the active modal for rendering.
    pub fn render_modal(&self) -> Option<AnyView> {
        self.active_modal.clone()
    }

    /// Get the settings panel for rendering in the main content area.
    pub fn render_settings_panel(&self) -> Option<Entity<SettingsPanel>> {
        self.settings_panel.clone()
    }

    /// Check if the embedded settings panel is open.
    pub fn has_settings_panel(&self) -> bool {
        self.settings_panel.is_some()
    }

    pub fn close_settings_panel(&mut self, cx: &mut Context<Self>) {
        if self.settings_panel.take().is_some() {
            self.workspace
                .update(cx, |ws, cx| ws.restore_focused_terminal(cx));
            cx.notify();
        }
    }

    fn open_settings_panel(&mut self, entity: Entity<SettingsPanel>, cx: &mut Context<Self>) {
        self.close_modal(cx);
        self.settings_panel = Some(entity);
        self.workspace
            .update(cx, |ws, cx| ws.clear_focused_terminal(cx));
        cx.notify();
    }

    // ========================================================================
    // Context menu visibility checks (kept separate)
    // ========================================================================

    /// Close all context menu slots (mutual exclusion).
    fn close_all_context_menus(&mut self) {
        self.context_menu.close();
        self.explorer_context_menu.close();
        self.folder_context_menu.close();
        self.git_file_context_menu.close();
        self.git_overflow_menu.close();
        self.git_stash_list.close();
        self.remote_context_menu.close();
        self.terminal_context_menu.close();
        self.tab_context_menu.close();
        self.worktree_list.close();
        self.color_picker.close();
    }

    /// Check if context menu is open.
    pub fn has_context_menu(&self) -> bool {
        self.context_menu.is_open()
    }

    /// Check if folder context menu is open.
    pub fn has_folder_context_menu(&self) -> bool {
        self.folder_context_menu.is_open()
    }

    /// Check if terminal context menu is open.
    pub fn has_terminal_context_menu(&self) -> bool {
        self.terminal_context_menu.is_open()
    }

    /// Check if tab context menu is open.
    pub fn has_tab_context_menu(&self) -> bool {
        self.tab_context_menu.is_open()
    }

    // ========================================================================
    // Simple toggle overlays
    // ========================================================================

    /// Toggle add project dialog overlay.
    pub fn toggle_add_project_dialog(
        &mut self,
        remote_manager: Option<Entity<RemoteConnectionManager>>,
        cx: &mut Context<Self>,
    ) {
        if self.is_modal::<AddProjectDialog>() {
            self.close_modal(cx);
        } else {
            let workspace = self.workspace.clone();
            let entity = cx.new(|cx| AddProjectDialog::new(workspace, remote_manager, cx));
            cx.subscribe(&entity, |this, _, event: &AddProjectDialogEvent, cx| {
                if event.is_close() {
                    this.close_modal(cx);
                }
            })
            .detach();
            self.open_modal(entity, cx);
        }
        cx.notify();
    }

    /// Toggle keybindings help overlay.
    pub fn toggle_keybindings_help(&mut self, cx: &mut Context<Self>) {
        if self.is_modal::<KeybindingsHelp>() {
            self.close_modal(cx);
        } else {
            let entity = cx.new(KeybindingsHelp::new);
            cx.subscribe(
                &entity,
                |this, _, event: &KeybindingsHelpEvent, cx| match event {
                    KeybindingsHelpEvent::Close => {
                        this.close_modal(cx);
                    }
                    KeybindingsHelpEvent::ReloadBindings => {
                        crate::keybindings::reload_keybindings(cx);
                    }
                },
            )
            .detach();
            self.open_modal(entity, cx);
        }
        cx.notify();
    }

    /// Toggle theme selector overlay.
    pub fn toggle_theme_selector(&mut self, cx: &mut Context<Self>) {
        toggle_overlay!(
            self,
            cx,
            ThemeSelector,
            ThemeSelectorEvent,
            ThemeSelector::new
        );
    }

    /// Toggle command palette overlay.
    pub fn toggle_command_palette(&mut self, cx: &mut Context<Self>) {
        if self.is_modal::<CommandPalette>() {
            self.close_modal(cx);
        } else {
            let ws = self.workspace.clone();
            let entity = cx.new(|cx| CommandPalette::new(ws, cx));
            cx.subscribe(&entity, |this, _, event: &CommandPaletteEvent, cx| {
                match event {
                    CommandPaletteEvent::Close => {
                        this.close_modal(cx);
                    }
                }
            })
            .detach();
            self.open_modal(entity, cx);
        }
        cx.notify();
    }

    /// Toggle settings panel in the main content area.
    pub fn toggle_settings_panel(&mut self, cx: &mut Context<Self>) {
        if self.has_settings_panel() {
            self.close_settings_panel(cx);
        } else {
            let workspace = self.workspace.clone();
            let entity = cx.new(|cx| SettingsPanel::new(workspace, cx));
            cx.subscribe(&entity, |this, _, event: &SettingsPanelEvent, cx| {
                if event.is_close() {
                    this.close_settings_panel(cx);
                }
            })
            .detach();
            self.open_settings_panel(entity, cx);
        }
        cx.notify();
    }

    /// Toggle hook log overlay.
    pub fn toggle_hook_log(&mut self, cx: &mut Context<Self>) {
        toggle_overlay!(self, cx, HookLog, HookLogEvent, HookLog::new);
    }

    /// Toggle pairing dialog overlay.
    pub fn toggle_pairing_dialog(&mut self, cx: &mut Context<Self>) {
        if self.is_modal::<PairingDialog>() {
            self.close_modal(cx);
        } else if let Some(remote_info) = cx.try_global::<GlobalRemoteInfo>()
            && let Some(auth_store) = remote_info.0.auth_store()
        {
            let entity = cx.new(|cx| PairingDialog::new(auth_store, cx));
            cx.subscribe(&entity, |this, _, event: &PairingDialogEvent, cx| {
                if event.is_close() {
                    this.close_modal(cx);
                }
            })
            .detach();
            self.open_modal(entity, cx);
        }
        cx.notify();
    }

    /// Show settings panel opened to Hooks category for a specific project.
    pub fn show_settings_for_project(&mut self, project_id: String, cx: &mut Context<Self>) {
        let workspace = self.workspace.clone();
        let entity = cx.new(|cx| SettingsPanel::new_for_project(workspace, project_id, cx));
        cx.subscribe(&entity, |this, _, event: &SettingsPanelEvent, cx| {
            if event.is_close() {
                this.close_settings_panel(cx);
            }
        })
        .detach();
        self.open_settings_panel(entity, cx);
        cx.notify();
    }

    /// Toggle project switcher overlay.
    pub fn toggle_project_switcher(&mut self, cx: &mut Context<Self>) {
        if self.is_modal::<ProjectSwitcher>() {
            self.close_modal(cx);
        } else {
            let workspace = self.workspace.clone();
            let entity = cx.new(|cx| ProjectSwitcher::new(workspace, cx));
            cx.subscribe(
                &entity,
                |this, _, event: &ProjectSwitcherEvent, cx| match event {
                    ProjectSwitcherEvent::Close => {
                        this.close_modal(cx);
                    }
                    ProjectSwitcherEvent::FocusProject(project_id) => {
                        cx.emit(OverlayManagerEvent::FocusProject(project_id.clone()));
                        this.close_modal(cx);
                    }
                    ProjectSwitcherEvent::ToggleVisibility(project_id) => {
                        cx.emit(OverlayManagerEvent::ToggleProjectVisibility(
                            project_id.clone(),
                        ));
                        cx.notify();
                    }
                },
            )
            .detach();
            self.open_modal(entity, cx);
        }
        cx.notify();
    }

    // ========================================================================
    // Session manager (complex - emits SwitchWorkspace event)
    // ========================================================================

    /// Toggle session manager overlay.
    pub fn toggle_session_manager(&mut self, cx: &mut Context<Self>) {
        if self.is_modal::<SessionManager>() {
            self.close_modal(cx);
        } else {
            let workspace = self.workspace.clone();
            let manager = cx.new(|cx| SessionManager::new(workspace, cx));
            cx.subscribe(
                &manager,
                |this, _, event: &SessionManagerEvent, cx| match event {
                    SessionManagerEvent::Close => {
                        this.close_modal(cx);
                    }
                    SessionManagerEvent::SwitchWorkspace(data) => {
                        cx.emit(OverlayManagerEvent::SwitchWorkspace(*data.clone()));
                        this.close_modal(cx);
                    }
                },
            )
            .detach();
            self.open_modal(manager, cx);
        }
        cx.notify();
    }

    // ========================================================================
    // Shell selector (parametric)
    // ========================================================================

    /// Show shell selector overlay for a terminal.
    pub fn show_shell_selector(
        &mut self,
        current_shell: ShellType,
        project_id: String,
        terminal_id: String,
        cx: &mut Context<Self>,
    ) {
        let context = Some((project_id.clone(), terminal_id.clone()));
        let entity = cx.new(|cx| ShellSelectorOverlay::new(current_shell, context, cx));
        cx.subscribe(
            &entity,
            move |this, _, event: &ShellSelectorOverlayEvent, cx| match event {
                ShellSelectorOverlayEvent::Close => {
                    this.close_modal(cx);
                }
                ShellSelectorOverlayEvent::ShellSelected {
                    shell_type,
                    context,
                } => {
                    if let Some((project_id, terminal_id)) = context {
                        cx.emit(OverlayManagerEvent::ShellSelected {
                            shell_type: shell_type.clone(),
                            project_id: project_id.clone(),
                            terminal_id: terminal_id.clone(),
                        });
                    }
                    this.close_modal(cx);
                }
            },
        )
        .detach();
        self.open_modal(entity, cx);
        cx.notify();
    }

    // ========================================================================
    // Worktree dialog (parametric)
    // ========================================================================

    /// Show worktree dialog for a project.
    pub fn show_worktree_dialog(
        &mut self,
        project_id: String,
        project_path: String,
        cx: &mut Context<Self>,
    ) {
        let workspace = self.workspace.clone();
        let app_settings = crate::settings::settings(cx);
        let dialog = cx.new(|cx| {
            WorktreeDialog::new(
                workspace,
                project_id,
                project_path,
                app_settings.worktree,
                app_settings.hooks,
                cx,
            )
        });
        cx.subscribe(
            &dialog,
            |this, _, event: &WorktreeDialogEvent, cx| match event {
                WorktreeDialogEvent::Close => {
                    this.close_modal(cx);
                }
                WorktreeDialogEvent::Created(new_project_id) => {
                    cx.emit(OverlayManagerEvent::WorktreeCreated(new_project_id.clone()));
                    this.close_modal(cx);
                }
            },
        )
        .detach();
        self.open_modal(dialog, cx);
        cx.notify();
    }

    // ========================================================================
    // Close worktree dialog (parametric)
    // ========================================================================

    /// Show close worktree confirmation dialog.
    pub fn show_close_worktree_dialog(&mut self, project_id: String, cx: &mut Context<Self>) {
        let workspace = self.workspace.clone();
        let app_settings = crate::settings::settings(cx);
        let dialog = cx.new(|cx| {
            CloseWorktreeDialog::new(
                workspace,
                project_id,
                app_settings.worktree,
                app_settings.hooks,
                cx,
            )
        });
        cx.subscribe(&dialog, |this, _, event: &CloseWorktreeDialogEvent, cx| {
            if event.is_close() {
                this.close_modal(cx);
            }
        })
        .detach();
        self.open_modal(dialog, cx);
        cx.notify();
    }

    // ========================================================================
    // Rename directory dialog (parametric)
    // ========================================================================

    /// Show rename directory dialog for a project.
    pub fn show_rename_directory_dialog(
        &mut self,
        project_id: String,
        project_path: String,
        cx: &mut Context<Self>,
    ) {
        let workspace = self.workspace.clone();
        let dialog =
            cx.new(|cx| RenameDirectoryDialog::new(workspace, project_id, project_path, cx));
        cx.subscribe(
            &dialog,
            |this, _, event: &RenameDirectoryDialogEvent, cx| {
                if event.is_close() {
                    this.close_modal(cx);
                }
            },
        )
        .detach();
        self.open_modal(dialog, cx);
        cx.notify();
    }

    // ========================================================================
    // Context menu (parametric - remains as separate OverlaySlot)
    // ========================================================================

    /// Show context menu for a project.
    pub fn show_context_menu(&mut self, request: ContextMenuRequest, cx: &mut Context<Self>) {
        self.close_modal(cx);
        self.close_all_context_menus();

        let workspace = self.workspace.clone();
        let menu = cx.new(|cx| ContextMenu::new(workspace.clone(), request, cx));

        cx.subscribe(&menu, |this, _, event: &ContextMenuEvent, cx| {
            match event {
                ContextMenuEvent::Close => {
                    this.hide_context_menu(cx);
                }
                ContextMenuEvent::AddTerminal { project_id } => {
                    this.hide_context_menu(cx);
                    cx.emit(OverlayManagerEvent::AddTerminal {
                        project_id: project_id.clone(),
                    });
                }
                ContextMenuEvent::CreateWorktree {
                    project_id,
                    project_path,
                } => {
                    this.hide_context_menu(cx);
                    cx.emit(OverlayManagerEvent::CreateWorktree {
                        project_id: project_id.clone(),
                        project_path: project_path.clone(),
                    });
                }
                ContextMenuEvent::RenameProject {
                    project_id,
                    project_name,
                } => {
                    this.hide_context_menu(cx);
                    cx.emit(OverlayManagerEvent::RenameProject {
                        project_id: project_id.clone(),
                        project_name: project_name.clone(),
                    });
                }
                ContextMenuEvent::RenameDirectory {
                    project_id,
                    project_path,
                } => {
                    this.hide_context_menu(cx);
                    cx.emit(OverlayManagerEvent::RenameDirectory {
                        project_id: project_id.clone(),
                        project_path: project_path.clone(),
                    });
                }
                ContextMenuEvent::CloseWorktree { project_id } => {
                    this.hide_context_menu(cx);
                    cx.emit(OverlayManagerEvent::CloseWorktree {
                        project_id: project_id.clone(),
                    });
                }
                ContextMenuEvent::DeleteProject { project_id } => {
                    this.hide_context_menu(cx);
                    cx.emit(OverlayManagerEvent::DeleteProject {
                        project_id: project_id.clone(),
                    });
                }
                ContextMenuEvent::ConfigureHooks { project_id } => {
                    this.hide_context_menu(cx);
                    cx.emit(OverlayManagerEvent::ConfigureHooks {
                        project_id: project_id.clone(),
                    });
                }
                ContextMenuEvent::QuickCreateWorktree { project_id } => {
                    this.hide_context_menu(cx);
                    cx.emit(OverlayManagerEvent::QuickCreateWorktree {
                        project_id: project_id.clone(),
                    });
                }
                ContextMenuEvent::ManageWorktrees {
                    project_id,
                    position,
                } => {
                    this.hide_context_menu(cx);
                    this.show_worktree_list(project_id.clone(), *position, cx);
                }
                ContextMenuEvent::ReloadServices { project_id } => {
                    this.hide_context_menu(cx);
                    cx.emit(OverlayManagerEvent::ReloadServices {
                        project_id: project_id.clone(),
                    });
                }
                ContextMenuEvent::FocusParent { project_id } => {
                    this.hide_context_menu(cx);
                    cx.emit(OverlayManagerEvent::FocusParent {
                        project_id: project_id.clone(),
                    });
                }
                ContextMenuEvent::CopyPath { .. } => {
                    // Path already copied to clipboard in the handler
                    this.hide_context_menu(cx);
                }
                ContextMenuEvent::BrowseFiles { project_id } => {
                    this.hide_context_menu(cx);
                    this.request_broker.update(cx, |broker, cx| {
                        broker.push_overlay_request(
                            OverlayRequest::FileBrowser {
                                project_id: project_id.clone(),
                            },
                            cx,
                        );
                    });
                }
                ContextMenuEvent::ShowDiff { project_id } => {
                    this.hide_context_menu(cx);
                    this.request_broker.update(cx, |broker, cx| {
                        broker.push_overlay_request(
                            OverlayRequest::DiffViewer {
                                project_id: project_id.clone(),
                                file: None,
                                mode: None,
                                commit_message: None,
                                commits: None,
                                commit_index: None,
                            },
                            cx,
                        );
                    });
                }
                ContextMenuEvent::FocusProject { project_id } => {
                    this.hide_context_menu(cx);
                    cx.emit(OverlayManagerEvent::FocusProject(project_id.clone()));
                }
                ContextMenuEvent::HideProject { project_id } => {
                    this.hide_context_menu(cx);
                    cx.emit(OverlayManagerEvent::ToggleProjectVisibility(
                        project_id.clone(),
                    ));
                }
                ContextMenuEvent::CreateFolder => {
                    this.hide_context_menu(cx);
                    this.request_broker.update(cx, |broker, cx| {
                        broker.push_sidebar_request(SidebarRequest::CreateFolder, cx);
                    });
                }
            }
        })
        .detach();

        self.context_menu.set(menu);
        cx.notify();
    }

    /// Hide context menu.
    pub fn hide_context_menu(&mut self, cx: &mut Context<Self>) {
        self.context_menu.close();
        cx.notify();
    }

    /// Show folder context menu.
    pub fn show_folder_context_menu(
        &mut self,
        request: FolderContextMenuRequest,
        cx: &mut Context<Self>,
    ) {
        self.close_modal(cx);
        self.close_all_context_menus();

        let workspace = self.workspace.clone();
        let menu = cx.new(|cx| FolderContextMenu::new(workspace.clone(), request, cx));

        cx.subscribe(
            &menu,
            |this, _, event: &FolderContextMenuEvent, cx| match event {
                FolderContextMenuEvent::Close => {
                    this.hide_folder_context_menu(cx);
                }
                FolderContextMenuEvent::RenameFolder {
                    folder_id,
                    folder_name,
                } => {
                    this.hide_folder_context_menu(cx);
                    this.request_broker.update(cx, |broker, cx| {
                        broker.push_sidebar_request(
                            SidebarRequest::RenameFolder {
                                folder_id: folder_id.clone(),
                                folder_name: folder_name.clone(),
                            },
                            cx,
                        );
                    });
                }
                FolderContextMenuEvent::DeleteFolder { folder_id } => {
                    this.hide_folder_context_menu(cx);
                    this.workspace.update(cx, |ws, cx| {
                        ws.delete_folder(folder_id, cx);
                    });
                }
                FolderContextMenuEvent::FilterToFolder { folder_id } => {
                    this.hide_folder_context_menu(cx);
                    this.workspace.update(cx, |ws, cx| {
                        ws.toggle_folder_focus(folder_id, cx);
                    });
                }
            },
        )
        .detach();

        self.folder_context_menu.set(menu);
        cx.notify();
    }

    /// Hide folder context menu.
    pub fn hide_folder_context_menu(&mut self, cx: &mut Context<Self>) {
        self.folder_context_menu.close();
        cx.notify();
    }

    // ========================================================================
    // Remote connection context menu (positioned popup)
    // ========================================================================

    /// Check if remote context menu is open.
    pub fn has_remote_context_menu(&self) -> bool {
        self.remote_context_menu.is_open()
    }

    /// Show remote connection context menu.
    pub fn show_remote_context_menu(
        &mut self,
        connection_id: String,
        connection_name: String,
        is_pairing: bool,
        position: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.close_modal(cx);
        self.close_all_context_menus();

        let conn_name = connection_name.clone();
        let menu = cx.new(|cx| {
            RemoteContextMenu::new(connection_id, connection_name, is_pairing, position, cx)
        });

        cx.subscribe(
            &menu,
            move |this, _, event: &RemoteContextMenuEvent, cx| match event {
                RemoteContextMenuEvent::Close => {
                    this.hide_remote_context_menu(cx);
                }
                RemoteContextMenuEvent::Reconnect { connection_id } => {
                    this.hide_remote_context_menu(cx);
                    cx.emit(OverlayManagerEvent::RemoteReconnect {
                        connection_id: connection_id.clone(),
                    });
                }
                RemoteContextMenuEvent::Pair { connection_id } => {
                    this.hide_remote_context_menu(cx);
                    cx.emit(OverlayManagerEvent::RemotePair {
                        connection_id: connection_id.clone(),
                        connection_name: conn_name.clone(),
                    });
                }
                RemoteContextMenuEvent::RemoveConnection { connection_id } => {
                    this.hide_remote_context_menu(cx);
                    cx.emit(OverlayManagerEvent::RemoteRemoveConnection {
                        connection_id: connection_id.clone(),
                    });
                }
            },
        )
        .detach();

        self.remote_context_menu.set(menu);
        cx.notify();
    }

    /// Hide remote context menu.
    pub fn hide_remote_context_menu(&mut self, cx: &mut Context<Self>) {
        self.remote_context_menu.close();
        cx.notify();
    }

    /// Get remote context menu entity for rendering.
    pub fn render_remote_context_menu(&self) -> Option<Entity<RemoteContextMenu>> {
        self.remote_context_menu.render()
    }

    // ========================================================================
    // Terminal context menu (positioned popup)
    // ========================================================================

    /// Show terminal context menu.
    #[allow(clippy::too_many_arguments)]
    pub fn show_terminal_context_menu(
        &mut self,
        terminal_id: String,
        project_id: String,
        layout_path: Vec<usize>,
        position: gpui::Point<gpui::Pixels>,
        has_selection: bool,
        link_url: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.close_modal(cx);
        self.close_all_context_menus();

        let menu = cx.new(|cx| {
            TerminalContextMenu::new(
                terminal_id,
                project_id,
                layout_path,
                position,
                has_selection,
                link_url,
                cx,
            )
        });

        cx.subscribe(
            &menu,
            |this, _, event: &TerminalContextMenuEvent, cx| match event {
                TerminalContextMenuEvent::Close => {
                    this.hide_terminal_context_menu(cx);
                }
                TerminalContextMenuEvent::Copy { terminal_id } => {
                    this.hide_terminal_context_menu(cx);
                    cx.emit(OverlayManagerEvent::TerminalCopy {
                        terminal_id: terminal_id.clone(),
                    });
                }
                TerminalContextMenuEvent::Paste { terminal_id } => {
                    this.hide_terminal_context_menu(cx);
                    cx.emit(OverlayManagerEvent::TerminalPaste {
                        terminal_id: terminal_id.clone(),
                    });
                }
                TerminalContextMenuEvent::Clear { terminal_id } => {
                    this.hide_terminal_context_menu(cx);
                    cx.emit(OverlayManagerEvent::TerminalClear {
                        terminal_id: terminal_id.clone(),
                    });
                }
                TerminalContextMenuEvent::SelectAll { terminal_id } => {
                    this.hide_terminal_context_menu(cx);
                    cx.emit(OverlayManagerEvent::TerminalSelectAll {
                        terminal_id: terminal_id.clone(),
                    });
                }
                TerminalContextMenuEvent::Split {
                    project_id,
                    layout_path,
                    direction,
                } => {
                    this.hide_terminal_context_menu(cx);
                    cx.emit(OverlayManagerEvent::TerminalSplit {
                        project_id: project_id.clone(),
                        layout_path: layout_path.clone(),
                        direction: *direction,
                    });
                }
                TerminalContextMenuEvent::CloseTerminal {
                    project_id,
                    terminal_id,
                } => {
                    this.hide_terminal_context_menu(cx);
                    cx.emit(OverlayManagerEvent::TerminalClose {
                        project_id: project_id.clone(),
                        terminal_id: terminal_id.clone(),
                    });
                }
                TerminalContextMenuEvent::OpenLink { url } => {
                    this.hide_terminal_context_menu(cx);
                    crate::views::layout::terminal_pane::url_detector::UrlDetector::open_url(url);
                }
                TerminalContextMenuEvent::CopyLink { url } => {
                    this.hide_terminal_context_menu(cx);
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(url.clone()));
                }
            },
        )
        .detach();

        self.terminal_context_menu.set(menu);
        cx.notify();
    }

    /// Hide terminal context menu.
    pub fn hide_terminal_context_menu(&mut self, cx: &mut Context<Self>) {
        self.terminal_context_menu.close();
        cx.notify();
    }

    /// Get terminal context menu entity for rendering.
    pub fn render_terminal_context_menu(&self) -> Option<Entity<TerminalContextMenu>> {
        self.terminal_context_menu.render()
    }

    // ========================================================================
    // Git file context menu (positioned popup)
    // ========================================================================

    /// Check if git file context menu is open.
    pub fn has_git_file_context_menu(&self) -> bool {
        self.git_file_context_menu.is_open()
    }

    /// Show git file context menu.
    #[allow(clippy::too_many_arguments)]
    pub fn show_git_file_context_menu(
        &mut self,
        project_id: String,
        file_path: String,
        is_staged: bool,
        is_untracked: bool,
        is_conflict: bool,
        position: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.close_modal(cx);
        self.close_all_context_menus();

        let menu = cx.new(|cx| {
            GitFileContextMenu::new(
                project_id,
                file_path,
                is_staged,
                is_untracked,
                is_conflict,
                position,
                cx,
            )
        });

        cx.subscribe(
            &menu,
            |this, _, event: &GitFileContextMenuEvent, cx| match event {
                GitFileContextMenuEvent::Close => {
                    this.hide_git_file_context_menu(cx);
                }
                GitFileContextMenuEvent::Stage {
                    project_id,
                    file_path,
                } => {
                    this.hide_git_file_context_menu(cx);
                    cx.emit(OverlayManagerEvent::GitFileStage {
                        project_id: project_id.clone(),
                        file_path: file_path.clone(),
                    });
                }
                GitFileContextMenuEvent::Unstage {
                    project_id,
                    file_path,
                } => {
                    this.hide_git_file_context_menu(cx);
                    cx.emit(OverlayManagerEvent::GitFileUnstage {
                        project_id: project_id.clone(),
                        file_path: file_path.clone(),
                    });
                }
                GitFileContextMenuEvent::Discard {
                    project_id,
                    file_path,
                    is_untracked,
                } => {
                    this.hide_git_file_context_menu(cx);
                    cx.emit(OverlayManagerEvent::GitFileDiscard {
                        project_id: project_id.clone(),
                        file_path: file_path.clone(),
                        is_untracked: *is_untracked,
                    });
                }
                GitFileContextMenuEvent::OpenDiff {
                    project_id,
                    file_path,
                } => {
                    this.hide_git_file_context_menu(cx);
                    this.request_broker.update(cx, |broker, cx| {
                        broker.push_overlay_request(
                            OverlayRequest::DiffViewer {
                                project_id: project_id.clone(),
                                file: Some(file_path.clone()),
                                mode: None,
                                commit_message: None,
                                commits: None,
                                commit_index: None,
                            },
                            cx,
                        );
                    });
                }
                GitFileContextMenuEvent::OpenFile {
                    project_id,
                    file_path: _,
                } => {
                    this.hide_git_file_context_menu(cx);
                    // Route to file browser/viewer — uses the FileBrowser overlay
                    this.request_broker.update(cx, |broker, cx| {
                        broker.push_overlay_request(
                            OverlayRequest::FileBrowser {
                                project_id: project_id.clone(),
                            },
                            cx,
                        );
                    });
                }
                GitFileContextMenuEvent::AddToGitignore {
                    project_id,
                    file_path,
                } => {
                    this.hide_git_file_context_menu(cx);
                    cx.emit(OverlayManagerEvent::GitFileAddToGitignore {
                        project_id: project_id.clone(),
                        file_path: file_path.clone(),
                    });
                }
                GitFileContextMenuEvent::CopyPath { path } => {
                    cx.write_to_clipboard(ClipboardItem::new_string(path.clone()));
                    this.hide_git_file_context_menu(cx);
                }
            },
        )
        .detach();

        self.git_file_context_menu.set(menu);
        cx.notify();
    }

    /// Hide git file context menu.
    pub fn hide_git_file_context_menu(&mut self, cx: &mut Context<Self>) {
        self.git_file_context_menu.close();
        cx.notify();
    }

    /// Get git file context menu entity for rendering.
    pub fn render_git_file_context_menu(&self) -> Option<Entity<GitFileContextMenu>> {
        self.git_file_context_menu.render()
    }

    // ========================================================================
    // Explorer context menu (positioned popup for sidebar file tree)
    // ========================================================================

    pub fn has_explorer_context_menu(&self) -> bool {
        self.explorer_context_menu.is_open()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn show_explorer_context_menu(
        &mut self,
        kind: notmux_workspace::requests::ExplorerKind,
        path: std::path::PathBuf,
        parent_dir: std::path::PathBuf,
        has_clipboard: bool,
        position: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.close_modal(cx);
        self.close_all_context_menus();

        let menu = cx.new(|cx| {
            ExplorerContextMenu::new(kind, path, parent_dir, has_clipboard, position, cx)
        });

        cx.subscribe(
            &menu,
            |this, _, event: &ExplorerContextMenuEvent, cx| match event {
                ExplorerContextMenuEvent::Close => {
                    this.hide_explorer_context_menu(cx);
                }
                ExplorerContextMenuEvent::NewFile { parent } => {
                    this.hide_explorer_context_menu(cx);
                    cx.emit(OverlayManagerEvent::ExplorerNewFile {
                        parent: parent.clone(),
                    });
                }
                ExplorerContextMenuEvent::NewFolder { parent } => {
                    this.hide_explorer_context_menu(cx);
                    cx.emit(OverlayManagerEvent::ExplorerNewFolder {
                        parent: parent.clone(),
                    });
                }
                ExplorerContextMenuEvent::Rename { target } => {
                    this.hide_explorer_context_menu(cx);
                    cx.emit(OverlayManagerEvent::ExplorerRename {
                        target: target.clone(),
                    });
                }
                ExplorerContextMenuEvent::Delete { path, is_dir } => {
                    this.hide_explorer_context_menu(cx);
                    cx.emit(OverlayManagerEvent::ExplorerDelete {
                        path: path.clone(),
                        is_dir: *is_dir,
                    });
                }
                ExplorerContextMenuEvent::CopyPath { path } => {
                    cx.write_to_clipboard(ClipboardItem::new_string(path.to_string_lossy().into()));
                    this.hide_explorer_context_menu(cx);
                }
                ExplorerContextMenuEvent::RevealInFinder { path } => {
                    this.hide_explorer_context_menu(cx);
                    cx.emit(OverlayManagerEvent::ExplorerReveal { path: path.clone() });
                }
                ExplorerContextMenuEvent::Cut { path } => {
                    let cb = cx.global_mut::<notmux_files::clipboard::ExplorerClipboard>();
                    cb.set(path.clone(), notmux_files::clipboard::ClipboardOp::Cut);
                    this.hide_explorer_context_menu(cx);
                }
                ExplorerContextMenuEvent::Copy { path } => {
                    let cb = cx.global_mut::<notmux_files::clipboard::ExplorerClipboard>();
                    cb.set(path.clone(), notmux_files::clipboard::ClipboardOp::Copy);
                    this.hide_explorer_context_menu(cx);
                }
                ExplorerContextMenuEvent::Paste { target_dir } => {
                    this.hide_explorer_context_menu(cx);
                    cx.emit(OverlayManagerEvent::ExplorerPaste {
                        target_dir: target_dir.clone(),
                    });
                }
                ExplorerContextMenuEvent::AddToGitignore { path } => {
                    this.hide_explorer_context_menu(cx);
                    cx.emit(OverlayManagerEvent::ExplorerAddToGitignore { path: path.clone() });
                }
            },
        )
        .detach();

        self.explorer_context_menu.set(menu);
        cx.notify();
    }

    pub fn hide_explorer_context_menu(&mut self, cx: &mut Context<Self>) {
        self.explorer_context_menu.close();
        cx.emit(OverlayManagerEvent::ExplorerContextMenuClosed);
        cx.notify();
    }

    pub fn render_explorer_context_menu(&self) -> Option<Entity<ExplorerContextMenu>> {
        self.explorer_context_menu.render()
    }

    // ========================================================================
    // Git overflow menu (three-dots in panel header)
    // ========================================================================

    /// Check if git overflow menu is open.
    pub fn has_git_overflow_menu(&self) -> bool {
        self.git_overflow_menu.is_open()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn show_git_overflow_menu(
        &mut self,
        project_id: String,
        position: gpui::Point<gpui::Pixels>,
        has_staged: bool,
        has_unstaged: bool,
        has_tracked: bool,
        has_untracked: bool,
        has_stash: bool,
        cx: &mut Context<Self>,
    ) {
        self.close_modal(cx);
        self.close_all_context_menus();

        let menu = cx.new(|cx| {
            GitOverflowMenu::new(
                project_id,
                position,
                has_staged,
                has_unstaged,
                has_tracked,
                has_untracked,
                has_stash,
                cx,
            )
        });

        cx.subscribe(
            &menu,
            |this, _, event: &GitOverflowMenuEvent, cx| match event {
                GitOverflowMenuEvent::Close => {
                    this.hide_git_overflow_menu(cx);
                }
                GitOverflowMenuEvent::StageAll { project_id } => {
                    this.hide_git_overflow_menu(cx);
                    cx.emit(OverlayManagerEvent::GitStageAll {
                        project_id: project_id.clone(),
                    });
                }
                GitOverflowMenuEvent::UnstageAll { project_id } => {
                    this.hide_git_overflow_menu(cx);
                    cx.emit(OverlayManagerEvent::GitUnstageAll {
                        project_id: project_id.clone(),
                    });
                }
                GitOverflowMenuEvent::StashAll { project_id } => {
                    this.hide_git_overflow_menu(cx);
                    cx.emit(OverlayManagerEvent::GitStashAll {
                        project_id: project_id.clone(),
                    });
                }
                GitOverflowMenuEvent::StashPop { project_id } => {
                    this.hide_git_overflow_menu(cx);
                    cx.emit(OverlayManagerEvent::GitStashPop {
                        project_id: project_id.clone(),
                    });
                }
                GitOverflowMenuEvent::ShowStash {
                    project_id,
                    position,
                } => {
                    this.hide_git_overflow_menu(cx);
                    this.request_broker.update(cx, |broker, cx| {
                        broker.push_overlay_request(
                            OverlayRequest::GitStashList {
                                project_id: project_id.clone(),
                                position: *position,
                            },
                            cx,
                        );
                    });
                }
                GitOverflowMenuEvent::DiscardAllTracked { project_id } => {
                    this.hide_git_overflow_menu(cx);
                    cx.emit(OverlayManagerEvent::GitDiscardAllTracked {
                        project_id: project_id.clone(),
                    });
                }
            },
        )
        .detach();

        self.git_overflow_menu.set(menu);
        cx.notify();
    }

    pub fn hide_git_overflow_menu(&mut self, cx: &mut Context<Self>) {
        self.git_overflow_menu.close();
        cx.notify();
    }

    pub fn render_git_overflow_menu(&self) -> Option<Entity<GitOverflowMenu>> {
        self.git_overflow_menu.render()
    }

    // ========================================================================
    // Git stash list overlay
    // ========================================================================

    pub fn has_git_stash_list(&self) -> bool {
        self.git_stash_list.is_open()
    }

    pub fn show_git_stash_list(
        &mut self,
        project_id: String,
        provider: std::sync::Arc<dyn notmux_views_git::diff_viewer::provider::GitProvider>,
        position: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.close_modal(cx);
        self.close_all_context_menus();

        let list = cx.new(|cx| GitStashList::new(project_id, provider, position, cx));

        cx.subscribe(
            &list,
            |this, _, event: &GitStashListEvent, cx| match event {
                GitStashListEvent::Close { project_id } => {
                    this.hide_git_stash_list(cx);
                    cx.emit(OverlayManagerEvent::GitStashRefresh {
                        project_id: project_id.clone(),
                    });
                }
            },
        )
        .detach();

        self.git_stash_list.set(list);
        cx.notify();
    }

    pub fn hide_git_stash_list(&mut self, cx: &mut Context<Self>) {
        self.git_stash_list.close();
        cx.notify();
    }

    pub fn render_git_stash_list(&self) -> Option<Entity<GitStashList>> {
        self.git_stash_list.render()
    }

    // ========================================================================
    // Tab context menu (positioned popup)
    // ========================================================================

    /// Show tab context menu.
    pub fn show_tab_context_menu(
        &mut self,
        tab_index: usize,
        num_tabs: usize,
        project_id: String,
        layout_path: Vec<usize>,
        position: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.close_modal(cx);
        self.close_all_context_menus();

        let menu = cx.new(|cx| {
            TabContextMenu::new(tab_index, num_tabs, project_id, layout_path, position, cx)
        });

        cx.subscribe(
            &menu,
            |this, _, event: &TabContextMenuEvent, cx| match event {
                TabContextMenuEvent::Close => {
                    this.hide_tab_context_menu(cx);
                }
                TabContextMenuEvent::CloseTab {
                    project_id,
                    layout_path,
                    tab_index,
                } => {
                    this.hide_tab_context_menu(cx);
                    cx.emit(OverlayManagerEvent::TabClose {
                        project_id: project_id.clone(),
                        layout_path: layout_path.clone(),
                        tab_index: *tab_index,
                    });
                }
                TabContextMenuEvent::CloseOtherTabs {
                    project_id,
                    layout_path,
                    tab_index,
                } => {
                    this.hide_tab_context_menu(cx);
                    cx.emit(OverlayManagerEvent::TabCloseOthers {
                        project_id: project_id.clone(),
                        layout_path: layout_path.clone(),
                        tab_index: *tab_index,
                    });
                }
                TabContextMenuEvent::CloseTabsToRight {
                    project_id,
                    layout_path,
                    tab_index,
                } => {
                    this.hide_tab_context_menu(cx);
                    cx.emit(OverlayManagerEvent::TabCloseToRight {
                        project_id: project_id.clone(),
                        layout_path: layout_path.clone(),
                        tab_index: *tab_index,
                    });
                }
            },
        )
        .detach();

        self.tab_context_menu.set(menu);
        cx.notify();
    }

    /// Hide tab context menu.
    pub fn hide_tab_context_menu(&mut self, cx: &mut Context<Self>) {
        self.tab_context_menu.close();
        cx.notify();
    }

    /// Get tab context menu entity for rendering.
    pub fn render_tab_context_menu(&self) -> Option<Entity<TabContextMenu>> {
        self.tab_context_menu.render()
    }

    // ========================================================================
    // Worktree list popover (positioned popup)
    // ========================================================================

    /// Check if worktree list popover is open.
    pub fn has_worktree_list(&self) -> bool {
        self.worktree_list.is_open()
    }

    /// Show worktree list popover.
    pub fn show_worktree_list(
        &mut self,
        project_id: String,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.close_all_context_menus();

        let workspace = self.workspace.clone();
        let hooks = crate::settings::settings(cx).hooks.clone();
        let popover =
            cx.new(|cx| WorktreeListPopover::new(workspace, project_id, position, hooks, cx));

        cx.subscribe(&popover, |this, _, event: &WorktreeListPopoverEvent, cx| {
            if event.is_close() {
                this.hide_worktree_list(cx);
            }
        })
        .detach();

        self.worktree_list.set(popover);
        cx.notify();
    }

    /// Hide worktree list popover.
    pub fn hide_worktree_list(&mut self, cx: &mut Context<Self>) {
        self.worktree_list.close();
        cx.notify();
    }

    /// Get worktree list popover entity for rendering.
    pub fn render_worktree_list(&self) -> Option<Entity<WorktreeListPopover>> {
        self.worktree_list.render()
    }

    // ========================================================================
    // Color picker popover (positioned popup)
    // ========================================================================

    /// Check if color picker popover is open.
    pub fn has_color_picker(&self) -> bool {
        self.color_picker.is_open()
    }

    /// Show color picker popover.
    pub fn show_color_picker(
        &mut self,
        target: ColorPickerTarget,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.close_all_context_menus();

        let workspace = self.workspace.clone();
        let popover = cx.new(|cx| ColorPickerPopover::new(workspace, target, position, cx));

        cx.subscribe(&popover, |this, _, event: &ColorPickerPopoverEvent, cx| {
            match event {
                ColorPickerPopoverEvent::Close => {
                    this.hide_color_picker(cx);
                }
                ColorPickerPopoverEvent::ProjectColorChanged { project_id, color } => {
                    // Emit for sidebar to handle remote sync
                    cx.emit(OverlayManagerEvent::ProjectColorChanged {
                        project_id: project_id.clone(),
                        color: *color,
                    });
                }
            }
        })
        .detach();

        self.color_picker.set(popover);
        cx.notify();
    }

    /// Hide color picker popover.
    pub fn hide_color_picker(&mut self, cx: &mut Context<Self>) {
        self.color_picker.close();
        cx.notify();
    }

    /// Get color picker popover entity for rendering.
    pub fn render_color_picker(&self) -> Option<Entity<ColorPickerPopover>> {
        self.color_picker.render()
    }

    // ========================================================================
    // File search (parametric)
    // ========================================================================

    /// Toggle file search dialog for a project.
    pub fn toggle_file_search(
        &mut self,
        fs: std::sync::Arc<dyn notmux_files::project_fs::ProjectFs>,
        cx: &mut Context<Self>,
    ) {
        if self.is_modal::<FileSearchDialog>() {
            self.close_modal(cx);
        } else {
            self.show_file_search(fs, cx);
        }
    }

    /// Show file search dialog for a project.
    pub fn show_file_search(
        &mut self,
        fs: std::sync::Arc<dyn notmux_files::project_fs::ProjectFs>,
        cx: &mut Context<Self>,
    ) {
        let fs_for_viewer = fs.clone();
        let dialog = cx.new(|cx| FileSearchDialog::new(fs, cx));

        cx.subscribe(
            &dialog,
            move |this, _, event: &FileSearchDialogEvent, cx| match event {
                FileSearchDialogEvent::Close => {
                    this.close_modal(cx);
                }
                FileSearchDialogEvent::FileSelected(path) => {
                    let relative_path = path.to_string_lossy().to_string();
                    this.close_modal(cx);
                    this.show_file_viewer(relative_path, fs_for_viewer.clone(), cx);
                }
            },
        )
        .detach();

        self.open_modal(dialog, cx);
        cx.notify();
    }

    // ========================================================================
    // Content search (Find in Files)
    // ========================================================================

    /// Toggle content search dialog for a project.
    pub fn toggle_content_search(
        &mut self,
        fs: std::sync::Arc<dyn notmux_files::project_fs::ProjectFs>,
        cx: &mut Context<Self>,
    ) {
        if self.is_modal::<ContentSearchDialog>() {
            self.close_modal(cx);
        } else {
            self.show_content_search(fs, cx);
        }
    }

    /// Show content search dialog for a project.
    pub fn show_content_search(
        &mut self,
        fs: std::sync::Arc<dyn notmux_files::project_fs::ProjectFs>,
        cx: &mut Context<Self>,
    ) {
        let fs_for_viewer = fs.clone();
        let dialog = cx.new(|cx| ContentSearchDialog::new(fs, cx));

        cx.subscribe(
            &dialog,
            move |this, _, event: &ContentSearchDialogEvent, cx| match event {
                ContentSearchDialogEvent::Close => {
                    this.close_modal(cx);
                }
                ContentSearchDialogEvent::FileSelected { path, line: _ } => {
                    let relative_path = path.to_string_lossy().to_string();
                    this.close_modal(cx);
                    this.show_file_viewer(relative_path, fs_for_viewer.clone(), cx);
                }
            },
        )
        .detach();

        self.open_modal(dialog, cx);
        cx.notify();
    }

    // ========================================================================
    // File browser / viewer (parametric)
    // ========================================================================

    /// Show file browser for a project (no pre-selected file).
    pub fn show_file_browser(
        &mut self,
        fs: std::sync::Arc<dyn notmux_files::project_fs::ProjectFs>,
        cx: &mut Context<Self>,
    ) {
        let settings = crate::settings::settings_entity(cx).read(cx).settings.clone();
        let font_size = settings.file_font_size;
        let monochrome_icons = settings.monochrome_icons;
        let theme_colors = crate::theme::theme(cx);
        let is_dark = theme_colors.is_dark();
        let cache_key = fs.project_id();

        // Reuse cached viewer if available
        if let Some(viewer) = self.cached_file_viewers.get(&cache_key) {
            viewer.update(cx, |v, cx| {
                v.update_config(font_size, is_dark, theme_colors, monochrome_icons, cx)
            });
            self.open_modal(viewer.clone(), cx);
            return;
        }

        let viewer = cx.new(|cx| {
            FileViewer::new_browse(
                fs,
                font_size,
                is_dark,
                theme_colors,
                monochrome_icons,
                cx,
            )
        });

        cx.subscribe(&viewer, move |this, _, event: &FileViewerEvent, cx| {
            match event {
                FileViewerEvent::Close => {
                    // Hide but keep cached
                    this.hide_modal(cx);
                }
            }
        })
        .detach();

        self.cached_file_viewers.insert(cache_key, viewer.clone());
        self.open_modal(viewer, cx);
        cx.notify();
    }

    /// Show file viewer for a file.
    pub fn show_file_viewer(
        &mut self,
        relative_path: String,
        fs: std::sync::Arc<dyn notmux_files::project_fs::ProjectFs>,
        cx: &mut Context<Self>,
    ) {
        let settings = crate::settings::settings_entity(cx).read(cx).settings.clone();
        let font_size = settings.file_font_size;
        let monochrome_icons = settings.monochrome_icons;
        let theme_colors = crate::theme::theme(cx);
        let is_dark = theme_colors.is_dark();
        let cache_key = fs.project_id();

        // Reuse cached viewer if available
        if let Some(viewer) = self.cached_file_viewers.get(&cache_key) {
            viewer.update(cx, |v, cx| {
                v.update_config(font_size, is_dark, theme_colors, monochrome_icons, cx);
                v.open_file_in_tab(PathBuf::from(&relative_path), cx);
            });
            self.open_modal(viewer.clone(), cx);
            return;
        }

        let viewer = cx.new(|cx| {
            FileViewer::new(
                PathBuf::from(&relative_path),
                fs,
                font_size,
                is_dark,
                theme_colors,
                monochrome_icons,
                cx,
            )
        });

        cx.subscribe(&viewer, |this, _, event: &FileViewerEvent, cx| {
            match event {
                FileViewerEvent::Close => {
                    // Hide but keep cached
                    this.hide_modal(cx);
                }
            }
        })
        .detach();

        self.cached_file_viewers.insert(cache_key, viewer.clone());
        self.open_modal(viewer, cx);
        cx.notify();
    }

    // ========================================================================
    // Diff viewer (parametric)
    // ========================================================================

    /// Show diff viewer for a project, optionally selecting a specific file, diff mode, commit message, and commit navigation list.
    #[allow(clippy::too_many_arguments)]
    pub fn show_diff_viewer(
        &mut self,
        provider: std::sync::Arc<dyn crate::views::overlays::diff_viewer::provider::GitProvider>,
        select_file: Option<String>,
        mode: Option<notmux_core::types::DiffMode>,
        commit_message: Option<String>,
        commits: Option<Vec<crate::git::CommitLogEntry>>,
        commit_index: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        let viewer = cx.new(|cx| {
            DiffViewer::new(
                provider,
                select_file,
                mode,
                commit_message,
                commits,
                commit_index,
                cx,
            )
        });

        cx.subscribe(&viewer, |this, _, event: &DiffViewerEvent, cx| {
            match event {
                DiffViewerEvent::Close => {
                    // Settings are now persisted through ExtensionSettingsStore
                    // when toggled — no manual sync needed on close.
                    this.close_modal(cx);
                }
            }
        })
        .detach();

        self.open_modal(viewer, cx);
        cx.notify();
    }

    // ========================================================================
    // Remote connect dialog (parametric)
    // ========================================================================

    /// Toggle remote connect dialog overlay.
    pub fn toggle_remote_connect(
        &mut self,
        remote_manager: Entity<RemoteConnectionManager>,
        cx: &mut Context<Self>,
    ) {
        if self.is_modal::<RemoteConnectDialog>() {
            self.close_modal(cx);
        } else {
            let entity = cx.new(|cx| RemoteConnectDialog::new(remote_manager, cx));
            cx.subscribe(
                &entity,
                |this, _, event: &RemoteConnectDialogEvent, cx| match event {
                    RemoteConnectDialogEvent::Close => {
                        this.close_modal(cx);
                    }
                    RemoteConnectDialogEvent::Connected { config } => {
                        cx.emit(OverlayManagerEvent::RemoteConnected {
                            config: config.clone(),
                        });
                        this.close_modal(cx);
                    }
                },
            )
            .detach();
            self.open_modal(entity, cx);
        }
        cx.notify();
    }

    // ========================================================================
    // Remote pair dialog (re-pair existing connection)
    // ========================================================================

    /// Show remote pair dialog for an existing connection.
    pub fn show_remote_pair_dialog(
        &mut self,
        connection_id: String,
        connection_name: String,
        cx: &mut Context<Self>,
    ) {
        let entity = cx.new(|cx| RemotePairDialog::new(connection_id, connection_name, cx));
        cx.subscribe(
            &entity,
            |this, _, event: &RemotePairDialogEvent, cx| match event {
                RemotePairDialogEvent::Close => {
                    this.close_modal(cx);
                }
                RemotePairDialogEvent::Pair {
                    connection_id,
                    code,
                } => {
                    cx.emit(OverlayManagerEvent::RemotePaired {
                        connection_id: connection_id.clone(),
                        code: code.clone(),
                    });
                    this.close_modal(cx);
                }
            },
        )
        .detach();
        self.open_modal(entity, cx);
        cx.notify();
    }

    // ========================================================================
    // Render helpers (context menus only - modal uses render_modal())
    // ========================================================================

    /// Get context menu entity for rendering.
    pub fn render_context_menu(&self) -> Option<Entity<ContextMenu>> {
        self.context_menu.render()
    }

    /// Get folder context menu entity for rendering.
    pub fn render_folder_context_menu(&self) -> Option<Entity<FolderContextMenu>> {
        self.folder_context_menu.render()
    }
}

impl EventEmitter<OverlayManagerEvent> for OverlayManager {}
