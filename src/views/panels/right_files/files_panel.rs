//! Embeds the notmux file explorer + file viewer in the right side panel.
//!
//! Left: the vendored `FileExplorer` tree (lazy directory listing with git
//! status decoration, inline rename/new-file/new-folder, right-click context
//! menu). Right: the vendored `FileViewer` editor (tabs, syntect syntax
//! highlighting, markdown source/preview toggle, in-file search, editing).
//!
//! This mirrors `git_panel.rs`: a thin host that owns the vendored views,
//! bridges the app's `GpuiTheme` into the `ThemeColors` they expect, and
//! plays the role of notmux's root view for the explorer's overlay requests
//! (open file → viewer tab, right-click → context menu, menu events → fs ops).

use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::*;

use notmux_files::clipboard::{ClipboardOp, ExplorerClipboard};
use notmux_files::file_viewer::{FileViewer, FileViewerEvent};
use notmux_files::fs_ops;
use notmux_files::project_fs::{LocalProjectFs, ProjectFs};
use notmux_theme::ThemeColors;
use notmux_workspace::request_broker::RequestBroker;
use notmux_workspace::requests::OverlayRequest;

use super::explorer_context_menu::{ExplorerContextMenu, ExplorerContextMenuEvent};
use super::file_explorer::FileExplorer;
use super::resize_handle::ResizeHandle;
use crate::theme::git_theme;

/// Editor font size — matches the chat's body text size (`markdown::TEXT_SIZE`).
const FILE_FONT_SIZE: f32 = 14.0;

/// Default width of the explorer tree column.
const EXPLORER_WIDTH: f32 = 240.0;
/// Bounds the explorer column can be resized within.
const EXPLORER_MIN_WIDTH: f32 = 140.0;
const EXPLORER_MAX_WIDTH: f32 = 520.0;

const PROJECT_ID: &str = "files-panel";

/// In-progress splitter drag between the tree and the editor: mouse x and
/// explorer width at drag start.
#[derive(Clone, Copy)]
struct SplitterDrag {
    start_x: f32,
    start_width: f32,
}

/// Hosts the vendored file explorer tree and file viewer editor over the cwd.
pub struct FilesPanel {
    explorer: Entity<FileExplorer>,
    viewer: Option<Entity<FileViewer>>,
    _broker: Entity<RequestBroker>,
    context_menu: Option<Entity<ExplorerContextMenu>>,
    project_path: PathBuf,
    /// Theme snapshot the viewer was last configured with, so a theme switch
    /// re-highlights open tabs (mirrors notmux's `update_config` on settings
    /// change).
    viewer_theme: ThemeColors,
    /// Current width of the explorer tree column (resizable via the splitter).
    explorer_width: f32,
    splitter_drag: Option<SplitterDrag>,
}

