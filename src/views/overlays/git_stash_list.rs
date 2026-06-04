//! Stash list overlay — shows `git stash list` entries with per-row
//! Apply / Pop / Drop / Show Diff actions.
//!
//! Stash operations are executed directly through the overlay's own
//! [`GitProvider`] handle. On close we emit a `RefreshPanel` event so
//! the git header re-reads working-tree status (which restores its
//! `has_stash` flag).

use crate::keybindings::Cancel;
use crate::theme::theme;
use gpui::prelude::*;
use gpui::*;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use notmux_core::theme::ThemeColors;
use notmux_git::{StashEntry, format_relative_time};
use notmux_views_git::diff_viewer::provider::GitProvider;

pub enum GitStashListEvent {
    /// Overlay was dismissed. Carries the project_id so the git header
    /// can refresh its working-tree state (which sources `has_stash`).
    Close { project_id: String },
}

impl notmux_ui::overlay::CloseEvent for GitStashListEvent {
    fn is_close(&self) -> bool {
        matches!(self, Self::Close { .. })
    }
}

pub struct GitStashList {
    project_id: String,
    provider: Arc<dyn GitProvider>,
    position: Point<Pixels>,
    entries: Vec<StashEntry>,
    loading: bool,
    last_error: Option<String>,
    expanded: HashSet<usize>,
    patches: HashMap<usize, String>,
    focus_handle: FocusHandle,
}

