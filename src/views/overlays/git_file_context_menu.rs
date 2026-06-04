//! Context menu for files in the git panel (right-click on a file entry).

use crate::keybindings::Cancel;
use crate::theme::theme;
use gpui::prelude::*;
use gpui::*;
use notmux_ui::menu::{context_menu_panel, menu_item, menu_item_with_color, menu_separator};

/// Event emitted by GitFileContextMenu.
pub enum GitFileContextMenuEvent {
    Close,
    Stage {
        project_id: String,
        file_path: String,
    },
    Unstage {
        project_id: String,
        file_path: String,
    },
    Discard {
        project_id: String,
        file_path: String,
        is_untracked: bool,
    },
    OpenDiff {
        project_id: String,
        file_path: String,
    },
    OpenFile {
        project_id: String,
        #[allow(dead_code)]
        file_path: String,
    },
    AddToGitignore {
        project_id: String,
        file_path: String,
    },
    CopyPath {
        path: String,
    },
}

impl notmux_ui::overlay::CloseEvent for GitFileContextMenuEvent {
    fn is_close(&self) -> bool {
        matches!(self, Self::Close)
    }
}

/// Context menu for git file entries.
pub struct GitFileContextMenu {
    project_id: String,
    file_path: String,
    is_staged: bool,
    is_untracked: bool,
    is_conflict: bool,
    position: Point<Pixels>,
    focus_handle: FocusHandle,
}

impl GitFileContextMenu {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        project_id: String,
        file_path: String,
        is_staged: bool,
        is_untracked: bool,
        is_conflict: bool,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        Self {
            project_id,
            file_path,
            is_staged,
            is_untracked,
            is_conflict,
            position,
            focus_handle,
        }
    }

    fn close(&self, cx: &mut Context<Self>) {
        cx.emit(GitFileContextMenuEvent::Close);
    }
}

impl EventEmitter<GitFileContextMenuEvent> for GitFileContextMenu {}

impl Render for GitFileContextMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);

        if !self.focus_handle.is_focused(window) {
            window.focus(&self.focus_handle, cx);
        }

        let position = self.position;
        let project_id = self.project_id.clone();
        let file_path = self.file_path.clone();
        let is_staged = self.is_staged;
        let is_untracked = self.is_untracked;
        let _is_conflict = self.is_conflict;

        div()
            .track_focus(&self.focus_handle)
            .key_context("GitFileContextMenu")
            .on_action(cx.listener(|this, _: &Cancel, _window, cx| {
                this.close(cx);
            }))
            .absolute()
            .inset_0()
            .id("git-file-context-menu-backdrop")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _window, cx| {
                    this.close(cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, _window, cx| {
                    this.close(cx);
                }),
            )
            .child(deferred(
                anchored().position(position).snap_to_window().child(
                    context_menu_panel("git-file-context-menu", &t)
                        // Stage / Unstage
                        .child(if is_staged {
                            menu_item("gfcm-unstage", "icons/chevron-down.svg", "Unstage", &t)
                                .on_click(cx.listener({
                                    let pid = project_id.clone();
                                    let fp = file_path.clone();
                                    move |_this, _, _window, cx| {
                                        cx.emit(GitFileContextMenuEvent::Unstage {
                                            project_id: pid.clone(),
                                            file_path: fp.clone(),
                                        });
                                    }
                                }))
                        } else {
                            menu_item("gfcm-stage", "icons/chevron-up.svg", "Stage", &t).on_click(
                                cx.listener({
                                    let pid = project_id.clone();
                                    let fp = file_path.clone();
                                    move |_this, _, _window, cx| {
                                        cx.emit(GitFileContextMenuEvent::Stage {
                                            project_id: pid.clone(),
                                            file_path: fp.clone(),
                                        });
                                    }
                                }),
                            )
                        })
                        // Discard / Trash (destructive)
                        .child(
                            menu_item_with_color(
                                "gfcm-discard",
                                "icons/trash.svg",
                                if is_untracked {
                                    "Trash File"
                                } else {
                                    "Discard Changes"
                                },
                                t.error,
                                t.error,
                                &t,
                            )
                            .on_click(cx.listener({
                                let pid = project_id.clone();
                                let fp = file_path.clone();
                                move |_this, _, _window, cx| {
                                    cx.emit(GitFileContextMenuEvent::Discard {
                                        project_id: pid.clone(),
                                        file_path: fp.clone(),
                                        is_untracked,
                                    });
                                }
                            })),
                        )
                        // Add to .gitignore (only for untracked)
                        .when(is_untracked, |d| {
                            d.child(
                                menu_item(
                                    "gfcm-gitignore",
                                    "icons/file.svg",
                                    "Add to .gitignore",
                                    &t,
                                )
                                .on_click(cx.listener({
                                    let pid = project_id.clone();
                                    let fp = file_path.clone();
                                    move |_this, _, _window, cx| {
                                        cx.emit(GitFileContextMenuEvent::AddToGitignore {
                                            project_id: pid.clone(),
                                            file_path: fp.clone(),
                                        });
                                    }
                                })),
                            )
                        })
                        .child(menu_separator(&t))
                        // Open Diff (not meaningful for untracked)
                        .when(!is_untracked, |d| {
                            d.child(
                                menu_item(
                                    "gfcm-open-diff",
                                    "icons/git-commit.svg",
                                    "Open Diff",
                                    &t,
                                )
                                .on_click(cx.listener({
                                    let pid = project_id.clone();
                                    let fp = file_path.clone();
                                    move |_this, _, _window, cx| {
                                        cx.emit(GitFileContextMenuEvent::OpenDiff {
                                            project_id: pid.clone(),
                                            file_path: fp.clone(),
                                        });
                                    }
                                })),
                            )
                        })
                        // Open File
                        .child(
                            menu_item("gfcm-open-file", "icons/file.svg", "Open File", &t)
                                .on_click(cx.listener({
                                    let pid = project_id.clone();
                                    let fp = file_path.clone();
                                    move |_this, _, _window, cx| {
                                        cx.emit(GitFileContextMenuEvent::OpenFile {
                                            project_id: pid.clone(),
                                            file_path: fp.clone(),
                                        });
                                    }
                                })),
                        )
                        .child(menu_separator(&t))
                        // Copy Path
                        .child(
                            menu_item("gfcm-copy-path", "icons/copy.svg", "Copy Path", &t)
                                .on_click(cx.listener({
                                    let fp = file_path.clone();
                                    move |_this, _, _window, cx| {
                                        cx.emit(GitFileContextMenuEvent::CopyPath {
                                            path: fp.clone(),
                                        });
                                    }
                                })),
                        ),
                ),
            ))
    }
}

impl gpui::Focusable for GitFileContextMenu {
    fn focus_handle(&self, _cx: &gpui::App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}