impl FilesPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let project_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        Self::with_project_path(project_path, cx)
    }

    pub(crate) fn with_project_path(project_path: PathBuf, cx: &mut Context<Self>) -> Self {
        // The vendored views read theme colors through the global provider
        // (panics if unset). The git/terminal panels install the same bridge,
        // but the files panel can open first — install ours if nothing has.
        if cx
            .try_global::<notmux_theme::GlobalThemeProvider>()
            .is_none()
        {
            cx.set_global(notmux_theme::GlobalThemeProvider(git_theme));
        }
        // Process-wide cut/copy/paste clipboard for the explorer context menu.
        if cx.try_global::<ExplorerClipboard>().is_none() {
            cx.set_global(ExplorerClipboard::default());
        }

        let broker = cx.new(|_| RequestBroker::new());
        let explorer = {
            let rb = broker.clone();
            let path = project_path.clone();
            cx.new(move |cx| {
                FileExplorer::new(
                    PROJECT_ID.to_string(),
                    path,
                    notmux_workspace::requests::ExplorerHost::FilesTab,
                    rb,
                    cx,
                )
            })
        };
        cx.observe(&explorer, |_, _, cx| cx.notify()).detach();

        // The explorer talks to its host through the request broker (open a
        // file, open a context menu) — drain and handle requests as they come.
        cx.observe(&broker, |this: &mut Self, broker, cx| {
            let requests = broker.update(cx, |b, _| b.drain_overlay_requests());
            for request in requests {
                this.handle_overlay_request(request, cx);
            }
        })
        .detach();

        Self {
            explorer,
            viewer: None,
            _broker: broker,
            context_menu: None,
            project_path,
            viewer_theme: git_theme(cx),
            explorer_width: EXPLORER_WIDTH,
            splitter_drag: None,
        }
    }

    fn project_fs(&self) -> Arc<dyn ProjectFs> {
        Arc::new(LocalProjectFs::new(self.project_path.clone()))
    }

    /// Whether keyboard focus is inside this panel (the editor or an inline
    /// explorer input). The app checks this before pulling focus back into
    /// the chat composer — otherwise typing in the editor would be impossible.
    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        if let Some(viewer) = &self.viewer
            && viewer
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx)
        {
            return true;
        }
        self.explorer.read(cx).input_focused(window, cx)
    }

    fn handle_overlay_request(&mut self, request: OverlayRequest, cx: &mut Context<Self>) {
        match request {
            OverlayRequest::MainFileViewer { file, .. } => {
                self.open_file(PathBuf::from(file), cx);
            }
            OverlayRequest::ExplorerContextMenu {
                kind,
                path,
                parent_dir,
                has_clipboard,
                position,
                // Single-host panel: the origin is always our own explorer.
                host: _,
            } => {
                self.show_context_menu(kind, path, parent_dir, has_clipboard, position, cx);
            }
            _ => {}
        }
    }

    /// Open a project-relative file path in the editor, creating the viewer on
    /// first use (like notmux's `show_main_file_viewer`) and adding a tab
    /// afterwards.
    fn open_file(&mut self, file: PathBuf, cx: &mut Context<Self>) {
        if let Some(viewer) = &self.viewer {
            viewer.update(cx, |v, cx| v.open_file_in_tab(file, cx));
            return;
        }

        let theme_colors = git_theme(cx);
        let is_dark = theme_colors.is_dark();
        self.viewer_theme = theme_colors;
        let fs = self.project_fs();
        let viewer = cx.new(|cx| {
            FileViewer::new_embedded(file, fs, FILE_FONT_SIZE, is_dark, theme_colors, false, cx)
        });
        cx.subscribe(&viewer, |this, _, event: &FileViewerEvent, cx| {
            if matches!(event, FileViewerEvent::Close) {
                this.viewer = None;
                cx.notify();
            }
        })
        .detach();
        cx.observe(&viewer, |_, _, cx| cx.notify()).detach();
        self.viewer = Some(viewer);
        cx.notify();
    }

    // ===== explorer context menu (plays notmux's overlay-manager role) =====

    fn show_context_menu(
        &mut self,
        kind: notmux_workspace::requests::ExplorerKind,
        path: PathBuf,
        parent_dir: PathBuf,
        has_clipboard: bool,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let menu = cx.new(|cx| {
            ExplorerContextMenu::new(kind, path, parent_dir, has_clipboard, position, cx)
        });
        cx.subscribe(&menu, |this, _, event: &ExplorerContextMenuEvent, cx| {
            this.handle_context_menu_event(event, cx);
        })
        .detach();
        self.context_menu = Some(menu);
        cx.notify();
    }

    fn hide_context_menu(&mut self, cx: &mut Context<Self>) {
        self.context_menu = None;
        self.explorer
            .update(cx, |fe, cx| fe.clear_context_menu_target(cx));
        cx.notify();
    }

    /// Handle a context-menu event — the same dispatch notmux's root view does
    /// (`OverlayManagerEvent::Explorer*`), against our single explorer.
    fn handle_context_menu_event(
        &mut self,
        event: &ExplorerContextMenuEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            ExplorerContextMenuEvent::Close => {
                self.hide_context_menu(cx);
            }
            ExplorerContextMenuEvent::NewFile { parent } => {
                let parent = parent.clone();
                self.hide_context_menu(cx);
                self.explorer
                    .update(cx, |fe, cx| fe.start_new_file(parent, cx));
            }
            ExplorerContextMenuEvent::NewFolder { parent } => {
                let parent = parent.clone();
                self.hide_context_menu(cx);
                self.explorer
                    .update(cx, |fe, cx| fe.start_new_folder(parent, cx));
            }
            ExplorerContextMenuEvent::Rename { target } => {
                let target = target.clone();
                self.hide_context_menu(cx);
                self.explorer
                    .update(cx, |fe, cx| fe.start_rename(target, cx));
            }
            ExplorerContextMenuEvent::Delete { path, is_dir } => {
                let path = path.clone();
                let is_dir = *is_dir;
                self.hide_context_menu(cx);
                let explorer = self.explorer.clone();
                cx.spawn(async move |_this, cx| {
                    let p = path.clone();
                    let result = smol::unblock(move || fs_ops::delete(&p, is_dir)).await;
                    cx.update(|cx| {
                        if let Err(msg) = result {
                            log::warn!("explorer delete failed: {msg}");
                        }
                        explorer
                            .update(cx, |fe, cx| fe.patch_paths(std::slice::from_ref(&path), cx));
                    });
                })
                .detach();
            }
            ExplorerContextMenuEvent::CopyPath { path } => {
                cx.write_to_clipboard(ClipboardItem::new_string(path.to_string_lossy().into()));
                self.hide_context_menu(cx);
            }
            ExplorerContextMenuEvent::RevealInFinder { path } => {
                let path = path.clone();
                self.hide_context_menu(cx);
                cx.spawn(async move |_this, _cx| {
                    let _ = smol::unblock(move || fs_ops::reveal_in_file_manager(&path)).await;
                })
                .detach();
            }
            ExplorerContextMenuEvent::Cut { path } => {
                let path = path.clone();
                cx.global_mut::<ExplorerClipboard>()
                    .set(path, ClipboardOp::Cut);
                self.hide_context_menu(cx);
            }
            ExplorerContextMenuEvent::Copy { path } => {
                let path = path.clone();
                cx.global_mut::<ExplorerClipboard>()
                    .set(path, ClipboardOp::Copy);
                self.hide_context_menu(cx);
            }
            ExplorerContextMenuEvent::Paste { target_dir } => {
                let target_dir = target_dir.clone();
                self.hide_context_menu(cx);
                let cb = cx.global::<ExplorerClipboard>().clone();
                let (Some(src), Some(op)) = (cb.path.clone(), cb.op) else {
                    return;
                };
                let file_name = match src.file_name() {
                    Some(n) => n.to_os_string(),
                    None => return,
                };
                let dst = target_dir.join(&file_name);
                let explorer = self.explorer.clone();
                let src_for_patch = src.clone();
                let dst_for_patch = dst.clone();
                cx.spawn(async move |_this, cx| {
                    let src_ = src.clone();
                    let dst_ = dst.clone();
                    let result = smol::unblock(move || match op {
                        ClipboardOp::Cut => fs_ops::move_to(&src_, &dst_),
                        ClipboardOp::Copy => fs_ops::copy(&src_, &dst_),
                    })
                    .await;
                    cx.update(|cx| {
                        if let Err(msg) = result {
                            log::warn!("explorer paste failed: {msg}");
                        }
                        // Clear clipboard on Cut; keep on Copy.
                        if matches!(op, ClipboardOp::Cut) {
                            cx.global_mut::<ExplorerClipboard>().clear();
                        }
                        explorer.update(cx, |fe, cx| {
                            fe.patch_paths(&[src_for_patch.clone(), dst_for_patch.clone()], cx);
                        });
                    });
                })
                .detach();
            }
            ExplorerContextMenuEvent::AddToGitignore { path } => {
                let path = path.clone();
                self.hide_context_menu(cx);
                let Ok(rel) = path.strip_prefix(&self.project_path) else {
                    return;
                };
                let rel = rel.to_string_lossy().replace('\\', "/");
                let gitignore = self.project_path.join(".gitignore");
                let explorer = self.explorer.clone();
                cx.spawn(async move |_this, cx| {
                    let result = smol::unblock(move || append_to_gitignore(&gitignore, &rel)).await;
                    cx.update(|cx| {
                        if let Err(msg) = result {
                            log::warn!("explorer add-to-gitignore failed: {msg}");
                        }
                        explorer.update(cx, |fe, cx| fe.refresh(cx));
                    });
                })
                .detach();
            }
        }
    }

    /// Push the current app theme into the viewer when it changed (theme
    /// switch re-highlights open tabs, like notmux's settings propagation).
    fn sync_viewer_theme(&mut self, cx: &mut Context<Self>) {
        let theme_colors = git_theme(cx);
        if theme_colors == self.viewer_theme {
            return;
        }
        self.viewer_theme = theme_colors;
        if let Some(viewer) = &self.viewer {
            let is_dark = theme_colors.is_dark();
            viewer.update(cx, |v, cx| {
                v.update_config(FILE_FONT_SIZE, is_dark, theme_colors, false, cx);
            });
        }
    }
}

