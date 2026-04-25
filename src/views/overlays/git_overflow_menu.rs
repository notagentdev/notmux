//! Three-dots overflow menu in the git panel header.
//!
//! Exposes batch operations that don't fit in the main commit/changes/history
//! tab strip: stage/unstage all, stash all/pop, show stash, and discard all
//! tracked. The destructive "Discard All Tracked" entry uses an inline
//! two-step confirmation rather than a separate dialog overlay.

use crate::keybindings::Cancel;
use crate::theme::theme;
use gpui::prelude::*;
use gpui::*;
use vryn_ui::menu::{
    context_menu_panel, menu_item, menu_item_conditional, menu_item_with_color, menu_separator,
};

pub enum GitOverflowMenuEvent {
    Close,
    StageAll { project_id: String },
    UnstageAll { project_id: String },
    StashAll { project_id: String },
    StashPop { project_id: String },
    ShowStash { project_id: String, position: Point<Pixels> },
    DiscardAllTracked { project_id: String },
}

impl vryn_ui::overlay::CloseEvent for GitOverflowMenuEvent {
    fn is_close(&self) -> bool {
        matches!(self, Self::Close)
    }
}

pub struct GitOverflowMenu {
    project_id: String,
    position: Point<Pixels>,
    has_staged: bool,
    has_unstaged: bool,
    has_tracked: bool,
    has_untracked: bool,
    has_stash: bool,
    /// When true, replace the destructive item with a Confirm/Cancel pair.
    confirming_discard: bool,
    focus_handle: FocusHandle,
}

impl GitOverflowMenu {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        project_id: String,
        position: Point<Pixels>,
        has_staged: bool,
        has_unstaged: bool,
        has_tracked: bool,
        has_untracked: bool,
        has_stash: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        Self {
            project_id,
            position,
            has_staged,
            has_unstaged,
            has_tracked,
            has_untracked,
            has_stash,
            confirming_discard: false,
            focus_handle,
        }
    }

    fn close(&self, cx: &mut Context<Self>) {
        cx.emit(GitOverflowMenuEvent::Close);
    }
}

impl EventEmitter<GitOverflowMenuEvent> for GitOverflowMenu {}

