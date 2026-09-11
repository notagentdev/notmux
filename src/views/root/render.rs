use crate::keybindings::{
    CheckForUpdates, CreateWorktree, EqualizeLayout,
    FocusNextNotification, FocusSidebar, InstallUpdate, NewProject, OpenBrowser, OpenSettingsFile,
    ShowCommandPalette, ShowContentSearch, ShowDiffViewer, ShowFileSearch, ShowHookLog,
    ShowKeybindings, ShowPairingDialog, ShowProjectSwitcher, ShowSessionManager, ShowSettings,
    ShowThemeSelector, StartAllServices, StopAllServices, ToggleFileExplorer, ToggleGitPanel,
    TogglePaneSwitcher, ToggleSidebar, ToggleSidebarAutoHide,
};
use crate::settings::{open_settings_file, settings_entity};
use crate::theme::{theme, with_alpha};
use crate::ui::tokens::{ui_text_md, ui_text_ms, ui_text_xl};
use crate::views::layout::navigation::{get_pane_map, prune_pane_map};
use crate::views::layout::split_pane::{
    DragState, compute_resize, render_project_divider, render_sidebar_divider,
};
use crate::views::panels::project_column::{PinnedColumnDrag, PinnedColumnDragView};
use crate::workspace::state::{DropZone, PinnedNode, SplitDirection};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use notmux_views_terminal::elements::resize_handle::ResizeHandle;
use crate::workspace::requests::OverlayRequest;
use gpui::prelude::*;
use gpui::*;
use std::future::Future;

use super::{RightView, RootView};

impl RootView {
    /// Applies the active resize drag for a mouse position. Returns true when
    /// the caller must refresh the window (split/column resizes bypass cached
    /// views). Driven from a window-level mouse listener so drags keep working
    /// while the cursor is over occluding hitboxes (e.g. the embedded editor).
    pub(super) fn handle_active_drag_move(
        &mut self,
        position: Point<Pixels>,
        window_width: f32,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(state) = self.active_drag.borrow().clone() else {
            return false;
        };
        match state {
            DragState::Sidebar => {
                let new_width = f32::from(position.x);
                self.sidebar_ctrl.set_width(new_width);
                // Persist through global SettingsState (debounced)
                let width = self.sidebar_ctrl.width();
                settings_entity(cx).update(cx, |s, cx| s.set_sidebar_width(width, cx));
                cx.notify();
                false
            }
            DragState::ServicePanel {
                project_id,
                initial_mouse_y,
                initial_height,
            } => {
                // Dragging up increases height, dragging down decreases
                let delta = initial_mouse_y - f32::from(position.y);
                let new_height = initial_height + delta;
                if let Some(col) = self.project_columns.get(&project_id).cloned() {
                    col.update(cx, |col, cx| {
                        col.set_service_panel_height(new_height, cx);
                    });
                }
                false
            }
            DragState::HookPanel {
                project_id,
                initial_mouse_y,
                initial_height,
            } => {
                let delta = initial_mouse_y - f32::from(position.y);
                let new_height = initial_height + delta;
                if let Some(col) = self.project_columns.get(&project_id).cloned() {
                    col.update(cx, |col, cx| {
                        col.set_hook_panel_height(new_height, cx);
                    });
                }
                false
            }
            DragState::GitPanel => {
                // Git panel sits on the far right.
                let new_width = window_width - f32::from(position.x);
                self.git_panel_ctrl.set_width(new_width);
                let width = self.git_panel_ctrl.width();
                settings_entity(cx).update(cx, |s, cx| s.set_git_panel_width(width, cx));
                cx.notify();
                false
            }
            state => {
                // Split and project column resize
                compute_resize(position, &state, &self.workspace, cx);
                true
            }
        }
    }

    /// Normalize raw project widths to percentages summing to 100%.
    fn normalize_widths(raw_widths: &[f32]) -> Vec<f32> {
        let total: f32 = raw_widths.iter().sum();
        if total > 0.0 {
            raw_widths.iter().map(|w| w / total * 100.0).collect()
        } else {
            let n = raw_widths.len();
            vec![100.0 / n as f32; n]
        }
    }

    /// Convert normalized percentage widths to pixel widths.
    fn to_pixel_widths(widths: &[f32], container_width: f32, min_col_width: f32) -> Vec<f32> {
        let num_dividers = widths.len().saturating_sub(1) as f32;
        let available_width = (container_width - num_dividers * 1.0).max(0.0);
        widths
            .iter()
            .map(|w| (available_width * w / 100.0).max(min_col_width))
            .collect()
    }

    /// Scroll the projects grid horizontally to ensure the focused project column is visible.
    pub(super) fn scroll_to_focused_project(
        &self,
        focused_id: Option<&str>,
        center: bool,
        cx: &Context<Self>,
    ) {
        let focused_id = match focused_id {
            Some(id) => id,
            None => return,
        };

        let workspace = self.workspace.read(cx);

        // Don't scroll when zoomed to a single project
        if workspace.focus_manager.fullscreen_project_id().is_some() {
            return;
        }

        // Pinned view is a fixed multi-agent dashboard: never auto-scroll it.
        // Otherwise focusing one pinned project (e.g. switching agents) would
        // scroll the others off-screen — noticeably the left column once the
        // git panel narrows the grid past the columns' min width.
        if workspace.data.pinned_view_active {
            return;
        }

        let visible_projects: Vec<String> = workspace
            .visible_projects()
            .iter()
            .map(|p| p.id.clone())
            .collect();
        let num_projects = visible_projects.len();
        if num_projects <= 1 {
            return;
        }

        // Find the focused project's index
        let focused_idx = match visible_projects.iter().position(|id| id == focused_id) {
            Some(idx) => idx,
            None => return,
        };

        let settings = settings_entity(cx).read(cx).settings.clone();
        let container_width = f32::from(self.projects_grid_bounds.borrow().size.width);

        let raw_widths: Vec<f32> = visible_projects
            .iter()
            .map(|id| workspace.get_project_width(id, num_projects))
            .collect();
        let widths = Self::normalize_widths(&raw_widths);
        let pixel_widths =
            Self::to_pixel_widths(&widths, container_width, settings.min_column_width);

        // Compute the left edge (x offset) of the focused column
        let mut col_left: f32 = 0.0;
        for pw in pixel_widths.iter().take(focused_idx) {
            col_left += pw + 1.0; // +1 for divider
        }

        let new_offset = if center {
            // Center the focused column in the viewport
            let col_center = col_left + pixel_widths[focused_idx] / 2.0;
            -(col_center - container_width / 2.0)
        } else {
            let col_right = col_left + pixel_widths[focused_idx];
            let current_offset = f32::from(self.projects_scroll_handle.offset().x);
            let viewport_left = -current_offset;
            let viewport_right = viewport_left + container_width;

            if col_left < viewport_left {
                -col_left
            } else if col_right > viewport_right {
                -(col_right - container_width)
            } else {
                return; // already visible
            }
        };

        let max_offset = self.projects_scroll_handle.max_offset();
        let clamped = new_offset.clamp(-f32::from(max_offset.x), 0.0);
        self.projects_scroll_handle
            .set_offset(point(px(clamped), px(0.0)));
    }

