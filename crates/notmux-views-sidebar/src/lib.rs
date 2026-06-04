pub mod color_picker;
pub mod context_menu;
pub mod drag;
pub mod file_explorer;
pub mod folder_context_menu;
pub mod folder_list;
pub mod hook_list;
pub mod hook_log;
pub mod item_widgets;
pub mod project_list;
pub mod remote_list;
pub mod rename_directory_dialog;
pub mod search_panel;
pub mod service_list;
pub mod sidebar;
pub mod worktree_list;

pub use file_explorer::FileExplorer;

pub use sidebar::Sidebar;

// Re-export settings types
pub use sidebar::{BuildProjectFsFn, DispatchActionFn, GetSettingsFn, SidebarSettings};

// Re-export remote manager callback types
pub use sidebar::{
    GetRemoteConnectionsFn, GetRemoteFolderFn, RemoteConnectionSnapshot, SendRemoteActionFn,
};

// Re-export context menu types
pub use context_menu::{ContextMenu, ContextMenuEvent};
pub use folder_context_menu::{FolderContextMenu, FolderContextMenuEvent};
pub use hook_log::{HookLog, HookLogEvent};
pub use rename_directory_dialog::{RenameDirectoryDialog, RenameDirectoryDialogEvent};

// Re-export popover types
pub use color_picker::{ColorPickerPopover, ColorPickerPopoverEvent, ColorPickerTarget};
pub use worktree_list::{WorktreeListPopover, WorktreeListPopoverEvent};

gpui::actions!(
    notmux_views_sidebar,
    [
        SidebarUp,
        SidebarDown,
        SidebarConfirm,
        SidebarToggleExpand,
        SidebarEscape,
        Cancel,
        ExplorerInputConfirm,
        ExplorerInputCancel,
    ]
);