/// Append `rel` as a line to the project `.gitignore` (creating the file if
/// needed), unless an identical line is already present.
fn append_to_gitignore(gitignore: &std::path::Path, rel: &str) -> Result<(), String> {
    let existing = std::fs::read_to_string(gitignore).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == rel) {
        return Ok(());
    }
    let mut content = existing;
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(rel);
    content.push('\n');
    std::fs::write(gitignore, content).map_err(|e| e.to_string())
}

impl Render for FilesPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_viewer_theme(cx);
        let t = git_theme(cx);

        let editor: AnyElement = if let Some(viewer) = &self.viewer {
            div()
                .flex_1()
                .min_w(px(0.))
                .h_full()
                .child(viewer.clone())
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_w(px(0.))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgb(t.bg_secondary))
                .text_size(px(13.))
                .text_color(rgb(t.text_muted))
                .child(SharedString::from("Select a file to open it"))
                .into_any_element()
        };

        // Draggable splitter between tree and editor: mouse-down on the handle
        // starts the drag; the panel-level move/up listeners apply and end it
        // (same pattern as the app's panel splitters).
        let tc = git_theme(cx);
        let outline = gpui::rgb(tc.border).into();
        let accent = gpui::rgb(tc.border_active).into();
        let entity = cx.entity();
        let start_width = self.explorer_width;
        let splitter = ResizeHandle::new(outline, accent, move |pos: Point<Pixels>, cx| {
            entity.update(cx, |this, _| {
                this.splitter_drag = Some(SplitterDrag { start_x: pos.x.into(), start_width });
            });
        });

        div()
            .relative()
            .size_full()
            .flex()
            .flex_row()
            .min_h(px(0.))
            .child(
                div()
                    .w(px(self.explorer_width))
                    .flex_shrink_0()
                    .h_full()
                    .child(self.explorer.clone()),
            )
            .child(splitter)
            .child(editor)
            // While dragging, an occluding overlay above the whole panel takes
            // the mouse moves — the editor beneath occludes its own bounds, so
            // without this the drag would freeze as soon as the pointer
            // crosses into it (same pattern as the app's panel splitters).
            .when(self.splitter_drag.is_some(), |d| {
                d.child(
                    deferred(
                        div()
                            .id("files-splitter-drag-overlay")
                            .absolute()
                            .inset_0()
                            .occlude()
                            .cursor(CursorStyle::ResizeLeftRight)
                            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _window, cx| {
                                if let Some(drag) = this.splitter_drag {
                                    let x: f32 = ev.position.x.into();
                                    let width = (drag.start_width + (x - drag.start_x))
                                        .clamp(EXPLORER_MIN_WIDTH, EXPLORER_MAX_WIDTH);
                                    if width != this.explorer_width {
                                        this.explorer_width = width;
                                        cx.notify();
                                    }
                                }
                            }))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|this, _, _window, cx| {
                                    if this.splitter_drag.take().is_some() {
                                        cx.notify();
                                    }
                                }),
                            )
                            .on_mouse_up_out(
                                MouseButton::Left,
                                cx.listener(|this, _, _window, cx| {
                                    if this.splitter_drag.take().is_some() {
                                        cx.notify();
                                    }
                                }),
                            ),
                    )
                    .with_priority(2),
                )
            })
            .when_some(self.context_menu.clone(), |d, menu| d.child(menu))
    }
}

