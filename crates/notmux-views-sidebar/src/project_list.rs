//! Project and terminal list rendering for the sidebar

use gpui::prelude::*;
use gpui::*;
use gpui_component::tooltip::Tooltip;
use notmux_ui::icon_button::icon_button;
use notmux_ui::rename_state::is_renaming;
use notmux_ui::theme::sidebar_theme as theme;
use notmux_ui::tokens::{ui_text_md, ui_text_sm};
use crate::drag::{FolderDrag, ProjectDrag, ProjectDragView, WorktreeDrag, WorktreeDragView};
use crate::item_widgets::*;
use crate::sidebar::{Sidebar, SidebarProjectInfo};

/// Drag/drop configuration for group header rendering.
/// Determines how project drag and folder drag are handled.
pub enum GroupHeaderDragConfig {
    /// Top-level group header: reorder projects/folders by index.
    TopLevel { index: usize },
    /// Group header inside a folder: move projects into folder at position.
    InFolder { folder_id: String },
}

/// Determines how a project row renders its color dot, badges, and visibility toggle.
pub enum ProjectRowStyle {
    /// Standard project: clickable color dot, worktree badge, rename support.
    Project,
    /// Worktree item: plain hollow dot, optional busy state, rename support.
    Worktree {
        is_orphan: bool,
        is_busy: bool,
        busy_label: &'static str,
    },
    /// Child under a group header: plain solid dot, no rename.
    GroupChild,
}