impl Render for GitOverflowMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);

        if !self.focus_handle.is_focused(window) {
            window.focus(&self.focus_handle, cx);
        }

        let position = self.position;
        let project_id = self.project_id.clone();
        let has_staged = self.has_staged;
        let has_unstaged = self.has_unstaged;
        let has_tracked = self.has_tracked;
        let has_untracked = self.has_untracked;
        let has_stash = self.has_stash;
        let confirming = self.confirming_discard;
        let can_stash_all = has_tracked || has_untracked;

        div()
            .track_focus(&self.focus_handle)
            .key_context("GitOverflowMenu")
            .on_action(cx.listener(|this, _: &Cancel, _window, cx| {
                this.close(cx);
            }))
            .absolute()
            .inset_0()
            .id("git-overflow-menu-backdrop")
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _window, cx| {
                this.close(cx);
            }))
            .on_mouse_down(MouseButton::Right, cx.listener(|this, _, _window, cx| {
                this.close(cx);
            }))
            .child(deferred(
                anchored()
                    .position(position)
                    .snap_to_window()
                    .child({
                        let panel = context_menu_panel("git-overflow-menu", &t);
                        if confirming {
                            // Confirmation step replaces the menu body with two
                            // explicit buttons. Keeping it inline (rather than
                            // popping a second overlay) avoids a focus jump and
                            // keeps the click-target right where the user already
                            // looked.
                            panel
                                .child(
                                    div()
                                        .px(px(12.0))
                                        .py(px(8.0))
                                        .text_color(rgb(t.text_secondary))
                                        .text_size(px(12.0))
                                        .child("Discard all changes to tracked files? Untracked files are kept."),
                                )
                                .child(menu_separator(&t))
                                .child(
                                    menu_item_with_color(
                                        "gom-confirm-discard",
                                        "icons/trash.svg",
                                        "Confirm Discard All Tracked",
                                        t.error,
                                        t.error,
                                        &t,
                                    )
                                    .on_click(cx.listener({
                                        let pid = project_id.clone();
                                        move |_this, _, _window, cx| {
                                            cx.emit(GitOverflowMenuEvent::DiscardAllTracked {
                                                project_id: pid.clone(),
                                            });
                                        }
                                    })),
                                )
                                .child(
                                    menu_item("gom-cancel-discard", "icons/close.svg", "Cancel", &t)
                                        .on_click(cx.listener(|this, _, _window, cx| {
                                            this.confirming_discard = false;
                                            cx.notify();
                                        })),
                                )
                        } else {
                            panel
                                .child(stage_item(
                                    "gom-stage-all",
                                    "icons/chevron-up.svg",
                                    "Stage All",
                                    has_unstaged,
                                    &t,
                                    cx,
                                    {
                                        let pid = project_id.clone();
                                        move |cx| {
                                            cx.emit(GitOverflowMenuEvent::StageAll {
                                                project_id: pid.clone(),
                                            });
                                        }
                                    },
                                ))
                                .child(stage_item(
                                    "gom-unstage-all",
                                    "icons/chevron-down.svg",
                                    "Unstage All",
                                    has_staged,
                                    &t,
                                    cx,
                                    {
                                        let pid = project_id.clone();
                                        move |cx| {
                                            cx.emit(GitOverflowMenuEvent::UnstageAll {
                                                project_id: pid.clone(),
                                            });
                                        }
                                    },
                                ))
                                .child(menu_separator(&t))
                                .child(stage_item(
                                    "gom-stash-all",
                                    "icons/bookmark.svg",
                                    "Stash All",
                                    can_stash_all,
                                    &t,
                                    cx,
                                    {
                                        let pid = project_id.clone();
                                        move |cx| {
                                            cx.emit(GitOverflowMenuEvent::StashAll {
                                                project_id: pid.clone(),
                                            });
                                        }
                                    },
                                ))
                                .child(stage_item(
                                    "gom-stash-pop",
                                    "icons/chevron-up.svg",
                                    "Stash Pop",
                                    has_stash,
                                    &t,
                                    cx,
                                    {
                                        let pid = project_id.clone();
                                        move |cx| {
                                            cx.emit(GitOverflowMenuEvent::StashPop {
                                                project_id: pid.clone(),
                                            });
                                        }
                                    },
                                ))
                                .child(stage_item(
                                    "gom-show-stash",
                                    "icons/diff-multiple.svg",
                                    "Show Stash",
                                    has_stash,
                                    &t,
                                    cx,
                                    {
                                        let pid = project_id.clone();
                                        let pos = position;
                                        move |cx| {
                                            cx.emit(GitOverflowMenuEvent::ShowStash {
                                                project_id: pid.clone(),
                                                position: pos,
                                            });
                                        }
                                    },
                                ))
                                .child(menu_separator(&t))
                                .child(discard_item(has_tracked, &t, cx))
                        }
                    }),
            ))
    }
}

fn stage_item<F>(
    id: &'static str,
    icon: &'static str,
    label: &'static str,
    enabled: bool,
    t: &vryn_core::theme::ThemeColors,
    cx: &mut Context<GitOverflowMenu>,
    on_click: F,
) -> Stateful<Div>
where
    F: Fn(&mut Context<GitOverflowMenu>) + 'static,
{
    let item = menu_item_conditional(id, icon, label, enabled, t);
    if enabled {
        item.on_click(cx.listener(move |_this, _, _window, cx| {
            on_click(cx);
        }))
    } else {
        item
    }
}

fn discard_item(
    enabled: bool,
    t: &vryn_core::theme::ThemeColors,
    cx: &mut Context<GitOverflowMenu>,
) -> Stateful<Div> {
    if !enabled {
        return menu_item_conditional(
            "gom-discard-all",
            "icons/trash.svg",
            "Discard All Tracked",
            false,
            t,
        );
    }
    menu_item_with_color(
        "gom-discard-all",
        "icons/trash.svg",
        "Discard All Tracked",
        t.error,
        t.error,
        t,
    )
    .on_click(cx.listener(|this, _, _window, cx| {
        this.confirming_discard = true;
        cx.notify();
    }))
}

impl Focusable for GitOverflowMenu {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
