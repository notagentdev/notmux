//! Context menu for the sidebar file explorer (right-click on files,
//! folders, or the empty area below the tree).
//!
//! Entries match the notmux reference project's file explorer (minus Open,
//! which is not supported in notmux yet).

use crate::keybindings::Cancel;
use crate::theme::theme as git_theme;
use gpui::prelude::*;
use gpui::*;
use notmux_ui::menu::{context_menu_panel, menu_item, menu_item_with_color, menu_separator};
use notmux_workspace::requests::ExplorerKind;
use std::path::PathBuf;

/// Event emitted by ExplorerContextMenu.
pub enum ExplorerContextMenuEvent {
    Close,
    NewFile { parent: PathBuf },
    NewFolder { parent: PathBuf },
    Rename { target: PathBuf },
    Delete { path: PathBuf, is_dir: bool },
    CopyPath { path: PathBuf },
    RevealInFinder { path: PathBuf },
    Cut { path: PathBuf },
    Copy { path: PathBuf },
    Paste { target_dir: PathBuf },
    AddToGitignore { path: PathBuf },
}

impl notmux_ui::overlay::CloseEvent for ExplorerContextMenuEvent {
    fn is_close(&self) -> bool {
        matches!(self, Self::Close)
    }
}

pub struct ExplorerContextMenu {
    kind: ExplorerKind,
    path: PathBuf,
    parent_dir: PathBuf,
    has_clipboard: bool,
    position: Point<Pixels>,
    focus_handle: FocusHandle,
}

impl ExplorerContextMenu {
    pub fn new(
        kind: ExplorerKind,
        path: PathBuf,
        parent_dir: PathBuf,
        has_clipboard: bool,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            kind,
            path,
            parent_dir,
            has_clipboard,
            position,
            focus_handle: cx.focus_handle(),
        }
    }

    fn close(&self, cx: &mut Context<Self>) {
        cx.emit(ExplorerContextMenuEvent::Close);
    }
}

impl EventEmitter<ExplorerContextMenuEvent> for ExplorerContextMenu {}

impl Focusable for ExplorerContextMenu {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ExplorerContextMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = git_theme(cx);

        if !self.focus_handle.is_focused(window) {
            window.focus(&self.focus_handle, cx);
        }

        let kind = self.kind;
        let path = self.path.clone();
        let parent = self.parent_dir.clone();
        let is_dir = kind == ExplorerKind::Folder;
        let has_clipboard = self.has_clipboard;
        let position = self.position;

        let mut panel = context_menu_panel("explorer-context-menu", &t);

        // New File / New Folder — Folder + Empty
        if matches!(kind, ExplorerKind::Folder | ExplorerKind::Empty) {
            panel = panel
                .child(
                    menu_item("ecm-new-file", "icons/file.svg", "New File", &t).on_click(
                        cx.listener({
                            let parent = parent.clone();
                            move |_this, _, _window, cx| {
                                cx.emit(ExplorerContextMenuEvent::NewFile {
                                    parent: parent.clone(),
                                });
                            }
                        }),
                    ),
                )
                .child(
                    menu_item("ecm-new-folder", "icons/folder.svg", "New Folder", &t).on_click(
                        cx.listener({
                            let parent = parent.clone();
                            move |_this, _, _window, cx| {
                                cx.emit(ExplorerContextMenuEvent::NewFolder {
                                    parent: parent.clone(),
                                });
                            }
                        }),
                    ),
                );
        }

        // Cut / Copy — File + Folder only
        if matches!(kind, ExplorerKind::File | ExplorerKind::Folder) {
            if matches!(kind, ExplorerKind::Folder) {
                panel = panel.child(menu_separator(&t));
            }
            panel = panel
                .child(
                    menu_item("ecm-cut", "icons/copy.svg", "Cut", &t).on_click(cx.listener({
                        let p = path.clone();
                        move |_this, _, _window, cx| {
                            cx.emit(ExplorerContextMenuEvent::Cut { path: p.clone() });
                        }
                    })),
                )
                .child(
                    menu_item("ecm-copy", "icons/copy.svg", "Copy", &t).on_click(cx.listener({
                        let p = path.clone();
                        move |_this, _, _window, cx| {
                            cx.emit(ExplorerContextMenuEvent::Copy { path: p.clone() });
                        }
                    })),
                );
        }

