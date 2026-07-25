//! PINNED sidebar section: lists pinned panes across local projects.
//! Clicking the header enters the pinned view (all pinned projects side by side).

use gpui::prelude::*;
use gpui::*;
use gpui_component::tooltip::Tooltip;
use notmux_ui::icon_button::icon_button;
use notmux_ui::theme::sidebar_theme as theme;
use notmux_ui::tokens::{ui_text_md, ui_text_ms, ui_text_sm};
use notmux_workspace::state::LayoutNode;

use crate::sidebar::Sidebar;

/// Owned snapshot of one pinned pane for rendering.
struct PinnedRow {
    project_id: String,
    project_name: String,
    slot_id: String,
    icon: &'static str,
    label: String,
    /// True while the pinned terminal's agent is mid-turn — swaps the leading
    /// icon for a rotating spinner (matches the PROJECTS terminal rows).
    working: bool,
}

impl Sidebar {
    /// Render the PINNED section (only when at least one pane is pinned).
    pub fn render_pinned_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.pinned_rows(cx);
        if rows.is_empty() {
            return div().into_any_element();
        }

        let mut children: Vec<AnyElement> = Vec::new();
        children.push(self.render_pinned_header(cx).into_any_element());
        for row in rows {
            children.push(self.render_pinned_row(row, cx).into_any_element());
        }
        // Bottom padding separates the section from the PROJECTS header below
        div().pb(px(8.0)).children(children).into_any_element()
    }

    fn pinned_rows(&self, cx: &Context<Self>) -> Vec<PinnedRow> {
        let ws = self.workspace.read(cx);
        let mut rows = Vec::new();
        for project in ws.data.projects.iter().filter(|p| !p.is_remote) {
            let Some(ref layout) = project.layout else {
                continue;
            };
            for leaf in layout.collect_pinned_leaves(&project.pinned_slots) {
                let (slot_id, icon, label, working) = match &leaf {
                    LayoutNode::Terminal {
                        slot_id,
                        terminal_id,
                        ..
                    } => {
                        let (name, working) = terminal_id
                            .as_ref()
                            .map(|tid| {
                                let terminals = self.terminals.lock();
                                let terminal = terminals.get(tid);
                                let osc = terminal.and_then(|t| t.title());
                                let working = terminal.is_some_and(|t| t.agent_working());
                                (project.terminal_display_name(tid, osc), working)
                            })
                            .unwrap_or_else(|| ("Terminal".to_string(), false));
                        (slot_id.clone(), "icons/terminal.svg", name, working)
                    }
                    LayoutNode::Editor {
                        slot_id, file_path, ..
                    } => {
                        let name = std::path::Path::new(file_path)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .filter(|n| !n.is_empty())
                            .unwrap_or("Untitled")
                            .to_string();
                        (slot_id.clone(), "icons/file.svg", name, false)
                    }
                    LayoutNode::Browser { slot_id, url, .. } => {
                        let name = url
                            .split("://")
                            .nth(1)
                            .unwrap_or(url)
                            .split('/')
                            .next()
                            .filter(|h| !h.is_empty())
                            .unwrap_or("Browser")
                            .to_string();
                        (slot_id.clone(), "icons/globe.svg", name, false)
                    }
                    _ => continue,
                };
                rows.push(PinnedRow {
                    project_id: project.id.clone(),
                    project_name: project.name.clone(),
                    slot_id,
                    icon,
                    label,
                    working,
                });
            }
        }
        rows
    }

    fn render_pinned_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let workspace = self.workspace.clone();
        let is_active = self.workspace.read(cx).data.pinned_view_active;

        div()
            .id("pinned-header")
            .h(px(28.0))
            // Same alignment as the PROJECTS section header below it
            .pl(px(12.0))
            .pr(px(14.0))
            .flex()
            .items_center()
            .cursor_pointer()
            .hover(|s| s.bg(rgb(t.bg_hover)))
            .on_click(move |_, _window, cx| {
                workspace.update(cx, |ws, cx| {
                    ws.enter_pinned_view(cx);
                });
                cx.stop_propagation();
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        svg()
                            .path("icons/pinned.svg")
                            .size(px(14.0))
                            .text_color(rgb(if is_active {
                                t.border_active
                            } else {
                                t.text_secondary
                            })),
                    )
                    .child(
                        div()
                            .text_size(ui_text_ms(cx))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(if is_active {
                                t.border_active
                            } else {
                                t.text_secondary
                            }))
                            .child("PINNED"),
                    ),
            )
    }

    fn render_pinned_row(&self, row: PinnedRow, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let workspace = self.workspace.clone();
        let workspace_for_unpin = self.workspace.clone();
        let project_id = row.project_id;
        let slot_id = row.slot_id;
        let working = row.working;
        let icon = row.icon;

        // While the pinned view is active, mark the entry whose pane has focus
        let is_focused = {
            let ws = self.workspace.read(cx);
            ws.data.pinned_view_active
                && ws.focus_manager.focused_terminal_state().is_some_and(|ft| {
                    ft.project_id == project_id
                        && ws
                            .project(&project_id)
                            .and_then(|p| p.layout.as_ref())
                            .and_then(|l| l.find_path_by_slot_id(&slot_id))
                            .is_some_and(|path| ft.layout_path == path)
                })
        };

        div()
            .id(ElementId::Name(
                format!("pinned-item-{}-{}", project_id, slot_id).into(),
            ))
            .group("pinned-item")
            .h(px(26.0))
            .mx(px(6.0))
            .pl(px(12.0))
            .pr(px(8.0))
            .rounded_lg()
            .flex()
            .items_center()
            .gap(px(8.0))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(t.bg_hover)))
            .when(is_focused, |d| d.bg(rgb(t.bg_hover)))
            .on_click({
                let project_id = project_id.clone();
                let slot_id = slot_id.clone();
                move |_, _window, cx| {
                    workspace.update(cx, |ws, cx| {
                        // A pinned entry always opens the pinned view
                        ws.enter_pinned_view(cx);
                        ws.focus_pane_by_slot(&project_id, &slot_id, cx);
                    });
                    cx.stop_propagation();
                }
            })
            .child(
                // Leading icon slot — static type icon (terminal/file/globe).
                div()
                    .flex_shrink_0()
                    .w(px(14.0))
                    .h(px(14.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        svg()
                            .path(icon)
                            .size(px(12.0))
                            .text_color(rgb(t.text_muted)),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(ui_text_md(cx))
                    .text_color(rgb(t.text_primary))
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(row.label),
            )
            .child(
                div()
                    .text_size(ui_text_sm(cx))
                    .text_color(rgb(t.text_muted))
                    .flex_shrink_0()
                    .child(row.project_name),
            )
            // Unpin button — dimmed, full opacity on row hover
            .child(
                icon_button(
                    ElementId::Name(
                        format!("pinned-unpin-{}-{}", project_id, slot_id).into(),
                    ),
                    "icons/unpin.svg",
                    &t,
                )
                .opacity(0.4)
                .group_hover("pinned-item", |s| s.opacity(1.0))
                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                    cx.stop_propagation();
                })
                .on_click({
                    let project_id = project_id.clone();
                    let slot_id = slot_id.clone();
                    move |_, _window, cx| {
                        cx.stop_propagation();
                        workspace_for_unpin.update(cx, |ws, cx| {
                            ws.toggle_pin(&project_id, &slot_id, cx);
                        });
                    }
                })
                .tooltip(|_window, cx| Tooltip::new("Unpin").build(_window, cx)),
            )
            // Working spinner — very end of the row, only while the pinned
            // pane's agent works a turn.
            .when(working, |d| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .w(px(14.0))
                        .h(px(14.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            svg()
                                .path("icons/spinner.svg")
                                .size(px(12.0))
                                .text_color(rgb(t.text_primary))
                                .with_animation(
                                    ElementId::Name(
                                        format!("pinned-spinner-{}-{}", project_id, slot_id)
                                            .into(),
                                    ),
                                    Animation::new(std::time::Duration::from_secs(1)).repeat(),
                                    |svg, delta| {
                                        svg.with_transformation(Transformation::rotate(
                                            percentage(delta),
                                        ))
                                    },
                                ),
                        ),
                )
            })
    }
}