impl Sidebar {
    /// Appends standard project row content (expand arrow, color dot, name, badges, visibility)
    /// to a pre-configured container div. Each caller sets up its own container with drag/drop.
    pub fn append_project_row_content(
        &self,
        row: Stateful<Div>,
        project: &SidebarProjectInfo,
        id_prefix: &str,
        _group_name: &'static str,
        style: &ProjectRowStyle,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let t = theme(cx);
        let is_expanded = self.expanded_projects.contains(&project.id);
        let project_id = project.id.clone();
        let project_name = project.name.clone();
        let is_renaming_now = is_renaming(&self.project_rename, &project.id);
        let is_busy = matches!(style, ProjectRowStyle::Worktree { is_busy: true, .. });
        let supports_rename = !matches!(style, ProjectRowStyle::GroupChild);

        let has_expandable = match style {
            ProjectRowStyle::Project => {
                project.has_layout || project.worktree_count > 0 || !project.services.is_empty()
            }
            _ => project.has_layout || !project.services.is_empty(),
        };

        let idle_count = if !is_expanded {
            self.count_waiting_terminals(&project.terminal_ids)
        } else {
            0
        };

        row
            // 1. Expand arrow
            .child(if has_expandable {
                sidebar_expand_arrow(
                    ElementId::Name(format!("expand-{}-{}", id_prefix, project.id).into()),
                    is_expanded,
                    &t,
                )
                .on_click(cx.listener({
                    let project_id = project_id.clone();
                    move |this, _, _window, cx| {
                        this.toggle_expanded(&project_id);
                        cx.notify();
                        cx.stop_propagation();
                    }
                }))
                .into_any_element()
            } else {
                sidebar_expand_spacer().into_any_element()
            })
            // 2. Color dot
            .child(match style {
                ProjectRowStyle::Project => {
                    let folder_color = t.get_folder_color(project.folder_color);
                    let pid = project.id.clone();
                    sidebar_color_indicator(
                        ElementId::Name(format!("{}-icon-{}", id_prefix, project.id).into()),
                        folder_icon(folder_color),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                            this.show_color_picker(pid.clone(), event.position, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .into_any_element()
                }
                ProjectRowStyle::Worktree { is_orphan, .. } => {
                    let folder_color = t.get_folder_color(project.folder_color);
                    let dot_color = if *is_orphan { t.warning } else { folder_color };
                    let pid = project.id.clone();
                    sidebar_color_indicator(
                        ElementId::Name(format!("{}-icon-{}", id_prefix, project.id).into()),
                        folder_icon(dot_color),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                            this.show_color_picker(pid.clone(), event.position, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .into_any_element()
                }
                ProjectRowStyle::GroupChild => {
                    let folder_color = t.get_folder_color(project.folder_color);
                    let pid = project.id.clone();
                    sidebar_color_indicator(
                        ElementId::Name(format!("{}-icon-{}", id_prefix, project.id).into()),
                        folder_icon(folder_color),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                            this.show_color_picker(pid.clone(), event.position, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .into_any_element()
                }
            })
            // 3. Name / rename / name+badge
            .child(if is_renaming_now {
                sidebar_rename_input(
                    ElementId::Name(format!("{}-rename-input", id_prefix).into()),
                    &self.project_rename,
                    &t,
                    cx,
                )
                .map(|el| el.into_any_element())
                .unwrap_or_else(|| div().flex_1().into_any_element())
            } else {
                let name_label = sidebar_name_label(
                    ElementId::Name(format!("{}-name-{}", id_prefix, project.id).into()),
                    project_name.clone(),
                    &t,
                    cx,
                )
                .on_click(cx.listener({
                    let project_id = project_id.clone();
                    let project_name = project_name.clone();
                    move |this, _event: &ClickEvent, window, cx| {
                        if supports_rename && this.check_project_double_click(&project_id) {
                            this.start_project_rename(
                                project_id.clone(),
                                project_name.clone(),
                                window,
                                cx,
                            );
                        } else {
                            this.cursor_index = None;
                            this.workspace.update(cx, |ws, cx| {
                                ws.set_focused_project_individual(Some(project_id.clone()), cx);
                            });
                        }
                        cx.stop_propagation();
                    }
                }));
                sidebar_name_or_badge(
                    name_label,
                    &project_name,
                    true,
                    project.terminal_ids.len(),
                    &t,
                    cx,
                )
            })
            // 3b. Metadata: git branch + listening ports of running services
            // (muted, truncating — the name keeps layout priority).
            .when(!is_renaming_now, |d| {
                let mut ports: Vec<u16> = project
                    .services
                    .iter()
                    .filter(|s| {
                        matches!(s.status, notmux_services::manager::ServiceStatus::Running)
                    })
                    .flat_map(|s| s.ports.iter().copied())
                    .collect();
                ports.sort_unstable();
                ports.dedup();
                let port_label = match ports.len() {
                    0 => None,
                    1..=3 => Some(
                        ports
                            .iter()
                            .map(|p| format!(":{p}"))
                            .collect::<Vec<_>>()
                            .join(" "),
                    ),
                    n => Some(format!(":{} +{}", ports[0], n - 1)),
                };
                d.when_some(project.branch.clone(), |d, branch| {
                    d.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(3.0))
                            .max_w(px(110.0))
                            .flex_shrink(1.0)
                            .overflow_hidden()
                            .child(
                                svg()
                                    .path("icons/git-branch.svg")
                                    .size(px(10.0))
                                    .flex_shrink_0()
                                    .text_color(rgb(t.text_muted)),
                            )
                            .child(
                                div()
                                    .text_size(ui_text_sm(cx))
                                    .text_color(rgb(t.text_muted))
                                    .truncate()
                                    .child(branch),
                            ),
                    )
                })
                .when_some(port_label, |d, label| {
                    d.child(
                        div()
                            .flex_shrink_0()
                            .text_size(ui_text_sm(cx))
                            .text_color(rgb(t.text_muted))
                            .child(label),
                    )
                })
            })
            // 4. Idle dot
            .when(idle_count > 0 && !is_busy, |d| {
                d.child(sidebar_idle_dot(&t))
            })
            // 5. Worktree badge (Project style only)
            .when(
                matches!(style, ProjectRowStyle::Project) && project.worktree_count > 0,
                |d| d.child(sidebar_worktree_badge(project.worktree_count, &t, cx)),
            )
            // 6. Busy label (Worktree busy only)
            .when(is_busy, |d| {
                let label = match style {
                    ProjectRowStyle::Worktree { busy_label, .. } => *busy_label,
                    _ => "",
                };
                d.child(
                    div()
                        .ml_auto()
                        .text_size(ui_text_sm(cx))
                        .text_color(rgb(t.text_secondary))
                        .child(label),
                )
            })
    }

    pub fn render_project_item(
        &self,
        project: &SidebarProjectInfo,
        index: usize,
        is_cursor: bool,
        is_focused_project: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx);
        let project_id = project.id.clone();
        let project_name = project.name.clone();

        let row = div()
            .id(ElementId::Name(
                format!("project-row-{}", project.id).into(),
            ))
            .group("project-item")
            .mx(px(6.0))
            .px(px(14.0))
            .py(px(8.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .rounded_lg()
            .cursor_pointer()
            .hover(|s| s.bg(rgb(t.bg_hover)))
            .when(is_focused_project, |d| d.bg(rgb(t.bg_hover)))
            .when(is_cursor, |d| {
                d.bg(rgb(t.bg_hover))
            })
            .on_drag(
                ProjectDrag {
                    project_id: project_id.clone(),
                    project_name: project_name.clone(),
                },
                move |drag, _position, _window, cx| {
                    cx.new(|_| ProjectDragView {
                        name: drag.project_name.clone(),
                    })
                },
            )
            .drag_over::<ProjectDrag>(move |style, _, _, _| {
                style.border_t_2().border_color(rgb(t.border_active))
            })
            .on_drop(cx.listener({
                let project_id = project_id.clone();
                move |this, drag: &ProjectDrag, _window, cx| {
                    if drag.project_id != project_id {
                        this.workspace.update(cx, |ws, cx| {
                            ws.move_project(&drag.project_id, index, cx);
                        });
                    }
                }
            }))
            .drag_over::<FolderDrag>(move |style, _, _, _| {
                style.border_t_2().border_color(rgb(t.border_active))
            })
            .on_drop(cx.listener(move |this, drag: &FolderDrag, _window, cx| {
                this.workspace.update(cx, |ws, cx| {
                    ws.move_item_in_order(&drag.folder_id, index, cx);
                });
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener({
                    let project_id = project_id.clone();
                    move |this, event: &MouseDownEvent, _window, cx| {
                        this.request_context_menu(project_id.clone(), event.position, cx);
                        cx.stop_propagation();
                    }
                }),
            )
            .on_click(cx.listener({
                let project_id = project_id.clone();
                move |this, _, _window, cx| {
                    this.cursor_index = None;
                    this.workspace.update(cx, |ws, cx| {
                        ws.set_focused_project_individual(Some(project_id.clone()), cx);
                    });
                }
            }));

        self.append_project_row_content(
            row,
            project,
            "project",
            "project-item",
            &ProjectRowStyle::Project,
            cx,
        )
    }

    /// Renders a worktree project row. Promoted worktrees use the same indent as their parent
    /// (solid dot, conditional expand arrow). Nested worktrees are indented with a hollow circle.
    #[allow(clippy::too_many_arguments)]
    pub fn render_worktree_item(
        &self,
        project: &SidebarProjectInfo,
        indent: f32,
        worktree_index: usize,
        is_cursor: bool,
        is_focused_project: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx);
        let is_closing = project.is_closing;
        let is_creating = project.is_creating;
        let is_busy = is_closing || is_creating;
        let project_id = project.id.clone();
        let project_name = project.name.clone();
        let parent_id = project.parent_project_id.clone().unwrap_or_default();

        let row = div()
            .id(ElementId::Name(
                format!("worktree-row-{}", project.id).into(),
            ))
            .group("worktree-item")
            .mx(px(6.0))
            .pl(px(indent))
            .pr(px(14.0))
            .py(px(7.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .rounded_lg()
            .when(!is_busy, |d| d.cursor_pointer())
            .when(is_busy, |d| d.opacity(0.5))
            .when(!is_busy, |d| d.hover(|s| s.bg(rgb(t.bg_hover))))
            .when(is_focused_project && !is_busy, |d| d.bg(rgb(t.bg_hover)))
            .when(is_cursor, |d| {
                d.bg(rgb(t.bg_hover))
            })
            .when(!parent_id.is_empty(), |d| {
                let wt_id = project_id.clone();
                let wt_name = project_name.clone();
                let pid = parent_id.clone();
                d.on_drag(
                    WorktreeDrag {
                        worktree_id: wt_id,
                        parent_id: pid,
                        worktree_name: wt_name,
                    },
                    move |drag, _position, _window, cx| {
                        cx.new(|_| WorktreeDragView {
                            name: drag.worktree_name.clone(),
                        })
                    },
                )
            })
            .drag_over::<WorktreeDrag>(move |style, _, _, _| {
                style.border_t_2().border_color(rgb(t.border_active))
            })
            .on_drop(cx.listener({
                let project_id = project_id.clone();
                let parent_id = parent_id.clone();
                move |this, drag: &WorktreeDrag, _window, cx| {
                    if drag.worktree_id != project_id && drag.parent_id == parent_id {
                        this.workspace.update(cx, |ws, cx| {
                            ws.reorder_worktree(&parent_id, &drag.worktree_id, worktree_index, cx);
                        });
                    }
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener({
                    let project_id = project_id.clone();
                    move |this, event: &MouseDownEvent, _window, cx| {
                        this.request_context_menu(project_id.clone(), event.position, cx);
                        cx.stop_propagation();
                    }
                }),
            )
            .on_click(cx.listener({
                let project_id = project_id.clone();
                move |this, _, _window, cx| {
                    this.cursor_index = None;
                    this.workspace.update(cx, |ws, cx| {
                        ws.set_focused_project_individual(Some(project_id.clone()), cx);
                    });
                }
            }));

        let busy_label = if is_creating {
            "Creating\u{2026}"
        } else {
            "Closing\u{2026}"
        };
        self.append_project_row_content(
            row,
            project,
            "wt",
            "worktree-item",
            &ProjectRowStyle::Worktree {
                is_orphan: project.is_orphan,
                is_busy,
                busy_label,
            },
            cx,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render_terminal_item(
        &self,
        project_id: &str,
        terminal_id: &str,
        is_minimized: bool,
        is_inactive_tab: bool,
        is_in_tab_group: bool,
        left_padding: f32,
        id_prefix: &str,
        is_cursor: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx);
        let project_id = project_id.to_string();
        let terminal_id = terminal_id.to_string();
let (terminal_name, has_bell, idle_label, agent_working) = {
            let ws = self.workspace.read(cx);
            let project = ws.project(&project_id);
            let terminals = self.terminals.lock();
            let terminal = terminals.get(terminal_id.as_str());
            let osc_title = terminal.and_then(|t| t.title());
            // Route through `terminal_display_name` so the empty-name fallback
            // (see workspace_data.rs) applies here too — a raw custom-name read
            // would resurrect the blank-label bug when an agent clears its title.
            let name = if let Some(p) = project {
                p.terminal_display_name(terminal_id.as_str(), osc_title)
            } else {
                "Terminal".to_string()
            };
            let bell = terminal.is_some_and(|t| t.has_bell());
            let waiting = terminal.is_some_and(|t| t.is_waiting_for_input());
            let working = terminal.is_some_and(|t| t.agent_working());
            let idle = if bell {
                terminal
                    .and_then(|t| t.last_notification())
                    .map(|n| {
                        let body = n.body.trim();
                        if body.is_empty() {
                            "bell".to_string()
                        } else if body.len() > 18 {
                            format!("{}…", &body[..body.char_indices().take(17).last().map(|(i, c)| i + c.len_utf8()).unwrap_or(18)])
                        } else {
                            body.to_string()
                        }
                    })
            } else if waiting {
                terminal.map(|t| t.idle_duration_display())
            } else {
                None
            };
            (name, bell, idle, working)
            };

        // Check if this terminal is being renamed
        let is_renaming = is_renaming(
            &self.terminal_rename,
            &(project_id.clone(), terminal_id.clone()),
        );

        // Check if this terminal is currently focused
        let is_focused = {
            let ws = self.workspace.read(cx);
            ws.focus_manager.focused_terminal_state().is_some_and(|ft| {
                if let Some(proj) = ws.project(&project_id) {
                    proj.layout
                        .as_ref()
                        .and_then(|l| l.find_terminal_path(&terminal_id))
                        .is_some_and(|path| ft.project_id == project_id && ft.layout_path == path)
                } else {
                    false
                }
            })
        };

        // Pin state (panes pin by their leaf slot id; local projects only)
        let (pin_slot, is_pinned, can_pin) = {
            let ws = self.workspace.read(cx);
            let project = ws.project(&project_id);
            let slot = project
                .and_then(|p| p.layout.as_ref())
                .and_then(|l| l.find_terminal_slot_id(&terminal_id));
            let pinned = project
                .zip(slot.as_ref())
                .is_some_and(|(p, s)| p.pinned_slots.iter().any(|ps| ps == s));
            let can = slot.is_some() && project.is_some_and(|p| !p.is_remote);
            (slot, pinned, can)
        };

        div()
            .id(ElementId::Name(
                format!("{}terminal-item-{}", id_prefix, terminal_id).into(),
            ))
            .group("terminal-item")
            .mx(px(6.0))
            .when(is_in_tab_group, |d| {
                d.ml(px(left_padding))
                    .pl(px(4.0))
                    .border_l_2()
                    .border_color(rgb(t.border))
            })
            .when(!is_in_tab_group, |d| d.pl(px(left_padding)))
            .pr(px(14.0))
            .py(px(5.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .rounded_lg()
            .cursor_pointer()
            .hover(|s| s.bg(rgb(t.bg_hover)))
            .when(is_minimized, |d| d.opacity(0.5))
            .when(is_inactive_tab && !is_minimized, |d| d.opacity(0.5))
            .when(is_focused, |d| d.bg(rgb(t.bg_hover)))
            .when(is_cursor && !is_in_tab_group, |d| {
                d.bg(rgb(t.bg_hover))
            })
            // Click to focus this terminal
            .on_click(cx.listener({
                let project_id = project_id.clone();
                let terminal_id = terminal_id.clone();
                move |this, _, _window, cx| {
                    this.cursor_index = None;
                    this.workspace.update(cx, |ws, cx| {
                        // Clicking a pane in the tree focuses its project
                        // (and thereby leaves the pinned view)
                        ws.set_focused_project(Some(project_id.clone()), cx);
                        ws.focus_terminal_by_id(&project_id, &terminal_id, cx);
                    });
                }
            }))
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(14.0))
                    .h(px(14.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        // Static type icon; the working spinner lives at the
                        // trailing end of the row instead.
                        svg()
                            .path(if has_bell {
                                "icons/bell.svg"
                            } else if is_minimized {
                                "icons/terminal-minimized.svg"
                            } else {
                                "icons/terminal.svg"
                            })
                            .size(px(12.0))
                            .text_color(if has_bell {
                                rgb(t.border_bell)
                            } else if is_minimized || is_inactive_tab {
                                rgb(t.text_muted)
                            } else {
                                rgb(t.text_secondary)
                            }),
                    ),
            )
            .child(
                // Terminal name (or input if renaming)
                if is_renaming {
                    sidebar_rename_input(
                        ElementId::Name(format!("{}terminal-rename-input", id_prefix).into()),
                        &self.terminal_rename,
                        &t,
                        cx,
                    )
                    .map(|el| el.into_any_element())
                    .unwrap_or_else(|| div().flex_1().min_w_0().into_any_element())
                } else {
                    sidebar_name_label(
                        ElementId::Name(
                            format!("{}terminal-name-{}", id_prefix, terminal_id).into(),
                        ),
                        terminal_name.clone(),
                        &t,
                        cx,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_this, _, _, cx| {
                            cx.stop_propagation();
                        }),
                    )
                    .on_click(cx.listener({
                        let project_id = project_id.clone();
                        let terminal_id = terminal_id.clone();
                        let terminal_name = terminal_name.clone();
                        move |this, _event: &ClickEvent, window, cx| {
                            if this.check_double_click(&terminal_id) {
                                this.start_rename(
                                    project_id.clone(),
                                    terminal_id.clone(),
                                    terminal_name.clone(),
                                    window,
                                    cx,
                                );
                            } else {
                                this.cursor_index = None;
                                this.workspace.update(cx, |ws, cx| {
                                    ws.set_focused_project(Some(project_id.clone()), cx);
                                    ws.focus_terminal_by_id(&project_id, &terminal_id, cx);
                                });
                            }
                            cx.stop_propagation();
                        }
                    }))
                    .into_any_element()
                },
            )
            .children(idle_label.map(|d| {
                div()
                    .text_size(ui_text_sm(cx))
                    .text_color(rgb(if has_bell { t.border_bell } else { t.border_idle }))
                    .flex_shrink_0()
                    .child(d)
            }))
            // Pin button: always visible when pinned, hover-only otherwise
            .children(can_pin.then(|| {
                let slot = pin_slot.clone().expect("can_pin implies a slot id");
                icon_button(
                    ElementId::Name(format!("{}pin-{}", id_prefix, terminal_id).into()),
                    if is_pinned {
                        "icons/unpin.svg"
                    } else {
                        "icons/pinned.svg"
                    },
                    &t,
                )
                .when(!is_pinned, |d| {
                    d.opacity(0.0)
                        .group_hover("terminal-item", |s| s.opacity(1.0))
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|_this, _, _, cx| {
                        cx.stop_propagation();
                    }),
                )
                .on_click(cx.listener({
                    let project_id = project_id.clone();
                    move |this, _, _window, cx| {
                        cx.stop_propagation();
                        this.workspace.update(cx, |ws, cx| {
                            ws.toggle_pin(&project_id, &slot, cx);
                        });
                    }
                }))
                .tooltip({
                    let tooltip_text = if is_pinned { "Unpin" } else { "Pin" };
                    move |_window, cx| Tooltip::new(tooltip_text).build(_window, cx)
                })
            }))
            // Working spinner — very end of the row, only while the agent
            // works a turn.
            .children(agent_working.then(|| {
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
                                ElementId::Name(format!("term-spinner-{}", terminal_id).into()),
                                Animation::new(std::time::Duration::from_secs(1)).repeat(),
                                |svg, delta| {
                                    svg.with_transformation(Transformation::rotate(percentage(
                                        delta,
                                    )))
                                },
                            ),
                    )
            }))
    }

    /// A row in the "Editors" group: file icon + basename, click focuses the
    /// editor pane (mirrors the terminal rows).
    #[allow(clippy::too_many_arguments)]
    pub fn render_editor_item(
        &self,
        project_id: &str,
        slot_id: &str,
        file_path: &str,
        left_padding: f32,
        id_prefix: &str,
        is_cursor: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let t = theme(cx);
        let project_id = project_id.to_string();
        let slot_id = slot_id.to_string();
        let file_name = std::path::Path::new(file_path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| {
                if file_path.is_empty() {
                    "Untitled".to_string()
                } else {
                    file_path.to_string()
                }
            });

        let is_focused = {
            let ws = self.workspace.read(cx);
            ws.focus_manager.focused_terminal_state().is_some_and(|ft| {
                ft.project_id == project_id
                    && ws
                        .project(&project_id)
                        .and_then(|p| p.layout.as_ref())
                        .and_then(|l| l.find_editor_path_by_slot(&slot_id))
                        .is_some_and(|path| ft.layout_path == path)
            })
        };

        let (is_pinned, can_pin) = self.pane_pin_state(&project_id, &slot_id, cx);

        div()
            .id(ElementId::Name(
                format!("{}editor-item-{}", id_prefix, slot_id).into(),
            ))
            .group("editor-item")
            .mx(px(6.0))
            .pl(px(left_padding))
            .pr(px(14.0))
            .py(px(5.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .rounded_lg()
            .cursor_pointer()
            .hover(|s| s.bg(rgb(t.bg_hover)))
            .when(is_focused || is_cursor, |d| d.bg(rgb(t.bg_hover)))
            // Click to focus this editor pane
            .on_click(cx.listener({
                let project_id = project_id.clone();
                let slot_id = slot_id.clone();
                move |this, _, _window, cx| {
                    this.cursor_index = None;
                    this.workspace.update(cx, |ws, cx| {
                        // Clicking a pane in the tree focuses its project
                        // (and thereby leaves the pinned view)
                        ws.set_focused_project(Some(project_id.clone()), cx);
                        let path = ws
                            .project(&project_id)
                            .and_then(|p| p.layout.as_ref())
                            .and_then(|l| l.find_editor_path_by_slot(&slot_id));
                        if let Some(path) = path {
                            ws.set_focused_terminal(project_id.clone(), path, cx);
                        }
                    });
                }
            }))
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(14.0))
                    .h(px(14.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        svg()
                            .path("icons/file.svg")
                            .size(px(12.0))
                            .text_color(rgb(t.text_secondary)),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(ui_text_md(cx))
                    .text_color(rgb(t.text_primary))
                    .truncate()
                    .child(file_name),
            )
            .children(can_pin.then(|| {
                self.render_pane_pin_button(
                    ElementId::Name(format!("{}editor-pin-{}", id_prefix, slot_id).into()),
                    "editor-item",
                    &project_id,
                    &slot_id,
                    is_pinned,
                    cx,
                )
            }))
    }

    /// A row in the "Browsers" group: globe icon + host, click focuses the
    /// browser pane (mirrors the editor rows).
    #[allow(clippy::too_many_arguments)]
    pub fn render_browser_item(
        &self,
        project_id: &str,
        slot_id: &str,
        url: &str,
        left_padding: f32,
        id_prefix: &str,
        is_cursor: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let t = theme(cx);
        let project_id = project_id.to_string();
        let slot_id = slot_id.to_string();
        // Label the row by its host, like the browser tab.
        let label = url
            .split("://")
            .nth(1)
            .unwrap_or(url)
            .split('/')
            .next()
            .filter(|h| !h.is_empty())
            .unwrap_or("Browser")
            .to_string();

        let is_focused = {
            let ws = self.workspace.read(cx);
            ws.focus_manager.focused_terminal_state().is_some_and(|ft| {
                ft.project_id == project_id
                    && ws
                        .project(&project_id)
                        .and_then(|p| p.layout.as_ref())
                        .and_then(|l| l.find_browser_path_by_slot(&slot_id))
                        .is_some_and(|path| ft.layout_path == path)
            })
        };

        let (is_pinned, can_pin) = self.pane_pin_state(&project_id, &slot_id, cx);

        div()
            .id(ElementId::Name(
                format!("{}browser-item-{}", id_prefix, slot_id).into(),
            ))
            .group("browser-item")
            .mx(px(6.0))
            .pl(px(left_padding))
            .pr(px(14.0))
            .py(px(5.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .rounded_lg()
            .cursor_pointer()
            .hover(|s| s.bg(rgb(t.bg_hover)))
            .when(is_focused || is_cursor, |d| d.bg(rgb(t.bg_hover)))
            // Click to focus this browser pane
            .on_click(cx.listener({
                let project_id = project_id.clone();
                let slot_id = slot_id.clone();
                move |this, _, _window, cx| {
                    this.cursor_index = None;
                    this.workspace.update(cx, |ws, cx| {
                        // Clicking a pane in the tree focuses its project
                        // (and thereby leaves the pinned view)
                        ws.set_focused_project(Some(project_id.clone()), cx);
                        let path = ws
                            .project(&project_id)
                            .and_then(|p| p.layout.as_ref())
                            .and_then(|l| l.find_browser_path_by_slot(&slot_id));
                        if let Some(path) = path {
                            ws.set_focused_terminal(project_id.clone(), path, cx);
                        }
                    });
                }
            }))
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(14.0))
                    .h(px(14.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        svg()
                            .path("icons/globe.svg")
                            .size(px(12.0))
                            .text_color(rgb(t.text_secondary)),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(ui_text_md(cx))
                    .text_color(rgb(t.text_primary))
                    .truncate()
                    .child(label),
            )
            .children(can_pin.then(|| {
                self.render_pane_pin_button(
                    ElementId::Name(format!("{}browser-pin-{}", id_prefix, slot_id).into()),
                    "browser-item",
                    &project_id,
                    &slot_id,
                    is_pinned,
                    cx,
                )
            }))
    }

    /// Pin state of a pane addressed directly by its slot id (editor/browser rows).
    fn pane_pin_state(&self, project_id: &str, slot_id: &str, cx: &Context<Self>) -> (bool, bool) {
        let ws = self.workspace.read(cx);
        let project = ws.project(project_id);
        let is_pinned =
            project.is_some_and(|p| p.pinned_slots.iter().any(|ps| ps == slot_id));
        let can_pin = project.is_some_and(|p| !p.is_remote);
        (is_pinned, can_pin)
    }

    /// Shared hover pin toggle button for sidebar pane rows.
    fn render_pane_pin_button(
        &self,
        id: ElementId,
        group: &'static str,
        project_id: &str,
        slot_id: &str,
        is_pinned: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let t = theme(cx);
        let project_id = project_id.to_string();
        let slot_id = slot_id.to_string();

        icon_button(
            id,
            if is_pinned {
                "icons/unpin.svg"
            } else {
                "icons/pinned.svg"
            },
            &t,
        )
        // Dimmed but always visible — editor/browser rows have no other
        // hover buttons that would hint at the affordance
        .when(!is_pinned, |d| {
            d.opacity(0.4).group_hover(group, |s| s.opacity(1.0))
        })
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|_this, _, _, cx| {
                cx.stop_propagation();
            }),
        )
        .on_click(cx.listener(move |this, _, _window, cx| {
            cx.stop_propagation();
            this.workspace.update(cx, |ws, cx| {
                ws.toggle_pin(&project_id, &slot_id, cx);
            });
        }))
        .tooltip({
            let tooltip_text = if is_pinned { "Unpin" } else { "Pin" };
            move |_window, cx| Tooltip::new(tooltip_text).build(_window, cx)
        })
    }

    /// Render project as a group header when it has worktrees.
    /// Click = show parent + all worktrees (non-individual focus).
    #[allow(clippy::too_many_arguments)]
    pub fn render_project_group_header(
        &self,
        project: &SidebarProjectInfo,
        left_padding: f32,
        id_prefix: &str,
        group_name: &'static str,
        drag_config: GroupHeaderDragConfig,
        is_cursor: bool,
        is_focused_project: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx);
        let is_expanded = self.is_project_expanded(&project.id, true);
        let project_id = project.id.clone();
        let project_name = project.name.clone();
        let is_renaming = is_renaming(&self.project_rename, &project.id);

        let idle_count = if !is_expanded {
            self.count_waiting_terminals(&project.terminal_ids)
        } else {
            0
        };

        let base = div()
            .id(ElementId::Name(
                format!("{}-{}", id_prefix, project.id).into(),
            ))
            .group(group_name)
            .mx(px(6.0))
            .pl(px(left_padding))
            .pr(px(14.0))
            .py(px(7.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .rounded_lg()
            .cursor_pointer()
            .hover(|s| s.bg(rgb(t.bg_hover)))
            .when(is_focused_project, |d| d.bg(rgb(t.bg_hover)))
            .when(is_cursor, |d| d.bg(rgb(t.bg_hover)))
            .on_drag(
                ProjectDrag {
                    project_id: project_id.clone(),
                    project_name: project_name.clone(),
                },
                move |drag, _position, _window, cx| {
                    cx.new(|_| ProjectDragView {
                        name: drag.project_name.clone(),
                    })
                },
            )
            .drag_over::<ProjectDrag>(move |style, _, _, _| {
                style.border_t_2().border_color(rgb(t.border_active))
            });

        let base = match drag_config {
            GroupHeaderDragConfig::TopLevel { index } => base
                .on_drop(cx.listener({
                    let project_id = project_id.clone();
                    move |this, drag: &ProjectDrag, _window, cx| {
                        if drag.project_id != project_id {
                            this.workspace.update(cx, |ws, cx| {
                                ws.move_project(&drag.project_id, index, cx);
                            });
                        }
                    }
                }))
                .drag_over::<FolderDrag>(move |style, _, _, _| {
                    style.border_t_2().border_color(rgb(t.border_active))
                })
                .on_drop(cx.listener(move |this, drag: &FolderDrag, _window, cx| {
                    this.workspace.update(cx, |ws, cx| {
                        ws.move_item_in_order(&drag.folder_id, index, cx);
                    });
                })),
            GroupHeaderDragConfig::InFolder { folder_id } => base.on_drop(cx.listener({
                let folder_id = folder_id.clone();
                let project_id = project_id.clone();
                move |this, drag: &ProjectDrag, _window, cx| {
                    if drag.project_id != project_id {
                        let pos =
                            this.workspace.read(cx).folder(&folder_id).and_then(|f| {
                                f.project_ids.iter().position(|id| id == &project_id)
                            });
                        if let Some(pos) = pos {
                            this.workspace.update(cx, |ws, cx| {
                                ws.move_project_to_folder(
                                    &drag.project_id,
                                    &folder_id,
                                    Some(pos),
                                    cx,
                                );
                            });
                        }
                    }
                }
            })),
        };

        base.on_mouse_down(
            MouseButton::Right,
            cx.listener({
                let project_id = project_id.clone();
                move |this, event: &MouseDownEvent, _window, cx| {
                    this.request_context_menu(project_id.clone(), event.position, cx);
                    cx.stop_propagation();
                }
            }),
        )
        .on_click(cx.listener({
            let project_id = project_id.clone();
            move |this, _, _window, cx| {
                this.cursor_index = None;
                this.workspace.update(cx, |ws, cx| {
                    ws.set_focused_project(Some(project_id.clone()), cx);
                });
            }
        }))
        .child(
            sidebar_expand_arrow(
                ElementId::Name(format!("expand-{}-{}", id_prefix, project.id).into()),
                is_expanded,
                &t,
            )
            .on_click(cx.listener({
                let project_id = project_id.clone();
                move |this, _, _window, cx| {
                    this.toggle_worktrees_collapsed(&project_id);
                    cx.notify();
                    cx.stop_propagation();
                }
            })),
        )
        .child({
            let folder_color = t.get_folder_color(project.folder_color);
            let project_id = project.id.clone();
            sidebar_color_indicator(
                ElementId::Name(format!("{}-icon-{}", id_prefix, project.id).into()),
                folder_icon(folder_color),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                    this.show_color_picker(project_id.clone(), event.position, cx);
                    cx.stop_propagation();
                }),
            )
        })
        .child(if is_renaming {
            sidebar_rename_input(
                ElementId::Name(format!("{}-rename-input", id_prefix).into()),
                &self.project_rename,
                &t,
                cx,
            )
            .map(|el| el.into_any_element())
            .unwrap_or_else(|| div().flex_1().into_any_element())
        } else {
            sidebar_name_label(
                ElementId::Name(format!("{}-name-{}", id_prefix, project.id).into()),
                project_name.clone(),
                &t,
                cx,
            )
            .font_weight(FontWeight::MEDIUM)
            .on_click(cx.listener({
                let project_id = project_id.clone();
                let project_name = project_name.clone();
                move |this, _event: &ClickEvent, window, cx| {
                    if this.check_project_double_click(&project_id) {
                        this.start_project_rename(
                            project_id.clone(),
                            project_name.clone(),
                            window,
                            cx,
                        );
                    } else {
                        this.cursor_index = None;
                        this.workspace.update(cx, |ws, cx| {
                            ws.set_focused_project(Some(project_id.clone()), cx);
                        });
                    }
                    cx.stop_propagation();
                }
            }))
            .into_any_element()
        })
        .when(idle_count > 0, |d| d.child(sidebar_idle_dot(&t)))
    }

    /// Render main project as a child row under a group header.
    /// Click = show just this project (individual focus).
    #[allow(clippy::too_many_arguments)]
    pub fn render_project_group_child(
        &self,
        project: &SidebarProjectInfo,
        left_padding: f32,
        id_prefix: &str,
        group_name: &'static str,
        is_cursor: bool,
        is_focused_project: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx);
        let project_id = project.id.clone();

        let row = div()
            .id(ElementId::Name(
                format!("{}-{}", id_prefix, project.id).into(),
            ))
            .group(group_name)
            .mx(px(6.0))
            .pl(px(left_padding))
            .pr(px(14.0))
            .py(px(7.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .rounded_lg()
            .cursor_pointer()
            .hover(|s| s.bg(rgb(t.bg_hover)))
            .when(is_focused_project, |d| d.bg(rgb(t.bg_hover)))
            .when(is_cursor, |d| d.bg(rgb(t.bg_hover)))
            .on_click(cx.listener({
                let project_id = project_id.clone();
                move |this, _, _window, cx| {
                    this.cursor_index = None;
                    this.workspace.update(cx, |ws, cx| {
                        ws.set_focused_project_individual(Some(project_id.clone()), cx);
                    });
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener({
                    let project_id = project_id.clone();
                    move |this, event: &MouseDownEvent, _window, cx| {
                        this.request_context_menu(project_id.clone(), event.position, cx);
                        cx.stop_propagation();
                    }
                }),
            );

        self.append_project_row_content(
            row,
            project,
            id_prefix,
            group_name,
            &ProjectRowStyle::GroupChild,
            cx,
        )
    }
}