        // Paste — all kinds, only if clipboard has content
        if has_clipboard {
            if matches!(kind, ExplorerKind::Empty) {
                panel = panel.child(menu_separator(&t));
            }
            panel = panel.child(
                menu_item("ecm-paste", "icons/copy.svg", "Paste", &t).on_click(cx.listener({
                    let parent = parent.clone();
                    move |_this, _, _window, cx| {
                        cx.emit(ExplorerContextMenuEvent::Paste { target_dir: parent.clone() });
                    }
                })),
            );
        }

        // Separator before reveal/rename/delete block
        if matches!(kind, ExplorerKind::File | ExplorerKind::Folder) {
            panel = panel.child(menu_separator(&t));
        }

        // Reveal in Finder — all kinds
        if matches!(kind, ExplorerKind::Empty) && !has_clipboard {
            panel = panel.child(menu_separator(&t));
        }
        panel = panel.child(
            menu_item("ecm-reveal", "icons/folder.svg", reveal_label(), &t).on_click(cx.listener(
                {
                    let p = path.clone();
                    move |_this, _, _window, cx| {
                        cx.emit(ExplorerContextMenuEvent::RevealInFinder { path: p.clone() });
                    }
                },
            )),
        );

        // Rename / Delete / Copy Path — File + Folder
        if matches!(kind, ExplorerKind::File | ExplorerKind::Folder) {
            panel = panel
                .child(
                    menu_item("ecm-rename", "icons/edit.svg", "Rename", &t).on_click(cx.listener(
                        {
                            let p = path.clone();
                            move |_this, _, _window, cx| {
                                cx.emit(ExplorerContextMenuEvent::Rename { target: p.clone() });
                            }
                        },
                    )),
                )
                .child(
                    menu_item_with_color(
                        "ecm-delete",
                        "icons/trash.svg",
                        "Delete",
                        t.error,
                        t.error,
                        &t,
                    )
                    .on_click(cx.listener({
                        let p = path.clone();
                        move |_this, _, _window, cx| {
                            cx.emit(ExplorerContextMenuEvent::Delete { path: p.clone(), is_dir });
                        }
                    })),
                )
                .child(
                    menu_item("ecm-copy-path", "icons/copy.svg", "Copy Path", &t).on_click(
                        cx.listener({
                            let p = path.clone();
                            move |_this, _, _window, cx| {
                                cx.emit(ExplorerContextMenuEvent::CopyPath { path: p.clone() });
                            }
                        }),
                    ),
                );

            // Files only: Add to .gitignore
            if matches!(kind, ExplorerKind::File) {
                panel = panel.child(
                    menu_item("ecm-gitignore", "icons/file.svg", "Add to .gitignore", &t).on_click(
                        cx.listener({
                            let p = path.clone();
                            move |_this, _, _window, cx| {
                                cx.emit(ExplorerContextMenuEvent::AddToGitignore {
                                    path: p.clone(),
                                });
                            }
                        }),
                    ),
                );
            }
        }

        div()
            .track_focus(&self.focus_handle)
            .key_context("ExplorerContextMenu")
            .on_action(cx.listener(|this, _: &Cancel, _window, cx| {
                this.close(cx);
            }))
            .absolute()
            .inset_0()
            .occlude()
            .id("explorer-context-menu-backdrop")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _window, cx| this.close(cx)),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, _window, cx| this.close(cx)),
            )
            .child(deferred(
                anchored().position(position).snap_to_window().child(panel),
            ))
    }
}

fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Reveal in Finder"
    } else if cfg!(target_os = "windows") {
        "Reveal in Explorer"
    } else {
        "Open in File Manager"
    }
}