impl GitStashList {
    pub fn new(
        project_id: String,
        provider: Arc<dyn GitProvider>,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let mut this = Self {
            project_id,
            provider,
            position,
            entries: Vec::new(),
            loading: true,
            last_error: None,
            expanded: HashSet::new(),
            patches: HashMap::new(),
            focus_handle,
        };
        this.refresh(cx);
        this
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let provider = self.provider.clone();
        self.loading = true;
        cx.spawn(async move |this, cx| {
            let result = smol::unblock(move || provider.stash_list()).await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(entries) => {
                        this.entries = entries;
                        this.last_error = None;
                        // Drop cached patches/expansions for indices no longer
                        // present (e.g., after a drop renumbered them).
                        let valid: HashSet<usize> = this.entries.iter().map(|e| e.index).collect();
                        this.expanded.retain(|i| valid.contains(i));
                        this.patches.retain(|i, _| valid.contains(i));
                    }
                    Err(e) => {
                        this.last_error = Some(e);
                        this.entries.clear();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn close(&self, cx: &mut Context<Self>) {
        cx.emit(GitStashListEvent::Close {
            project_id: self.project_id.clone(),
        });
    }

    fn apply(&mut self, index: usize, cx: &mut Context<Self>) {
        let provider = self.provider.clone();
        cx.spawn(async move |this, cx| {
            let result = smol::unblock(move || provider.stash_apply(index)).await;
            let _ = this.update(cx, |this, cx| {
                if let Err(e) = result {
                    this.last_error = Some(e);
                    cx.notify();
                } else {
                    // Apply restores changes into the working tree — the user
                    // wants to see the file list, not the stash list. Close.
                    this.close(cx);
                }
            });
        })
        .detach();
    }

    fn pop(&mut self, index: usize, cx: &mut Context<Self>) {
        // Pop only operates on the top of the stack (index 0); we expose it
        // per-row by combining apply+drop on the targeted entry.
        let provider = self.provider.clone();
        cx.spawn(async move |this, cx| {
            let result = smol::unblock(move || -> Result<(), String> {
                provider.stash_apply(index)?;
                provider.stash_drop(index)
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                if let Err(e) = result {
                    this.last_error = Some(e);
                    cx.notify();
                } else {
                    this.close(cx);
                }
            });
        })
        .detach();
    }

    fn drop_entry(&mut self, index: usize, cx: &mut Context<Self>) {
        let provider = self.provider.clone();
        cx.spawn(async move |this, cx| {
            let result = smol::unblock(move || provider.stash_drop(index)).await;
            let _ = this.update(cx, |this, cx| {
                if let Err(e) = result {
                    this.last_error = Some(e);
                    cx.notify();
                } else {
                    this.refresh(cx);
                }
            });
        })
        .detach();
    }

    fn toggle_diff(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.expanded.contains(&index) {
            self.expanded.remove(&index);
            cx.notify();
            return;
        }
        self.expanded.insert(index);
        if self.patches.contains_key(&index) {
            cx.notify();
            return;
        }
        let provider = self.provider.clone();
        cx.spawn(async move |this, cx| {
            let result = smol::unblock(move || provider.stash_show_patch(index)).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(text) => {
                        this.patches.insert(index, text);
                    }
                    Err(e) => {
                        this.patches
                            .insert(index, format!("Failed to load patch: {}", e));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl EventEmitter<GitStashListEvent> for GitStashList {}

impl Render for GitStashList {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        if !self.focus_handle.is_focused(window) {
            window.focus(&self.focus_handle, cx);
        }

        let position = self.position;
        let panel_body = self.render_body(&t, cx);

        div()
            .track_focus(&self.focus_handle)
            .key_context("GitStashList")
            .on_action(cx.listener(|this, _: &Cancel, _window, cx| {
                this.close(cx);
            }))
            .absolute()
            .inset_0()
            .id("git-stash-list-backdrop")
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
                    div()
                        .id("git-stash-list-panel")
                        .occlude()
                        .min_w(px(420.0))
                        .max_w(px(640.0))
                        .max_h(px(520.0))
                        .overflow_y_scroll()
                        .bg(rgb(t.bg_primary))
                        .border_1()
                        .border_color(rgb(t.border))
                        .rounded(px(8.0))
                        .shadow_xl()
                        .py(px(8.0))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_mouse_down(MouseButton::Right, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_scroll_wheel(|_, _, cx| {
                            cx.stop_propagation();
                        })
                        .child(panel_body),
                ),
            ))
    }
}

impl GitStashList {
    fn render_body(&self, t: &ThemeColors, cx: &mut Context<Self>) -> AnyElement {
        if self.loading {
            return div()
                .px(px(12.0))
                .py(px(12.0))
                .text_color(rgb(t.text_secondary))
                .text_size(px(13.0))
                .child("Loading stashes…")
                .into_any_element();
        }

        if let Some(err) = &self.last_error {
            return div()
                .px(px(12.0))
                .py(px(12.0))
                .text_color(rgb(t.error))
                .text_size(px(13.0))
                .child(err.clone())
                .into_any_element();
        }

        if self.entries.is_empty() {
            return div()
                .px(px(12.0))
                .py(px(12.0))
                .text_color(rgb(t.text_secondary))
                .text_size(px(13.0))
                .child("No stash entries.")
                .into_any_element();
        }

        let mut col = gpui_component::v_flex().gap(px(2.0));
        for entry in &self.entries {
            col = col.child(self.render_entry(entry, t, cx));
        }
        col.into_any_element()
    }

    fn render_entry(
        &self,
        entry: &StashEntry,
        t: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let index = entry.index;
        let expanded = self.expanded.contains(&index);
        let patch = self.patches.get(&index).cloned();
        let label = format!("stash@{{{}}}", index);
        let branch = entry
            .branch
            .clone()
            .map(|b| format!(" on {}", b))
            .unwrap_or_default();
        let subject = entry.subject.clone();
        let age = format_relative_time(entry.timestamp_unix);

        gpui_component::v_flex()
            .px(px(8.0))
            .py(px(6.0))
            .gap(px(4.0))
            .child(
                gpui_component::h_flex()
                    .gap(px(8.0))
                    .items_baseline()
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(t.text_muted))
                            .child(label),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(13.0))
                            .text_color(rgb(t.text_primary))
                            .text_ellipsis()
                            .overflow_hidden()
                            .child(format!("{}{}", subject, branch)),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(rgb(t.text_muted))
                            .child(age),
                    ),
            )
            .child(
                gpui_component::h_flex()
                    .gap(px(6.0))
                    .child(action_pill(
                        "gsl-apply",
                        index,
                        "Apply",
                        t,
                        cx,
                        |this, idx, cx| {
                            this.apply(idx, cx);
                        },
                    ))
                    .child(action_pill(
                        "gsl-pop",
                        index,
                        "Pop",
                        t,
                        cx,
                        |this, idx, cx| {
                            this.pop(idx, cx);
                        },
                    ))
                    .child(action_pill_destructive(
                        "gsl-drop",
                        index,
                        "Drop",
                        t,
                        cx,
                        |this, idx, cx| {
                            this.drop_entry(idx, cx);
                        },
                    ))
                    .child(action_pill(
                        "gsl-toggle-diff",
                        index,
                        if expanded { "Hide Diff" } else { "Show Diff" },
                        t,
                        cx,
                        |this, idx, cx| {
                            this.toggle_diff(idx, cx);
                        },
                    )),
            )
            .when(expanded, |d| {
                let patch_id = ElementId::Name(format!("gsl-patch-{}", index).into());
                d.child(
                    div()
                        .id(patch_id)
                        .mt(px(4.0))
                        .px(px(8.0))
                        .py(px(6.0))
                        .bg(rgb(t.bg_header))
                        .border_1()
                        .border_color(rgb(t.border))
                        .rounded(px(4.0))
                        .text_size(px(11.0))
                        .font_family("monospace")
                        .text_color(rgb(t.text_secondary))
                        .max_h(px(240.0))
                        .overflow_y_scroll()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_scroll_wheel(|_, _, cx| {
                            cx.stop_propagation();
                        })
                        .child(patch.unwrap_or_else(|| "Loading…".to_string())),
                )
            })
            .into_any_element()
    }
}

fn action_pill<F>(
    id_prefix: &'static str,
    index: usize,
    label: &'static str,
    t: &ThemeColors,
    cx: &mut Context<GitStashList>,
    on_click: F,
) -> Stateful<Div>
where
    F: Fn(&mut GitStashList, usize, &mut Context<GitStashList>) + 'static,
{
    div()
        .id(ElementId::Name(format!("{}-{}", id_prefix, index).into()))
        .px(px(8.0))
        .py(px(2.0))
        .rounded(px(4.0))
        .text_size(px(12.0))
        .text_color(rgb(t.text_primary))
        .bg(rgb(t.bg_hover))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(t.bg_selection)))
        .on_mouse_down(MouseButton::Left, |_, _, cx| {
            cx.stop_propagation();
        })
        .on_click(cx.listener(move |this, _, _window, cx| {
            on_click(this, index, cx);
        }))
        .child(label)
}

fn action_pill_destructive<F>(
    id_prefix: &'static str,
    index: usize,
    label: &'static str,
    t: &ThemeColors,
    cx: &mut Context<GitStashList>,
    on_click: F,
) -> Stateful<Div>
where
    F: Fn(&mut GitStashList, usize, &mut Context<GitStashList>) + 'static,
{
    div()
        .id(ElementId::Name(format!("{}-{}", id_prefix, index).into()))
        .px(px(8.0))
        .py(px(2.0))
        .rounded(px(4.0))
        .text_size(px(12.0))
        .text_color(rgb(t.error))
        .bg(rgb(t.bg_hover))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(t.bg_selection)))
        .on_mouse_down(MouseButton::Left, |_, _, cx| {
            cx.stop_propagation();
        })
        .on_click(cx.listener(move |this, _, _window, cx| {
            on_click(this, index, cx);
        }))
        .child(label)
}

impl Focusable for GitStashList {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