    pub(super) fn render_projects_grid(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        // Execute pending center-scroll (deferred from unfocus to let layout update first).
        // We wait until the scroll handle reports overflow (max_offset > 0), which means
        // the layout has been recalculated with all projects visible.
        if let Some(project_id) = self.pending_center_scroll.take() {
            let workspace = self.workspace.read(cx);
            let num_visible = workspace.visible_projects().len();
            let is_zoomed = workspace.focus_manager.focused_project_id().is_some();

            if is_zoomed || num_visible <= 1 {
                // Still zoomed or only one project — no centering needed
            } else if self.projects_scroll_handle.max_offset().x > px(0.0) {
                self.scroll_to_focused_project(Some(&project_id), true, cx);
            } else {
                // Layout hasn't updated yet — re-queue for next frame
                self.pending_center_scroll = Some(project_id);
                cx.notify();
            }
        }

        // Sync project columns to handle newly added projects
        self.sync_project_columns(cx);

        // Pinned view: the project containers arrange as their own layout
        // tree (splits both ways + project-level tab groups) — the isolated
        // drag-and-drop layer one level above the panes.
        let pinned_tree = {
            let ws = self.workspace.read(cx);
            if ws.data.pinned_view_active {
                ws.data.pinned_arrangement.clone()
            } else {
                None
            }
        };
        if let Some(tree) = pinned_tree {
            return self.render_pinned_arrangement(&tree, cx);
        }

        let visible_projects: Vec<_> = {
            let workspace = self.workspace.read(cx);
            // When zoomed, show only the zoomed project's column
            if let Some(pid) = workspace.focus_manager.fullscreen_project_id() {
                vec![pid.to_string()]
            } else {
                workspace
                    .visible_projects()
                    .iter()
                    .map(|p| p.id.clone())
                    .collect()
            }
        };

        let num_projects = visible_projects.len();

        // Evict stale pane map entries for projects no longer rendered
        // (e.g. worktree columns hidden in overview mode)
        {
            let visible_ids: std::collections::HashSet<&str> =
                visible_projects.iter().map(|s| s.as_str()).collect();
            prune_pane_map(&visible_ids);
        }

        // Empty state when folder filter yields no results
        if num_projects == 0 {
            let has_folder_filter = self.workspace.read(cx).active_folder_filter().is_some();
            if has_folder_filter {
                let t = theme(cx);
                let workspace = self.workspace.clone();
                return div()
                    .id("projects-grid-empty")
                    .flex_1()
                    .h_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_size(ui_text_xl(cx))
                            .text_color(rgb(t.text_muted))
                            .child("No projects in this folder"),
                    )
                    .child(
                        div()
                            .id("clear-folder-filter")
                            .text_size(ui_text_md(cx))
                            .text_color(rgb(t.border_active))
                            .cursor_pointer()
                            .hover(|s| s.underline())
                            .child("Show all projects")
                            .on_click(move |_, _window, cx| {
                                workspace.update(cx, |ws, cx| {
                                    ws.set_folder_filter(None, cx);
                                });
                            }),
                    )
                    .into_any_element();
            }
        }

        // Get widths for each project
        let settings = settings_entity(cx).read(cx).settings.clone();

        let widths: Vec<f32> = if num_projects <= 1 {
            vec![100.0; num_projects]
        } else {
            let workspace = self.workspace.read(cx);
            let raw_widths: Vec<f32> = visible_projects
                .iter()
                .map(|id| workspace.get_project_width(id, num_projects))
                .collect();
            Self::normalize_widths(&raw_widths)
        };

        // Persistent bounds reference for resize calculation (survives across renders)
        let container_bounds = self.projects_grid_bounds.clone();

        // Compute pixel widths from percentages, accounting for divider widths
        let container_width = f32::from(container_bounds.borrow().size.width);
        let pixel_widths =
            Self::to_pixel_widths(&widths, container_width, settings.min_column_width);

        // Build interleaved columns and dividers
        let mut elements: Vec<AnyElement> = Vec::new();

        for (i, project_id) in visible_projects.iter().enumerate() {
            let pixel_width = pixel_widths.get(i).copied().unwrap_or(200.0);

            if let Some(col) = self.project_columns.get(project_id).cloned() {
                let col_element = div()
                    .w(px(pixel_width))
                    .flex_shrink_0()
                    .h_full()
                    .child(AnyView::from(col).cached(StyleRefinement::default().size_full()))
                    .into_any_element();

                elements.push(col_element);

                // Add divider after each column except the last
                if i < num_projects - 1 {
                    let min_col_width = settings_entity(cx).read(cx).settings.min_column_width;
                    let divider = render_project_divider(
                        self.workspace.clone(),
                        i,
                        visible_projects.clone(),
                        container_bounds.clone(),
                        &self.active_drag,
                        min_col_width,
                        cx,
                    );
                    elements.push(divider.into_any_element());
                }
            }
        }

        let t = theme(cx);
        let scroll_handle = self.projects_scroll_handle.clone();
        let scrollbar_color = rgb(t.scrollbar);

        let scroll_handle_for_wheel = self.projects_scroll_handle.clone();

        div()
            .id("projects-grid-wrapper")
            .flex_1()
            .h_full()
            .min_w_0()
            .overflow_x_hidden()
            .relative()
            // Horizontal scrolling of project columns: shift+scroll or native touchpad horizontal scroll
            .on_scroll_wheel(
                cx.listener(move |_this, event: &ScrollWheelEvent, _window, cx| {
                    let delta = event.delta.pixel_delta(px(17.0));
                    let scroll_amount = if event.modifiers.shift {
                        // Shift+scroll: use horizontal delta if present, otherwise convert vertical
                        if !delta.x.is_zero() { delta.x } else { delta.y }
                    } else if !delta.x.is_zero() {
                        // Native touchpad horizontal scroll
                        delta.x
                    } else {
                        return;
                    };
                    let max_offset = scroll_handle_for_wheel.max_offset();
                    if max_offset.x <= px(2.0) {
                        return;
                    }
                    let current = scroll_handle_for_wheel.offset();
                    let new_x = (current.x + scroll_amount).clamp(-max_offset.x, px(0.0));
                    scroll_handle_for_wheel.set_offset(point(new_x, current.y));
                    cx.notify();
                }),
            )
            .child(
                div()
                    .id("projects-grid")
                    .size_full()
                    .flex()
                    .overflow_x_hidden()
                    .track_scroll(&self.projects_scroll_handle)
                    // Canvas to capture container bounds (updates persistent bounds for next render)
                    .child(
                        canvas(
                            {
                                let container_bounds = container_bounds.clone();
                                move |bounds, _window, _cx| {
                                    *container_bounds.borrow_mut() = bounds;
                                }
                            },
                            |_bounds, _prepaint, _window, _cx| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    // Mouse handlers are on root div - no need to duplicate here
                    .children(elements),
            )
            // Horizontal scrollbar overlay (absolute positioned at bottom)
            .child({
                let hscroll_bounds = self.hscroll_bounds.clone();
                div()
                    .id("hscrollbar")
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .right_0()
                    .h(px(6.0))
                    .cursor(CursorStyle::Arrow)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                            let max_offset = this.projects_scroll_handle.max_offset();
                            if max_offset.x <= px(2.0) {
                                return;
                            }
                            this.hscroll_dragging = true;
                            // Jump to clicked position
                            if let Some(bounds) = *this.hscroll_bounds.borrow() {
                                let track_width = f32::from(bounds.size.width);
                                let relative_x =
                                    f32::from(event.position.x) - f32::from(bounds.origin.x);
                                let ratio = (relative_x / track_width).clamp(0.0, 1.0);
                                let new_x = -ratio * f32::from(max_offset.x);
                                this.projects_scroll_handle
                                    .set_offset(point(px(new_x), px(0.0)));
                            }
                            cx.notify();
                        }),
                    )
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                        if !this.hscroll_dragging {
                            return;
                        }
                        let max_offset = this.projects_scroll_handle.max_offset();
                        if max_offset.x <= px(2.0) {
                            return;
                        }
                        if let Some(bounds) = *this.hscroll_bounds.borrow() {
                            let track_width = f32::from(bounds.size.width);
                            let relative_x =
                                f32::from(event.position.x) - f32::from(bounds.origin.x);
                            let ratio = (relative_x / track_width).clamp(0.0, 1.0);
                            let new_x = -ratio * f32::from(max_offset.x);
                            this.projects_scroll_handle
                                .set_offset(point(px(new_x), px(0.0)));
                        }
                        cx.notify();
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                            if this.hscroll_dragging {
                                this.hscroll_dragging = false;
                                cx.notify();
                            }
                        }),
                    )
                    .child(
                        canvas(
                            {
                                let hscroll_bounds = hscroll_bounds.clone();
                                move |bounds, _window, _cx| {
                                    *hscroll_bounds.borrow_mut() = Some(bounds);
                                }
                            },
                            move |bounds, _, window, _cx| {
                                let max_scroll = scroll_handle.max_offset();
                                if max_scroll.x <= px(2.0) {
                                    return;
                                }
                                let offset = scroll_handle.offset();
                                let track_width = f32::from(bounds.size.width);
                                let content_width = track_width + f32::from(max_scroll.x);
                                let thumb_width =
                                    (track_width / content_width * track_width).max(30.0);
                                let scroll_ratio = f32::from(-offset.x) / f32::from(max_scroll.x);
                                let thumb_x = scroll_ratio * (track_width - thumb_width);

                                let thumb_bounds = Bounds {
                                    origin: point(
                                        bounds.origin.x + px(thumb_x),
                                        bounds.origin.y + px(1.0),
                                    ),
                                    size: size(px(thumb_width), px(4.0)),
                                };
                                window.paint_quad(
                                    fill(thumb_bounds, scrollbar_color).corner_radii(px(2.0)),
                                );
                            },
                        )
                        .size_full(),
                    )
            })
            .into_any_element()
    }

    /// Pinned view root: render the project-container arrangement tree.
    /// This is the isolated drag layer above the panes — same recursive
    /// split/tabs model, whole projects as leaves, drop spots in orange.
    fn render_pinned_arrangement(
        &mut self,
        tree: &PinnedNode,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Evict bounds of split nodes that no longer exist
        let mut valid: HashSet<Vec<usize>> = HashSet::new();
        collect_pinned_split_paths(tree, &mut Vec::new(), &mut valid);
        self.pinned_split_bounds.retain(|p, _| valid.contains(p));

        div()
            .id("pinned-arrangement")
            .flex_1()
            .h_full()
            .min_w_0()
            .min_h_0()
            .child(self.render_pinned_node(tree, Vec::new(), cx))
            .into_any_element()
    }

    fn render_pinned_node(
        &mut self,
        node: &PinnedNode,
        path: Vec<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match node {
            PinnedNode::Project { project_id } => self.render_pinned_leaf(project_id, cx),
            PinnedNode::Split {
                direction,
                sizes,
                children,
            } => self.render_pinned_split(*direction, sizes, children, path, cx),
            PinnedNode::Tabs {
                children,
                active_tab,
            } => self.render_pinned_tabs(children, *active_tab, path, cx),
        }
    }

    /// A project-container leaf: the project column plus the 5-zone drop
    /// overlay of the container layer (structure copied from the pane layer).
    fn render_pinned_leaf(&mut self, project_id: &str, cx: &mut Context<Self>) -> AnyElement {
        let Some(col) = self.project_columns.get(project_id).cloned() else {
            return div().into_any_element();
        };
        div()
            .id(ElementId::Name(format!("pinned-leaf-{}", project_id).into()))
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .child(AnyView::from(col).cached(StyleRefinement::default().size_full()))
            .child(self.render_pinned_drop_zones(project_id, cx))
            .into_any_element()
    }

    /// The container layer's drop zones — top/bottom/left/right/center,
    /// highlighted in orange, typed to `PinnedColumnDrag` so the pane drop
    /// containers inside the project never react (and vice versa).
    fn render_pinned_drop_zones(
        &self,
        project_id: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx);
        let highlight = with_alpha(t.folder_orange, 0.3);
        let pid = project_id.to_string();
        let active_drag = self.active_drag.clone();

        let make_zone = |zone: DropZone| -> Stateful<Div> {
            let zone_id = format!("pinned-drop-{}-{:?}", pid, zone);
            let pid_for_hover = pid.clone();
            let pid_for_drop = pid.clone();
            let active_drag_for_hover = active_drag.clone();
            let active_drag_for_drop = active_drag.clone();
            div()
                .id(ElementId::Name(zone_id.into()))
                .drag_over::<PinnedColumnDrag>(move |style, drag, _, _| {
                    if active_drag_for_hover.borrow().is_some() {
                        return style;
                    }
                    // No highlight on the dragged container itself
                    if drag.project_id == pid_for_hover {
                        return style;
                    }
                    style.bg(highlight)
                })
                .on_drop(cx.listener(move |this, drag: &PinnedColumnDrag, _window, cx| {
                    if active_drag_for_drop.borrow().is_some() {
                        return;
                    }
                    if drag.project_id == pid_for_drop {
                        return;
                    }
                    this.workspace.update(cx, |ws, cx| {
                        ws.move_pinned_project_zone(&drag.project_id, &pid_for_drop, zone, cx);
                    });
                }))
        };

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .flex_row()
            .child(make_zone(DropZone::Left).w(relative(0.25)).h_full())
            .child(
                div()
                    .w(relative(0.50))
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(make_zone(DropZone::Top).w_full().h(relative(0.25)))
                    .child(make_zone(DropZone::Center).w_full().h(relative(0.50)))
                    .child(make_zone(DropZone::Bottom).w_full().h(relative(0.25))),
            )
            .child(make_zone(DropZone::Right).w(relative(0.25)).h_full())
    }

    fn render_pinned_split(
        &mut self,
        direction: SplitDirection,
        sizes: &[f32],
        children: &[PinnedNode],
        path: Vec<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_horizontal = direction == SplitDirection::Horizontal;
        let bounds_ref = self
            .pinned_split_bounds
            .entry(path.clone())
            .or_insert_with(|| {
                Rc::new(RefCell::new(Bounds {
                    origin: Point::default(),
                    size: Size {
                        width: px(800.0),
                        height: px(600.0),
                    },
                }))
            })
            .clone();

        let total: f32 = sizes.iter().sum();
        let mut elements: Vec<AnyElement> = Vec::new();
        for (i, child) in children.iter().enumerate() {
            if i > 0 {
                elements.push(
                    self.render_pinned_split_divider(
                        i - 1,
                        i,
                        direction,
                        path.clone(),
                        bounds_ref.clone(),
                        cx,
                    )
                    .into_any_element(),
                );
            }
            let size = sizes
                .get(i)
                .copied()
                .unwrap_or(100.0 / children.len() as f32);
            let frac = if total > 0.0 {
                size / total
            } else {
                1.0 / children.len() as f32
            };
            let mut child_path = path.clone();
            child_path.push(i);
            elements.push(
                div()
                    .flex_basis(relative(frac))
                    .min_w_0()
                    .min_h_0()
                    .child(self.render_pinned_node(child, child_path, cx))
                    .into_any_element(),
            );
        }

        div()
            .id(ElementId::Name(format!("pinned-split-{:?}", path).into()))
            .relative()
            .child(
                canvas(
                    {
                        let bounds_ref = bounds_ref.clone();
                        move |bounds, _window, _cx| {
                            *bounds_ref.borrow_mut() = bounds;
                        }
                    },
                    |_bounds, _prepaint, _window, _cx| {},
                )
                .absolute()
                .size_full(),
            )
            .flex()
            .when(is_horizontal, |d| d.flex_col())
            .flex_nowrap()
            .size_full()
            .min_h_0()
            .min_w_0()
            .children(elements)
            .into_any_element()
    }

    fn render_pinned_split_divider(
        &self,
        left_child: usize,
        right_child: usize,
        direction: SplitDirection,
        path: Vec<usize>,
        bounds: Rc<RefCell<Bounds<Pixels>>>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx);
        let active_drag = self.active_drag.clone();
        let workspace = self.workspace.clone();

        ResizeHandle::new(
            direction == SplitDirection::Horizontal,
            t.border,
            t.border_active,
            move |mouse_pos, cx| {
                let b = *bounds.borrow();
                let initial_sizes = workspace
                    .read(cx)
                    .data()
                    .pinned_arrangement
                    .as_ref()
                    .and_then(|tree| tree.get_at_path(&path))
                    .and_then(|n| match n {
                        PinnedNode::Split { sizes, .. } => Some(sizes.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                *active_drag.borrow_mut() = Some(DragState::PinnedSplit {
                    layout_path: path.clone(),
                    left_child,
                    right_child,
                    direction,
                    container_bounds: b,
                    initial_mouse_pos: mouse_pos,
                    initial_sizes,
                });
            },
        )
    }

    /// A project-level tab group: the tab strip replaces the member columns'
    /// title bars; only the active container renders below it. Tabs drag and
    /// drop with the container payload — orange indicators, exactly like the
    /// pane tab strip one level down.
    fn render_pinned_tabs(
        &mut self,
        children: &[PinnedNode],
        active_tab: usize,
        path: Vec<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active_tab = active_tab.min(children.len().saturating_sub(1));
        let strip = self.render_pinned_tab_strip(children, active_tab, &path, cx);
        let mut child_path = path.clone();
        child_path.push(active_tab);
        let body = match children.get(active_tab) {
            Some(child) => self.render_pinned_node(child, child_path, cx),
            None => div().into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .min_w_0()
            .child(strip)
            .child(div().flex_1().min_h_0().min_w_0().child(body))
            .into_any_element()
    }

    fn render_pinned_tab_strip(
        &self,
        children: &[PinnedNode],
        active_tab: usize,
        path: &[usize],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx);

        // Snapshot per-tab display data in one workspace read
        struct TabInfo {
            label: String,
            icon_color: u32,
            /// Draggable single-project tabs carry their project id
            leaf_project: Option<String>,
        }
        let tabs: Vec<TabInfo> = {
            let ws = self.workspace.read(cx);
            children
                .iter()
                .map(|child| {
                    let ids = child.collect_project_ids();
                    let names: Vec<String> = ids
                        .iter()
                        .filter_map(|id| ws.project(id).map(|p| p.name.clone()))
                        .collect();
                    let icon_color = ids
                        .first()
                        .and_then(|id| ws.project(id))
                        .map(|p| {
                            let color = ws.effective_folder_color(p);
                            if color != crate::theme::FolderColor::Default {
                                t.get_folder_color(color)
                            } else {
                                t.text_secondary
                            }
                        })
                        .unwrap_or(t.text_secondary);
                    let leaf_project = match child {
                        PinnedNode::Project { project_id } => Some(project_id.clone()),
                        _ => None,
                    };
                    TabInfo {
                        label: names.join(" / "),
                        icon_color,
                        leaf_project,
                    }
                })
                .collect()
        };

        let tab_elements: Vec<AnyElement> = tabs
            .into_iter()
            .enumerate()
            .map(|(i, info)| {
                let is_active = i == active_tab;
                let path_for_click = path.to_vec();
                let path_for_drop = path.to_vec();
                let active_drag_for_hover = self.active_drag.clone();
                let active_drag_for_drop = self.active_drag.clone();
                let leaf_for_hover = info.leaf_project.clone();
                let tab_group = SharedString::from(format!("pinned-tab-{}-{:?}", i, path));

                // Hover-revealed unpin inside the tab (pane-tab end slot),
                // for single-project tabs only.
                let end_slot = info.leaf_project.clone().map(|unpin_project| {
                    let cover_bg = if is_active { t.bg_secondary } else { t.bg_hover };
                    let bg_hover = t.bg_hover;
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right_0()
                        .flex()
                        .items_center()
                        .pr(px(3.0))
                        .pl(px(12.0))
                        .bg(rgb(cover_bg))
                        .opacity(0.0)
                        .group_hover(tab_group.clone(), |s| s.opacity(1.0))
                        .child(
                            div()
                                .id(ElementId::Name(
                                    format!("pinned-tab-unpin-{}-{:?}", i, path).into(),
                                ))
                                .flex_none()
                                .w(px(18.0))
                                .h(px(18.0))
                                .flex()
                                .justify_center()
                                .items_center()
                                .rounded(px(4.0))
                                .cursor_pointer()
                                .hover(move |s| s.bg(rgb(bg_hover)))
                                .child(
                                    svg()
                                        .path("icons/unpin.svg")
                                        .size(px(12.0))
                                        .text_color(rgb(t.text_muted)),
                                )
                                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation();
                                })
                                .on_click(cx.listener(move |this, _, _window, cx| {
                                    cx.stop_propagation();
                                    this.workspace.update(cx, |ws, cx| {
                                        ws.unpin_all_in_project(&unpin_project, cx);
                                    });
                                })),
                        )
                });

                let tab = div()
                    .id(ElementId::Name(
                        format!("pinned-tab-{}-{:?}", i, path).into(),
                    ))
                    .group(tab_group.clone())
                    .cursor_pointer()
                    .relative()
                    .flex_shrink_0()
                    .max_w(px(220.0))
                    .h(px(22.0))
                    .mx(px(2.0))
                    .rounded(px(6.0))
                    .overflow_hidden()
                    .border_1()
                    .text_size(ui_text_md(cx))
                    .when(is_active, |d| {
                        d.bg(rgb(t.bg_secondary))
                            .border_color(rgb(t.border))
                            .text_color(rgb(t.text_primary))
                    })
                    .when(!is_active, |d| {
                        d.border_color(with_alpha(t.border, 0.0))
                            .text_color(rgb(t.text_secondary))
                            .hover(|s| s.bg(rgb(t.bg_hover)))
                    })
                    .child(
                        div()
                            .h_full()
                            .w_full()
                            .px(px(8.0))
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .overflow_hidden()
                            .child(
                                svg()
                                    .path("icons/pinned.svg")
                                    .size(px(12.0))
                                    .flex_shrink_0()
                                    .text_color(rgb(info.icon_color)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(info.label.clone()),
                            )
                            .children(end_slot),
                    )
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.workspace.update(cx, |ws, cx| {
                            ws.set_pinned_active_tab(&path_for_click, i, cx);
                        });
                    }))
                    // Drop spot of the container layer: insert at this tab's
                    // position (orange, exactly like the pane tab strip)
                    .drag_over::<PinnedColumnDrag>(move |style, drag: &PinnedColumnDrag, _, _| {
                        if active_drag_for_hover.borrow().is_some() {
                            return style;
                        }
                        if leaf_for_hover.as_deref() == Some(drag.project_id.as_str()) {
                            return style;
                        }
                        style
                            .border_l(px(3.0))
                            .border_color(rgb(t.folder_orange))
                            .bg(with_alpha(t.folder_orange, 0.15))
                    })
                    .on_drop(cx.listener(move |this, drag: &PinnedColumnDrag, _window, cx| {
                        if active_drag_for_drop.borrow().is_some() {
                            return;
                        }
                        this.workspace.update(cx, |ws, cx| {
                            ws.move_pinned_project_to_tab_group(
                                &drag.project_id,
                                &path_for_drop,
                                Some(i),
                                cx,
                            );
                        });
                    }));

                // Single-project tabs drag as whole containers
                let tab = if let Some(project_id) = info.leaf_project {
                    let project_name = info.label.clone();
                    tab.on_drag(
                        PinnedColumnDrag {
                            project_id,
                            project_name,
                        },
                        move |drag, _position, _window, cx| {
                            cx.new(|_| PinnedColumnDragView {
                                name: drag.project_name.clone(),
                            })
                        },
                    )
                } else {
                    tab
                };
                tab.into_any_element()
            })
            .collect();

        // End filler: drop appends to the group; the empty strip area also
        // moves the window — but only when the strip sits at the window's
        // top edge (mirror of the pane tab strips).
        let strip_bounds: Rc<RefCell<Bounds<Pixels>>> = Rc::new(RefCell::new(Bounds {
            origin: point(px(0.0), px(9999.0)),
            size: Size::default(),
        }));
        let path_for_end = path.to_vec();
        let active_drag_for_end_hover = self.active_drag.clone();
        let active_drag_for_end_drop = self.active_drag.clone();
        let end_zone = div()
            .id(ElementId::Name(format!("pinned-tab-end-{:?}", path).into()))
            .flex_1()
            .h_full()
            .min_w(px(20.0))
            .drag_over::<PinnedColumnDrag>(move |style, _: &PinnedColumnDrag, _, _| {
                if active_drag_for_end_hover.borrow().is_some() {
                    return style;
                }
                style
                    .border_l(px(3.0))
                    .border_color(rgb(t.folder_orange))
                    .bg(with_alpha(t.folder_orange, 0.1))
            })
            .on_drop(cx.listener(move |this, drag: &PinnedColumnDrag, _window, cx| {
                if active_drag_for_end_drop.borrow().is_some() {
                    return;
                }
                this.workspace.update(cx, |ws, cx| {
                    ws.move_pinned_project_to_tab_group(&drag.project_id, &path_for_end, None, cx);
                });
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener({
                    let strip_bounds = strip_bounds.clone();
                    move |this, _, _, _| {
                        if strip_bounds.borrow().origin.y < px(6.0) {
                            this.title_should_move = true;
                        }
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.title_should_move = false),
            )
            .on_mouse_move(cx.listener(|this, _, window, _| {
                if this.title_should_move {
                    this.title_should_move = false;
                    window.start_window_move();
                }
            }));

        div()
            .h(px(notmux_ui::tokens::TITLE_BAR_STRIP_H))
            .flex_shrink_0()
            .relative()
            .px(px(6.0))
            .flex()
            .items_center()
            .bg(rgb(t.bg_header))
            .border_b_1()
            .border_color(rgb(t.border))
            .child(
                canvas(
                    {
                        let strip_bounds = strip_bounds.clone();
                        move |bounds, _window, _cx| {
                            *strip_bounds.borrow_mut() = bounds;
                        }
                    },
                    |_bounds, _prepaint, _window, _cx| {},
                )
                .absolute()
                .size_full(),
            )
            .children(tab_elements)
            .child(end_zone)
            .into_any_element()
    }
}

/// Collect the tree paths of all Split nodes (for bounds-map eviction).
fn collect_pinned_split_paths(
    node: &PinnedNode,
    path: &mut Vec<usize>,
    out: &mut HashSet<Vec<usize>>,
) {
    match node {
        PinnedNode::Split { children, .. } => {
            out.insert(path.clone());
            for (i, child) in children.iter().enumerate() {
                path.push(i);
                collect_pinned_split_paths(child, path, out);
                path.pop();
            }
        }
        PinnedNode::Tabs { children, .. } => {
            for (i, child) in children.iter().enumerate() {
                path.push(i);
                collect_pinned_split_paths(child, path, out);
                path.pop();
            }
        }
        PinnedNode::Project { .. } => {}
    }
}

impl Render for RootView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);

        let transparent = settings_entity(cx).read(cx).settings.transparent_background;
        let om = self.overlay_manager.read(cx);
        let has_context_menu = om.has_context_menu();
        let has_folder_context_menu = om.has_folder_context_menu();
        let has_remote_context_menu = om.has_remote_context_menu();
        let has_terminal_context_menu = om.has_terminal_context_menu();
        let has_git_file_context_menu = om.has_git_file_context_menu();
        let has_explorer_context_menu = om.has_explorer_context_menu();
        let has_git_overflow_menu = om.has_git_overflow_menu();
        let has_git_stash_list = om.has_git_stash_list();
        let has_tab_context_menu = om.has_tab_context_menu();
        let has_worktree_list = om.has_worktree_list();
        let has_color_picker = om.has_color_picker();
        let settings_panel = om.render_settings_panel();
        let _ = om;
        let main_diff_viewer = self.main_diff_viewer.clone();
        let main_file_viewer = self.main_file_viewer.clone();

        // Native webviews float above all GPUI content; suspend them while a
        // full-window viewer covers the middle panel. (Modal overlays and the
        // settings panel are handled via focus_manager.is_modal() in the
        // browser pane itself.)
        notmux_ui::webview_gate::set_webviews_suspended(
            main_diff_viewer.is_some() || main_file_viewer.is_some(),
        );

        // Get active drag for global mouse handling
        let active_drag = self.active_drag.clone();
        let workspace = self.workspace.clone();

        // Capture sidebar state for mouse move handler
        let sidebar_auto_hide = self.sidebar_ctrl.is_auto_hide();
        let sidebar_hover_shown = self.sidebar_ctrl.is_hover_shown();
        let current_sidebar_width = self.sidebar_ctrl.current_width();

        // Clone overlay_manager for action handlers
        let overlay_manager = self.overlay_manager.clone();

        let focus_handle = self.focus_handle.clone();

        // Focus root if nothing else is focused (allows global keybindings to work)
        if window.focused(cx).is_none() {
            window.focus(&focus_handle, cx);
        }

        // Title-bar overlay reserves: when the sidebar is collapsed the grid
        // reaches the window's left edge, so the top-left pane insets its tabs
        // Every view now renders a title bar above each column (projects and
        // pinned alike), so the window chrome never floats over a pane's tab
        // bar — the tab-strip reserves stay 0 and the chrome insets go to the
        // first/last column's title bar instead: traffic lights + left toggle
        // on the left, git/settings cluster and Windows caption buttons on
        // the right.
        let chrome_left = if self.sidebar_ctrl.should_render() {
            0.0
        } else {
            // 80 traffic-light pad + 28 toggle + 4 gap + 28 bell + 10 gap,
            // plus the Pair button while the remote server is running.
            let pair = if crate::views::chrome::title_bar::pair_button_active(cx) {
                32.0
            } else {
                0.0
            };
            150.0 + pair
        };
        notmux_views_terminal::set_tab_action_left_reserve(0.0, cx);
        // Match the title-bar overlay height so the tab strip centers with the
        // traffic lights / controls floating over it. Same constant positions
        // the fullscreen overlays (file/diff viewer) below the strip.
        notmux_views_terminal::set_tab_bar_height(notmux_ui::tokens::TITLE_BAR_STRIP_H, cx);

        let git_open = self.git_panel_ctrl.should_render();
        let mut chrome_right = 0.0_f32;
        if !git_open {
            chrome_right += 64.0; // git-panel toggle + settings + gaps/pad
        }
        if cfg!(target_os = "windows") {
            chrome_right += 138.0; // three 46px caption buttons
        }
        notmux_views_terminal::set_tab_action_right_reserve(0.0, cx);
        crate::views::panels::project_column::set_column_title_reserves(
            chrome_left,
            chrome_right,
            cx,
        );
        notmux_views_terminal::set_tab_action_right_reserve_path(None, cx);

        div()
            .id("root")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            // Translucent chrome over the system blur (vibrancy): only the
            // sidebar region actually shows it — the center and panels paint
            // opaque backgrounds on top. Exactly the notagent root chrome:
            // surface_2 at 90% alpha.
            .bg(if transparent {
                let mut chrome = cx.global::<crate::theme::GpuiTheme>().surface_2();
                chrome.a = 0.9;
                chrome
            } else {
                with_alpha(t.bg_primary, 1.0)
            })
            .track_focus(&focus_handle)
            // Reset the title-bar window-move flag on any mouse-up.
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.title_should_move = false),
            )
            // Global mouse move handler for window-move latch and sidebar
            // auto-hide. Resize drags are handled by a window-level listener
            // (registered in the canvas below): element listeners stop firing
            // when the cursor crosses an occluding hitbox (e.g. the embedded
            // file viewer), which froze split resizes toward the editor.
            .on_mouse_move(cx.listener({
                move |this, event: &MouseMoveEvent, window, cx| {
                    // Deferred window move: a title-bar mouse-down armed the flag;
                    // start the native drag on the first move.
                    if this.title_should_move {
                        this.title_should_move = false;
                        window.start_window_move();
                    }
                    // Handle auto-hide: check if mouse left the sidebar area
                    if sidebar_auto_hide && sidebar_hover_shown {
                        // Add small margin for smoother interaction
                        let hide_threshold = current_sidebar_width + 10.0;
                        if f32::from(event.position.x) > hide_threshold {
                            this.hide_sidebar_on_leave(cx);
                        }
                    }
                }
            }))
            // Global mouse up handler to end resize (registered via window event
            // to reliably fire regardless of which child element the cursor is over)
            .child(
                canvas(|_bounds, _window, _cx| {}, {
                    let active_drag = active_drag.clone();
                    let terminals = self.terminals.clone();
                    let workspace = workspace.clone();
                    let this_weak = cx.entity().downgrade();
                    move |_bounds, _prepaint, window, _cx| {
                        let active_drag = active_drag.clone();
                        let terminals = terminals.clone();
                        let workspace = workspace.clone();
                        // Window-level move handler drives all resize drags:
                        // unlike an element listener it keeps firing when the
                        // cursor is over occluding hitboxes (file viewer,
                        // scrollables), so splits shrink as well as grow.
                        {
                            let active_drag = active_drag.clone();
                            let this_weak = this_weak.clone();
                            window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                                if phase != DispatchPhase::Bubble {
                                    return;
                                }
                                if active_drag.borrow().is_none() {
                                    return;
                                }
                                let window_width = f32::from(window.bounds().size.width);
                                let needs_refresh = this_weak
                                    .update(cx, |this, cx| {
                                        this.handle_active_drag_move(e.position, window_width, cx)
                                    })
                                    .unwrap_or(false);
                                if needs_refresh {
                                    // Bypass all .cached() views so terminal elements
                                    // repaint with new bounds during drag.
                                    window.refresh();
                                }
                            });
                        }
                        window.on_mouse_event(move |e: &MouseUpEvent, phase, _window, cx| {
                            if phase == DispatchPhase::Bubble && e.button == MouseButton::Left {
                                let was_split_drag = matches!(
                                    *active_drag.borrow(),
                                    Some(DragState::Split { .. })
                                        | Some(DragState::PinnedSplit { .. })
                                );
                                let was_dragging = active_drag.borrow().is_some();
                                *active_drag.borrow_mut() = None;

                                if was_dragging {
                                    let terminals_guard = terminals.lock();
                                    for terminal in terminals_guard.values() {
                                        terminal.flush_pending_resize();
                                    }
                                }

                                // Persist final split sizes (drag used ui_only notify)
                                if was_split_drag {
                                    workspace.update(cx, |ws, cx| {
                                        ws.notify_data(cx);
                                    });
                                }
                            }
                        });
                    }
                })
                .absolute()
                .size_full(),
            )
            // Handle sidebar toggle action from title bar
            .on_action(cx.listener(|this, _: &ToggleSidebar, _window, cx| {
                this.toggle_sidebar(cx);
            }))
            // Handle toggle sidebar auto-hide action
            .on_action(cx.listener(|this, _: &ToggleSidebarAutoHide, _window, cx| {
                this.toggle_sidebar_auto_hide(cx);
            }))
            // Handle toggle git panel action
            // Git panel = the Git tab of the tabbed right panel.
            .on_action(cx.listener(|this, _: &ToggleGitPanel, _window, cx| {
                this.toggle_right_panel(RightView::Git, cx);
            }))
            // File explorer = the Files tab of the tabbed right panel.
            .on_action(cx.listener(|this, _: &ToggleFileExplorer, _window, cx| {
                this.toggle_right_panel(RightView::Files, cx);
            }))
            // Open the embedded browser pane in the active project.
            .on_action(cx.listener(|this, _: &OpenBrowser, _window, cx| {
                let ws = this.workspace.read(cx);
                let project_id = ws
                    .focus_manager
                    .focused_terminal_state()
                    .map(|state| state.project_id)
                    .or_else(|| ws.focus_manager.focused_project_id().cloned())
                    .or_else(|| ws.data.projects.first().map(|p| p.id.clone()));
                if let Some(project_id) = project_id {
                    this.workspace.update(cx, |ws, cx| {
                        ws.add_browser_right(&project_id, "", cx);
                    });
                }
            }))
            // Handle equalize layout action
            .on_action(cx.listener(|this, _: &FocusNextNotification, _window, cx| {
    let terminals = this.terminals.lock();
    let ws = this.workspace.read(cx);
    let all_with_bell: Vec<(String, String)> = ws
        .data
        .projects
        .iter()
        .flat_map(|p| {
            let ids = p
                .layout
                .as_ref()
                .map(|l| l.collect_terminal_ids())
                .unwrap_or_default();
            ids.into_iter().map(move |tid| (p.id.clone(), tid))
        })
        .filter(|(_, tid)| terminals.get(tid).is_some_and(|t| t.has_bell()))
        .collect();
    drop(terminals);
    if let Some((project_id, terminal_id)) = all_with_bell.into_iter().next() {
        this.workspace.update(cx, |ws, cx| {
            ws.focus_terminal_by_id(&project_id, &terminal_id, cx);
        });
    }
}))
.on_action(cx.listener(|this, _: &EqualizeLayout, _window, cx| {
                this.workspace.update(cx, |ws, cx| {
                    ws.data.project_widths.clear();
                    ws.equalize_focused_split(cx);
                });
            }))
            // Handle focus sidebar action (keyboard navigation)
            .on_action(cx.listener(|this, _: &FocusSidebar, window, cx| {
                // Ensure sidebar is visible
                if !this.sidebar_ctrl.is_open() && !this.sidebar_ctrl.is_hover_shown() {
                    this.toggle_sidebar(cx);
                }
                let current_focus = window.focused(cx);
                let handle = this.sidebar.read(cx).focus_handle().clone();
                this.sidebar.update(cx, |sidebar, cx| {
                    sidebar.saved_focus = current_focus;
                    sidebar.activate_cursor(cx);
                });
                window.focus(&handle, cx);
            }))
            // Handle show keybindings action
            .on_action(cx.listener({
                let overlay_manager = overlay_manager.clone();
                move |_this, _: &ShowKeybindings, _window, cx| {
                    overlay_manager.update(cx, |om, cx| om.toggle_keybindings_help(cx));
                }
            }))
            // Handle show session manager action
            .on_action(cx.listener({
                let overlay_manager = overlay_manager.clone();
                move |_this, _: &ShowSessionManager, _window, cx| {
                    overlay_manager.update(cx, |om, cx| om.toggle_session_manager(cx));
                }
            }))
            // Handle show theme selector action
            .on_action(cx.listener({
                let overlay_manager = overlay_manager.clone();
                move |_this, _: &ShowThemeSelector, _window, cx| {
                    overlay_manager.update(cx, |om, cx| om.toggle_theme_selector(cx));
                }
            }))
            // Handle show command palette action
            .on_action(cx.listener({
                let overlay_manager = overlay_manager.clone();
                move |_this, _: &ShowCommandPalette, _window, cx| {
                    overlay_manager.update(cx, |om, cx| om.toggle_command_palette(cx));
                }
            }))
            // Handle show settings panel action
            .on_action(cx.listener({
                let overlay_manager = overlay_manager.clone();
                move |this, _: &ShowSettings, _window, cx| {
                    this.main_diff_viewer = None;
                    overlay_manager.update(cx, |om, cx| om.toggle_settings_panel(cx));
                }
            }))
            // Handle show hook log action
            .on_action(cx.listener({
                let overlay_manager = overlay_manager.clone();
                move |_this, _: &ShowHookLog, _window, cx| {
                    overlay_manager.update(cx, |om, cx| om.toggle_hook_log(cx));
                }
            }))
            // Handle show pairing dialog action
            .on_action(cx.listener({
                let overlay_manager = overlay_manager.clone();
                move |_this, _: &ShowPairingDialog, _window, cx| {
                    overlay_manager.update(cx, |om, cx| om.toggle_pairing_dialog(cx));
                }
            }))
            // Handle new project action
            .on_action(cx.listener({
                let overlay_manager = overlay_manager.clone();
                move |this, _: &NewProject, _window, cx| {
                    let rm = this.remote_manager.clone();
                    overlay_manager.update(cx, |om, cx| om.toggle_add_project_dialog(rm, cx));
                }
            }))
            // Handle open settings file action
            .on_action(cx.listener(|_this, _: &OpenSettingsFile, _window, _cx| {
                open_settings_file();
            }))
            // Handle check for updates action
            .on_action(cx.listener(|_this, _: &CheckForUpdates, _window, cx| {
                if let Some(update_info) = cx.try_global::<notmux_ext_updater::GlobalUpdateInfo>() {
                    let info = update_info.0.clone();

                    // Prevent concurrent manual checks
                    if !info.try_start_manual() {
                        return;
                    }

                    info.set_status(notmux_ext_updater::UpdateStatus::Checking);
                    let token = info.current_token();
                    cx.notify();
                    cx.spawn(async move |_this, cx| {
                        match notmux_ext_updater::checker::check_for_update(info.app_version()).await
                        {
                            Ok(Some(release)) => {
                                if info.is_homebrew() {
                                    info.set_status(notmux_ext_updater::UpdateStatus::BrewUpdate {
                                        version: release.version,
                                    });
                                    let _ = _this.update(cx, |_, cx| cx.notify());
                                } else {
                                    // Set downloading status and notify before the blocking download
                                    info.set_status(notmux_ext_updater::UpdateStatus::Downloading {
                                        version: release.version.clone(),
                                        progress: 0,
                                    });
                                    let _ = _this.update(cx, |_, cx| cx.notify());

                                    // Download with periodic UI refresh for progress
                                    let download = notmux_ext_updater::downloader::download_asset(
                                        release.asset_url,
                                        release.asset_name,
                                        release.version.clone(),
                                        info.clone(),
                                        token,
                                        release.checksum_url,
                                    );
                                    let mut download = std::pin::pin!(download);

                                    let download_result: anyhow::Result<std::path::PathBuf> = loop {
                                        let polled = std::future::poll_fn(|task_cx| match download
                                            .as_mut()
                                            .poll(task_cx)
                                        {
                                            std::task::Poll::Ready(r) => {
                                                std::task::Poll::Ready(Some(r))
                                            }
                                            std::task::Poll::Pending => {
                                                std::task::Poll::Ready(None)
                                            }
                                        })
                                        .await;
                                        match polled {
                                            Some(r) => break r,
                                            None => {
                                                smol::Timer::after(
                                                    std::time::Duration::from_millis(250),
                                                )
                                                .await;
                                                let _ = _this.update(cx, |_, cx| cx.notify());
                                            }
                                        }
                                    };

                                    match download_result {
                                        Ok(path) => {
                                            info.set_status(
                                                notmux_ext_updater::UpdateStatus::Ready {
                                                    version: release.version,
                                                    path,
                                                },
                                            );
                                            let _ = _this.update(cx, |_, cx| cx.notify());
                                        }
                                        Err(e) => {
                                            log::error!("Download failed: {}", e);
                                            info.set_status(
                                                notmux_ext_updater::UpdateStatus::Failed {
                                                    error: e.to_string(),
                                                },
                                            );
                                            let _ = _this.update(cx, |_, cx| cx.notify());
                                        }
                                    }
                                }
                            }
                            Ok(None) => {
                                info.set_status(notmux_ext_updater::UpdateStatus::Idle);
                                let _ = _this.update(cx, |_, cx| cx.notify());
                            }
                            Err(e) => {
                                log::error!("Update check failed: {}", e);
                                info.set_status(notmux_ext_updater::UpdateStatus::Failed {
                                    error: e.to_string(),
                                });
                                let _ = _this.update(cx, |_, cx| cx.notify());
                            }
                        }

                        info.finish_manual();
                    })
                    .detach();
                }
            }))
            // Handle install update action (dispatched from status bar)
            .on_action(cx.listener(|_this, _: &InstallUpdate, _window, cx| {
                if let Some(update_info) = cx.try_global::<notmux_ext_updater::GlobalUpdateInfo>() {
                    let info = update_info.0.clone();
                    if let notmux_ext_updater::UpdateStatus::Ready { version, path } = info.status() {
                        info.set_status(notmux_ext_updater::UpdateStatus::Installing {
                            version: version.clone(),
                        });
                        cx.notify();
                        cx.spawn(async move |_this, cx| {
                            let result = smol::unblock({
                                move || notmux_ext_updater::installer::install_update(&path)
                            })
                            .await;
                            match result {
                                Ok(executable) => {
                                    info.set_status(
                                        notmux_ext_updater::UpdateStatus::ReadyToRestart { version, executable },
                                    );
                                }
                                Err(e) => {
                                    log::error!("Install failed: {}", e);
                                    info.set_status(notmux_ext_updater::UpdateStatus::Failed {
                                        error: e.to_string(),
                                    });
                                }
                            }
                            let _ = _this.update(cx, |_, cx| cx.notify());
                        })
                        .detach();
                    }
                }
            }))
            // Handle toggle pane switcher action
            .on_action(cx.listener(|this, _: &TogglePaneSwitcher, _window, cx| {
                if this.pane_switch_active {
                    this.pane_switch_active = false;
                    this.pane_switcher_entity = None;
                } else {
                    this.pane_switch_active = true;
                    let pane_map = get_pane_map();
                    this.show_pane_switcher(pane_map, cx);
                }
                cx.notify();
            }))
            // Handle create worktree action
            .on_action(cx.listener(|this, _: &CreateWorktree, _window, cx| {
                this.create_worktree_from_focus(cx);
            }))
            // Handle start all services action
            .on_action(cx.listener({
                let workspace = workspace.clone();
                move |this, _: &StartAllServices, _window, cx| {
                    if let Some(ref sm) = this.service_manager {
                        let project_id = workspace
                            .read(cx)
                            .focus_manager
                            .focused_terminal_state()
                            .map(|f| f.project_id.clone());
                        if let Some(pid) = project_id {
                            let path = sm.read(cx).project_path(&pid).cloned();
                            if let Some(path) = path {
                                sm.update(cx, |sm, cx| sm.start_all(&pid, &path, cx));
                            }
                        }
                    }
                }
            }))
            // Handle stop all services action
            .on_action(cx.listener({
                let workspace = workspace.clone();
                move |this, _: &StopAllServices, _window, cx| {
                    if let Some(ref sm) = this.service_manager {
                        let project_id = workspace
                            .read(cx)
                            .focus_manager
                            .focused_terminal_state()
                            .map(|f| f.project_id.clone());
                        if let Some(pid) = project_id {
                            sm.update(cx, |sm, cx| sm.stop_all(&pid, cx));
                        }
                    }
                }
            }))
            // Handle show file search action
            .on_action(cx.listener({
                let workspace = workspace.clone();
                move |this, _: &ShowFileSearch, _window, cx| {
                    let project_id = workspace
                        .read(cx)
                        .focus_manager
                        .focused_terminal_state()
                        .map(|f| f.project_id.clone())
                        .or_else(|| {
                            workspace
                                .read(cx)
                                .visible_projects()
                                .first()
                                .map(|p| p.id.clone())
                        });

                    if let Some(project_id) = project_id {
                        this.request_broker.update(cx, |broker, cx| {
                            broker.push_overlay_request(
                                OverlayRequest::FileSearch { project_id },
                                cx,
                            );
                        });
                    }
                }
            }))
            // Handle show content search action
            .on_action(cx.listener({
                move |this, _: &ShowContentSearch, _window, cx| {
                    if !this.sidebar_ctrl.is_open() && !this.sidebar_ctrl.is_hover_shown() {
                        this.toggle_sidebar(cx);
                    }
                    this.sidebar.update(cx, |sidebar, cx| {
                        sidebar.set_view(notmux_views_sidebar::sidebar::SidebarView::Search, cx);
                    });
                }
            }))
            // Handle show project switcher action
            .on_action(cx.listener({
                let overlay_manager = overlay_manager.clone();
                move |_this, _: &ShowProjectSwitcher, _window, cx| {
                    overlay_manager.update(cx, |om, cx| om.toggle_project_switcher(cx));
                }
            }))
            // Handle show diff viewer action (from keybinding or command palette - no path data)
            .on_action(cx.listener({
                let workspace = workspace.clone();
                move |this, _: &ShowDiffViewer, _window, cx| {
                    // Get the focused or first visible project ID
                    let project_id = workspace
                        .read(cx)
                        .focus_manager
                        .focused_terminal_state()
                        .map(|f| f.project_id.clone())
                        .or_else(|| {
                            workspace
                                .read(cx)
                                .visible_projects()
                                .first()
                                .map(|p| p.id.clone())
                        });

                    if let Some(project_id) = project_id {
                        this.request_broker.update(cx, |broker, cx| {
                            broker.push_overlay_request(
                                OverlayRequest::DiffViewer {
                                    project_id,
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
                }
            }))
            // Title bar is now a transparent absolute overlay (added at the very
            // end), so the main content fills the full height and the tab bars
            // sit at the very top — one floor up — with the controls floating over.
            // Main content area
            .child(
                // Content below title bar
                div()
                    .flex_1()
                    .flex()
                    .min_h_0()
                    .min_w_0()
                    .relative()
                    // Auto-hide hover zone (invisible strip on the left edge)
                    .when(
                        self.sidebar_ctrl.is_auto_hide()
                            && !self.sidebar_ctrl.is_open()
                            && !self.sidebar_ctrl.is_hover_shown(),
                        |d| {
                            d.child(
                                div()
                                    .id("sidebar-hover-zone")
                                    .absolute()
                                    .left_0()
                                    .top_0()
                                    .h_full()
                                    .w(px(8.0))
                                    .hover(|s| s.cursor_pointer())
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _window, cx| {
                                            this.show_sidebar_on_hover(cx);
                                        }),
                                    )
                                    .on_mouse_move(cx.listener(|this, _, _window, cx| {
                                        this.show_sidebar_on_hover(cx);
                                    })),
                            )
                        },
                    )
                    .child(
                        // Sidebar container - animated width
                        {
                            let sidebar_width = self.sidebar_ctrl.current_width();
                            let configured_width = self.sidebar_ctrl.width();
                            let show_sidebar = self.sidebar_ctrl.should_render();

                            div()
                                .id("sidebar-container")
                                .h_full()
                                .w(px(sidebar_width))
                                .overflow_hidden()
                                .flex_shrink_0()
                                .flex()
                                .flex_col()
                                // Reserve the title-bar overlay height so sidebar
                                // content clears the traffic lights + left toggle.
                                // This strip is also a window-drag handle
                                // (full sidebar width)
                                // so the window can be moved from the top-left.
                                // (The Pair button moved into the title-bar
                                // left cluster, next to the bell.)
                                .child(
                                    div()
                                        .h(px(notmux_ui::tokens::TITLE_BAR_STRIP_H))
                                        .w_full()
                                        .flex_shrink_0()
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _, _, _| {
                                                this.title_should_move = true
                                            }),
                                        ),
                                )
                                .when(show_sidebar, |d| {
                                    d.child(
                                        // Inner wrapper to maintain sidebar at full width for clipping effect
                                        div().w(px(configured_width)).flex_1().min_h_0().child(
                                            AnyView::from(self.sidebar.clone())
                                                .cached(StyleRefinement::default().size_full()),
                                        ),
                                    )
                                    // Status bar (system stats) at the sidebar's
                                    // bottom — disabled for now, kept for easy
                                    // restore:
                                    // .child(
                                    //     div()
                                    //         .w(px(configured_width))
                                    //         .flex_shrink_0()
                                    //         .child(self.status_bar.clone()),
                                    // )
                                    // Settings at the sidebar's bottom (moved
                                    // here from the title-bar right cluster).
                                    // Styled exactly like the sidebar's SEARCH
                                    // entry: same row metrics, hover surface,
                                    // and type.
                                    .child({
                                        let st = notmux_ui::theme::sidebar_theme(cx);
                                        div()
                                            .w(px(configured_width))
                                            .flex_shrink_0()
                                            .child(
                                                div()
                                                    .id("sidebar-settings-btn")
                                                    .mx(px(6.0))
                                                    .my(px(6.0))
                                                    .h(px(30.0))
                                                    // px(6)+mx(6) → icon at x=12, aligned with the
                                                    // SEARCH / PROJECTS / REMOTE header icons.
                                                    .px(px(6.0))
                                                    .flex()
                                                    .items_center()
                                                    .gap(px(6.0))
                                                    .cursor_pointer()
                                                    .rounded(px(6.0))
                                                    .hover(move |s| s.bg(rgb(st.bg_hover)))
                                                    .child(
                                                        svg()
                                                            .path("icons/settings-gear.svg")
                                                            .size(px(14.0))
                                                            .text_color(rgb(st.text_secondary))
                                                            .flex_shrink_0(),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_size(ui_text_ms(cx))
                                                            .font_weight(FontWeight::SEMIBOLD)
                                                            .text_color(rgb(st.text_secondary))
                                                            .child("SETTINGS"),
                                                    )
                                                    .on_click(|_, window, cx| {
                                                        window.dispatch_action(
                                                            Box::new(ShowSettings),
                                                            cx,
                                                        );
                                                    }),
                                            )
                                    })
                                })
                        },
                    )
                    // Sidebar resize divider (only when sidebar is visible)
                    .when(self.sidebar_ctrl.should_render(), |d| {
                        d.child(render_sidebar_divider(&self.active_drag, cx))
                    })
                    .child(
                        // Main area
                        div()
                            .id("main-area")
                            .flex_1()
                            .flex()
                            .flex_col()
                            .min_h_0()
                            .min_w_0()
                            .child(
                                // Projects grid (zoom is handled by LayoutContainer)
                                div()
                                    .id("projects-container")
                                    .flex_1()
                                    .min_h_0()
                                    .min_w_0()
                                    // Settings is now a full-window overlay (added
                                    // at the root below), so the main area always
                                    // shows the file viewer / diff viewer / grid.
                                    .child(if let Some(viewer) = main_file_viewer.clone() {
                                        AnyView::from(viewer)
                                            .cached(StyleRefinement::default().size_full())
                                            .into_any_element()
                                    } else if let Some(viewer) = main_diff_viewer.clone() {
                                        viewer
                                            .update(cx, |viewer, cx| {
                                                viewer.render_embedded(window, cx)
                                            })
                                            .into_any_element()
                                    } else {
                                        self.render_projects_grid(cx).into_any_element()
                                    }),
                            ),
                    )
                    // Git panel (right side)
                    .child(self.render_git_panel(cx)),
            )
            // Title-bar controls as two *corner* overlays (top-left + top-right),
            // NOT a full-width bar. The middle over the tab bars is left
            // completely uncovered so tab drag-and-drop reaches the tabs instead
            // of the macOS window-move (a full-width overlay's transparent middle
            // is draggable-by-default and would steal the gesture). Also shown
            // in macOS fullscreen — the panel toggles must stay reachable; only
            // the traffic-light padding collapses (lights auto-hide there).
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .flex()
                    .items_center()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, _| this.title_should_move = true),
                    )
                    .child(
                        self.title_bar
                            .update(cx, |tb, cx| tb.render_left_cluster(window, cx)),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .flex()
                    .items_center()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, _| this.title_should_move = true),
                    )
                    .child(
                        self.title_bar
                            .update(cx, |tb, cx| tb.render_right_cluster(window, cx)),
                    ),
            )
            // Settings panel: a full-window overlay covering sidebar + main area +
            // git panel (not just the middle panel). Popovers opened from within it
            // (color picker, context menus) render above it as they come later.
            .when_some(settings_panel.clone(), |d, panel| {
                d.child(
                    div().absolute().inset_0().child(
                        AnyView::from(panel).cached(StyleRefinement::default().size_full()),
                    ),
                )
            })
            // App menu dropdown (renders on top of everything, not on macOS where native menu is used)
            .when(
                !cfg!(target_os = "macos") && self.title_bar.read(cx).is_menu_open(),
                |d| d.child(self.title_bar.update(cx, |tb, cx| tb.render_menu(cx))),
            )
            // Color picker popover (positioned popup, rendered at root for full-window backdrop)
            .when(has_color_picker, |d| {
                d.children(self.overlay_manager.read(cx).render_color_picker())
            })
            // Worktree list popover (positioned popup, rendered at root for full-window backdrop)
            .when(has_worktree_list, |d| {
                d.children(self.overlay_manager.read(cx).render_worktree_list())
            })
            // Context menu overlay (positioned popup, separate from modals)
            .when(has_context_menu, |d| {
                d.children(self.overlay_manager.read(cx).render_context_menu())
            })
            // Folder context menu overlay (positioned popup, separate from modals)
            .when(has_folder_context_menu, |d| {
                d.children(self.overlay_manager.read(cx).render_folder_context_menu())
            })
            // Remote connection context menu overlay (positioned popup)
            .when(has_remote_context_menu, |d| {
                d.children(self.overlay_manager.read(cx).render_remote_context_menu())
            })
            // Terminal context menu overlay (positioned popup)
            .when(has_terminal_context_menu, |d| {
                d.children(self.overlay_manager.read(cx).render_terminal_context_menu())
            })
            // Git file context menu overlay (positioned popup)
            .when(has_git_file_context_menu, |d| {
                d.children(self.overlay_manager.read(cx).render_git_file_context_menu())
            })
            // Explorer context menu overlay (positioned popup, sidebar file tree)
            .when(has_explorer_context_menu, |d| {
                d.children(self.overlay_manager.read(cx).render_explorer_context_menu())
            })
            // Git overflow menu (three-dots in panel header)
            .when(has_git_overflow_menu, |d| {
                d.children(self.overlay_manager.read(cx).render_git_overflow_menu())
            })
            // Git stash list overlay
            .when(has_git_stash_list, |d| {
                d.children(self.overlay_manager.read(cx).render_git_stash_list())
            })
            // Tab context menu overlay (positioned popup)
            .when(has_tab_context_menu, |d| {
                d.children(self.overlay_manager.read(cx).render_tab_context_menu())
            })
            // Single active modal overlay (renders on top of everything)
            .when_some(self.overlay_manager.read(cx).render_modal(), |d, modal| {
                d.child(modal)
            })
            // Toast notifications (bottom-right, on top of everything)
            .child(self.toast_overlay.clone())
            // Pane switcher overlay (numbered pane badges)
            .when_some(self.pane_switcher_entity.clone(), |d, entity| {
                d.child(entity)
            })
    }
}
