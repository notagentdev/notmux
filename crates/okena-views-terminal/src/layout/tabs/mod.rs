//! Tab bar rendering and management

mod shell_selector;

use crate::actions::Cancel;
use crate::ActionDispatch;
use crate::terminal_view_settings;
use okena_files::theme::theme;
use okena_ui::theme::with_alpha;
use okena_ui::tokens::{ui_text_sm, ui_text_md};
use okena_ui::header_buttons::{header_button_base, ButtonSize, HeaderAction};
use crate::layout::layout_container::{LayoutContainer, is_renaming, rename_input};
use crate::layout::pane_drag::{PaneDrag, PaneDragView};
use crate::simple_input::SimpleInput;
use okena_workspace::state::{LayoutNode, SplitDirection};
use gpui::*;
use gpui_component::{h_flex, v_flex};
use gpui::prelude::*;
use std::collections::HashSet;

/// Alpha-blend `fg` over `bg` and return opaque RGB u32.
fn blend_u32(fg: u32, alpha: u8, bg: u32) -> u32 {
    let a = alpha as u32;
    let inv = 255 - a;
    let r = ((fg >> 16) & 0xff) * a + ((bg >> 16) & 0xff) * inv;
    let g = ((fg >> 8) & 0xff) * a + ((bg >> 8) & 0xff) * inv;
    let b = (fg & 0xff) * a + (bg & 0xff) * inv;
    (((r + 127) / 255) << 16) | (((g + 127) / 255) << 8) | ((b + 127) / 255)
}

/// Derive a tab-bar bg that's always visibly distinct from the terminal
/// content bg. Tints with text_primary at low alpha — lightens dark themes,
/// darkens light ones.
fn tab_bar_bg(term_bg: u32, text_primary: u32) -> u32 {
    blend_u32(text_primary, 0x14, term_bg)
}

/// Context for tab action button closures.
#[derive(Clone)]
pub(super) struct TabActionContext<D: ActionDispatch> {
    pub workspace: Entity<okena_workspace::state::Workspace>,
    pub project_id: String,
    pub layout_path: Vec<usize>,
    pub active_tab: usize,
    pub standalone: bool,
    pub action_dispatcher: Option<D>,
}


impl<D: ActionDispatch + Send + Sync> LayoutContainer<D> {
    pub(super) fn start_drop_animation(&mut self, tab_index: usize, cx: &mut Context<Self>) {
        self.drop_animation = Some((tab_index, 1.0));
        cx.notify();

        cx.spawn(async move |this: WeakEntity<LayoutContainer<D>>, cx| {
            let duration_ms = 200;
            let frame_time_ms = 33;
            let steps = duration_ms / frame_time_ms;
            let step_duration = std::time::Duration::from_millis(frame_time_ms as u64);

            for i in 1..=steps {
                smol::Timer::after(step_duration).await;

                let t = i as f32 / steps as f32;
                let progress = 1.0 - t * t;

                let result = this.update(cx, |this, cx| {
                    if let Some((idx, _)) = this.drop_animation {
                        this.drop_animation = Some((idx, progress));
                        cx.notify();
                    }
                });
                if result.is_err() {
                    break;
                }
            }

            let _ = this.update(cx, |this, cx| {
                this.drop_animation = None;
                cx.notify();
            });
        }).detach();
    }

    pub(super) fn render_tab_action_buttons(
        &self,
        ctx: TabActionContext<D>,
        terminal_id: Option<String>,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = theme(cx);
        let id_suffix = format!("tabs-{:?}", ctx.layout_path);

        let supports_buffer_capture = self.backend.supports_buffer_capture();
        let backend_for_export = self.backend.clone();
        let terminal_id_for_export = terminal_id.clone();
        let terminal_id_for_fullscreen = terminal_id.clone();

        let ctx_split_v = ctx.clone();
        let ctx_split_h = ctx.clone();
        let ctx_add_tab = ctx.clone();
        let ctx_minimize = ctx.clone();
        let ctx_fullscreen = ctx.clone();
        let ctx_detach = ctx.clone();

        let _ = ctx;

        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.0))
            .px(px(4.0))
            .child(
                header_button_base(HeaderAction::SplitVertical, &id_suffix, ButtonSize::COMPACT, &t, None, None)
                    .on_click(move |_, _window, cx| {
                        if let Some(ref dispatcher) = ctx_split_v.action_dispatcher {
                            dispatcher.dispatch(okena_core::api::ActionRequest::SplitTerminal {
                                project_id: ctx_split_v.project_id.clone(),
                                path: ctx_split_v.layout_path.clone(),
                                direction: SplitDirection::Vertical,
                            }, cx);
                        }
                    }),
            )
            .child(
                header_button_base(HeaderAction::SplitHorizontal, &id_suffix, ButtonSize::COMPACT, &t, None, None)
                    .on_click(move |_, _window, cx| {
                        if let Some(ref dispatcher) = ctx_split_h.action_dispatcher {
                            dispatcher.dispatch(okena_core::api::ActionRequest::SplitTerminal {
                                project_id: ctx_split_h.project_id.clone(),
                                path: ctx_split_h.layout_path.clone(),
                                direction: SplitDirection::Horizontal,
                            }, cx);
                        }
                    }),
            )
            .child(
                header_button_base(HeaderAction::AddTab, &id_suffix, ButtonSize::COMPACT, &t, None, None)
                    .on_click(move |_, _window, cx| {
                        if let Some(ref dispatcher) = ctx_add_tab.action_dispatcher {
                            dispatcher.add_tab(
                                &ctx_add_tab.project_id,
                                &ctx_add_tab.layout_path,
                                !ctx_add_tab.standalone,
                                cx,
                            );
                        }
                    }),
            )
            .child(
                header_button_base(HeaderAction::Minimize, &id_suffix, ButtonSize::COMPACT, &t, None, None)
                    .on_click({
                        let terminal_id_for_minimize = terminal_id.clone();
                        move |_, _window, cx| {
                            if let Some(ref tid) = terminal_id_for_minimize
                                && let Some(ref dispatcher) = ctx_minimize.action_dispatcher {
                                    dispatcher.dispatch(okena_core::api::ActionRequest::ToggleMinimized {
                                        project_id: ctx_minimize.project_id.clone(),
                                        terminal_id: tid.clone(),
                                    }, cx);
                                }
                        }
                    }),
            )
            .when(supports_buffer_capture, |el| {
                el.child(
                    header_button_base(HeaderAction::ExportBuffer, &id_suffix, ButtonSize::COMPACT, &t, None, None)
                        .on_click(move |_, _window, cx| {
                            if let Some(ref tid) = terminal_id_for_export
                                && let Some(path) = backend_for_export.capture_buffer(tid) {
                                    cx.write_to_clipboard(ClipboardItem::new_string(path.display().to_string()));
                                    log::info!("Buffer exported to {} (path copied to clipboard)", path.display());
                                }
                        }),
                )
            })
            .child(
                header_button_base(HeaderAction::Fullscreen, &id_suffix, ButtonSize::COMPACT, &t, None, None)
                    .on_click(move |_, _window, cx| {
                        if let Some(ref tid) = terminal_id_for_fullscreen
                            && let Some(ref dispatcher) = ctx_fullscreen.action_dispatcher {
                                dispatcher.dispatch(okena_core::api::ActionRequest::SetFullscreen {
                                    project_id: ctx_fullscreen.project_id.clone(),
                                    terminal_id: Some(tid.clone()),
                                }, cx);
                            }
                    }),
            )
            .child(
                header_button_base(HeaderAction::Detach, &id_suffix, ButtonSize::COMPACT, &t, None, None)
                    .on_click(move |_, _window, cx| {
                        let full_path = if ctx_detach.standalone {
                            ctx_detach.layout_path.clone()
                        } else {
                            let mut p = ctx_detach.layout_path.clone();
                            p.push(ctx_detach.active_tab);
                            p
                        };
                        ctx_detach.workspace.update(cx, |ws, cx| {
                            ws.detach_terminal(&ctx_detach.project_id, &full_path, cx);
                        });
                    }),
            )
    }

    pub(super) fn render_tabs(
        &mut self,
        children: &[LayoutNode],
        active_tab: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        if let Some(zoomed_idx) = self.find_zoomed_child_index(children, cx) {
            let mut child_path = self.layout_path.clone();
            child_path.push(zoomed_idx);

            let container = self
                .child_containers
                .entry(child_path.clone())
                .or_insert_with(|| {
                    cx.new(|_cx| {
                        LayoutContainer::new(
                            self.workspace.clone(),
                            self.request_broker.clone(),
                            self.project_id.clone(),
                            self.project_path.clone(),
                            child_path.clone(),
                            self.backend.clone(),
                            self.terminals.clone(),
                            self.active_drag.clone(),
                            self.action_dispatcher.clone(),
                        )
                    })
                })
                .clone();

            return v_flex()
                .size_full()
                .child(AnyView::from(container).cached(
                    StyleRefinement::default().size_full()
                ));
        }

        let num_children = children.len();
        let valid_paths: HashSet<Vec<usize>> = (0..num_children)
            .map(|i| {
                let mut path = self.layout_path.clone();
                path.push(i);
                path
            })
            .collect();
        self.child_containers.retain(|path, _| valid_paths.contains(path));

        let container_bounds_ref = self.container_bounds_ref.clone();

        v_flex()
            .size_full()
            .relative()
            .child(canvas(
                {
                    let container_bounds_ref = container_bounds_ref.clone();
                    move |bounds, _window, _cx| {
                        *container_bounds_ref.borrow_mut() = bounds;
                    }
                },
                |_bounds, _prepaint, _window, _cx| {},
            ).absolute().size_full())
            .child(self.render_tab_bar(children, active_tab, false, cx))
            .child(
                div().flex_1().child({
                    let mut child_path = self.layout_path.clone();
                    child_path.push(active_tab);

                    let container = self.child_containers
                        .entry(child_path.clone())
                        .or_insert_with(|| {
                            cx.new(|_cx| {
                                LayoutContainer::new(
                                    self.workspace.clone(),
                                    self.request_broker.clone(),
                                    self.project_id.clone(),
                                    self.project_path.clone(),
                                    child_path.clone(),
                                    self.backend.clone(),
                                    self.terminals.clone(),
                                    self.active_drag.clone(),
                                    self.action_dispatcher.clone(),
                                )
                            })
                        })
                        .clone();

                    AnyView::from(container).cached(StyleRefinement::default().size_full())
                }),
            )
    }

    pub(super) fn render_standalone_tab_bar(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let node = {
            let ws = self.workspace.read(cx);
            self.get_layout(ws).cloned()
        };

        let children: &[LayoutNode] = match node {
            Some(ref n @ LayoutNode::Terminal { .. }) => std::slice::from_ref(n),
            _ => &[],
        };

        self.render_tab_bar(children, 0, true, cx)
    }

    fn render_tab_bar(
        &mut self,
        children: &[LayoutNode],
        active_tab: usize,
        standalone: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = theme(cx);
        let workspace = self.workspace.clone();
        let project_id = self.project_id.clone();
        let layout_path = self.layout_path.clone();
        let num_children = children.len();

        let drop_animation = self.drop_animation;

        let terminals = self.terminals.clone();
        let workspace_reader = self.workspace.read(cx);
        let project = workspace_reader.project(&self.project_id);
        let project_for_names = project.cloned();

        let is_pane_focused = workspace_reader.focus_manager
            .focused_terminal_state()
            .is_some_and(|f| {
                f.project_id == self.project_id
                    && f.layout_path.starts_with(&self.layout_path)
            });

        let tab_elements: Vec<_> = children.iter().enumerate().map(|(i, child)| {
            let is_active = i == active_tab;
            let workspace = workspace.clone();
            let project_id = project_id.clone();
            let project_id_for_drag = project_id.clone();
            let project_id_for_drop = project_id.clone();
            let layout_path = layout_path.clone();
            let layout_path_for_drag = layout_path.clone();
            let layout_path_for_drop = layout_path.clone();

            let terminal_id = match child {
                LayoutNode::Terminal { terminal_id: Some(id), .. } => Some(id.clone()),
                _ => None,
            };

            let (is_waiting, idle_label) = terminal_id.as_ref().map_or((false, None), |tid| {
                let guard = terminals.lock();
                guard.get(tid).map_or((false, None), |t| {
                    if t.is_waiting_for_input() {
                        (true, Some(t.idle_duration_display()))
                    } else {
                        (false, None)
                    }
                })
            });

            let is_hook = terminal_id.as_ref().is_some_and(|tid| {
                project_for_names.as_ref().is_some_and(|p| p.hook_terminals.contains_key(tid))
            });

            let tab_label = if let Some(ref tid) = terminal_id {
                if let Some(ref p) = project_for_names {
                    let osc_title = terminals.lock().get(tid).and_then(|t| t.title());
                    p.terminal_display_name(tid, osc_title)
                } else {
                    format!("Tab {}", i + 1)
                }
            } else {
                format!("Tab {}", i + 1)
            };

            let has_drop_animation = drop_animation.map(|(idx, _)| idx == i).unwrap_or(false);
            let animation_progress = drop_animation
                .filter(|(idx, _)| *idx == i)
                .map(|(_, p)| p)
                .unwrap_or(0.0);

            let tab_group = SharedString::from(format!("term-tab-{}-{:?}", i, layout_path));
            div()
                .id(ElementId::Name(format!("tab-{}-{:?}", i, layout_path).into()))
                .group(tab_group.clone())
                .cursor_pointer()
                .relative()
                .flex_shrink_0()
                .max_w(px(200.0))
                .h(px(32.0))
                .overflow_hidden()
                .border_r_1()
                .border_color(rgb(t.border))
                .text_size(ui_text_md(cx))
                .when(is_active && is_pane_focused, |d| {
                    d.bg(rgb(t.term_background))
                        .text_color(rgb(t.text_primary))
                })
                .when(is_active && !is_pane_focused, |d| {
                    d.bg(rgb(t.term_background_unfocused))
                        .text_color(rgb(t.text_primary))
                })
                .when(!is_active, |d| {
                    d.text_color(rgb(t.text_secondary))
                        .hover(|s| s.bg(rgb(t.bg_hover)))
                })
                .when(has_drop_animation, |d| {
                    let glow_alpha = animation_progress * 0.5;
                    d.bg(with_alpha(t.border_active, glow_alpha))
                        .border_1()
                        .border_color(with_alpha(t.border_active, animation_progress * 0.9))
                        .rounded(px(4.0))
                })
                .child({
                    let is_renaming_this = terminal_id.as_ref().is_some_and(|tid| {
                        is_renaming(&self.tab_rename_state, tid)
                    });
                    if let Some(input) = is_renaming_this.then(|| rename_input(&self.tab_rename_state)).flatten() {
                        div()
                            .id(format!("tab-rename-{}", i))
                            .key_context("TerminalRename")
                            .flex_1()
                            .min_w(px(80.0))
                            .bg(rgb(t.bg_secondary))
                            .border_1()
                            .border_color(rgb(t.border_active))
                            .rounded(px(4.0))
                            .child(SimpleInput::new(input).text_size(ui_text_md(cx)))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                cx.stop_propagation();
                            })
                            .on_click(|_, _window, cx| {
                                cx.stop_propagation();
                            })
                            .on_action(cx.listener(|this, _: &Cancel, _window, cx| {
                                this.cancel_tab_rename(cx);
                            }))
                            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                                cx.stop_propagation();
                                if event.keystroke.key.as_str() == "enter" {
                                    this.finish_tab_rename(cx);
                                }
                            }))
                            .into_any_element()
                    } else {
                        let icon_color = if is_hook { rgb(t.term_yellow) } else if is_waiting { rgb(t.border_idle) } else if is_active { rgb(t.success) } else { rgb(t.text_muted) };
                        let close_id = ElementId::Name(format!("tab-close-{}-{:?}", i, layout_path).into());
                        let close_terminal_id = terminal_id.clone();
                        let close_project_id = project_id.clone();
                        let close_dispatcher = self.action_dispatcher.clone();

                        // Start slot: terminal icon
                        let start_slot = h_flex()
                            .w(px(12.0))
                            .h(px(12.0))
                            .justify_center()
                            .child(
                                svg()
                                    .path("icons/terminal.svg")
                                    .size(px(12.0))
                                    .text_color(icon_color),
                            );

                        // End slot: close button (Zed-style: muted icon, subtle hover bg)
                        let bg_hover = t.bg_hover;
                        let muted = t.text_muted;
                        let end_slot = h_flex()
                            .flex_none()
                            .justify_center()
                            .mt(px(2.0))
                            .when_some(close_terminal_id, |slot, tid| {
                                slot.child(
                                    h_flex()
                                        .id(close_id)
                                        .flex_none()
                                        .w(px(24.0))
                                        .h(px(24.0))
                                        .justify_center()
                                        .rounded(px(4.0))
                                        .cursor_pointer()
                                        .hover(move |s| s.bg(rgb(bg_hover)))
                                        .child(
                                            svg()
                                                .path("icons/close.svg")
                                                .size(px(16.0))
                                                .text_color(rgb(muted)),
                                        )
                                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                            cx.stop_propagation();
                                        })
                                        .on_click(move |_, _window, cx| {
                                            cx.stop_propagation();
                                            if let Some(ref dispatcher) = close_dispatcher {
                                                dispatcher.dispatch(
                                                    okena_core::api::ActionRequest::CloseTerminal {
                                                        project_id: close_project_id.clone(),
                                                        terminal_id: tid.clone(),
                                                    },
                                                    cx,
                                                );
                                            }
                                        }),
                                )
                            });

                        // Children: the label
                        let label = div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(tab_label.clone());

                        h_flex()
                            .h(px(32.0))
                            .w_full()
                            .px(px(8.0))
                            .gap(px(6.0))
                            .overflow_hidden()
                            .child(start_slot)
                            .child(label)
                            .children(idle_label.as_ref().map(|d| {
                                div()
                                    .flex_shrink_0()
                                    .text_size(ui_text_sm(cx))
                                    .text_color(rgb(t.border_idle))
                                    .child(d.clone())
                            }))
                            .child(end_slot)
                            .into_any_element()
                    }
                })
                .on_mouse_down(MouseButton::Right, {
                    let project_id = project_id.clone();
                    let layout_path = layout_path.clone();
                    cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                        this.request_broker.update(cx, |broker, cx| {
                            broker.push_overlay_request(
                                okena_workspace::requests::OverlayRequest::TabContextMenu {
                                    tab_index: i,
                                    num_tabs: num_children,
                                    project_id: project_id.clone(),
                                    layout_path: layout_path.clone(),
                                    position: event.position,
                                },
                                cx,
                            );
                        });
                        cx.stop_propagation();
                    })
                })
                .on_mouse_down(MouseButton::Middle, {
                    let project_id = project_id.clone();
                    let terminal_id = terminal_id.clone();
                    let action_dispatcher = self.action_dispatcher.clone();
                    cx.listener(move |_this, _event: &MouseDownEvent, _window, cx| {
                        if let Some(ref tid) = terminal_id
                            && let Some(ref dispatcher) = action_dispatcher {
                                dispatcher.dispatch(okena_core::api::ActionRequest::CloseTerminal {
                                    project_id: project_id.clone(),
                                    terminal_id: tid.clone(),
                                }, cx);
                            }
                        cx.stop_propagation();
                    })
                })
                .when_some(terminal_id.clone(), |el, tid| {
                    let terminal_path = if standalone {
                        layout_path_for_drag.clone()
                    } else {
                        let mut p = layout_path_for_drag.clone();
                        p.push(i);
                        p
                    };
                    el.on_drag(
                        PaneDrag {
                            project_id: project_id_for_drag.clone(),
                            layout_path: terminal_path,
                            terminal_id: tid,
                            terminal_name: tab_label.clone(),
                        },
                        move |drag, _position, _window, cx| {
                            cx.new(|_| PaneDragView::new(drag.terminal_name.clone()))
                        },
                    )
                })
                .when(!standalone, |el| {
                    el.drag_over::<PaneDrag>({
                        let active_drag = self.active_drag.clone();
                        move |style, _, _, _| {
                            if active_drag.borrow().is_some() {
                                return style;
                            }
                            style
                                .border_l(px(3.0))
                                .border_color(rgb(t.border_active))
                                .bg(with_alpha(t.border_active, 0.15))
                        }
                    })
                    .on_drop(cx.listener({
                        let active_drag = self.active_drag.clone();
                        let dispatcher_for_drop = self.action_dispatcher.clone();
                        move |this, drag: &PaneDrag, _window, cx| {
                            if active_drag.borrow().is_some() {
                                return;
                            }

                            let drag_parent = &drag.layout_path[..drag.layout_path.len().saturating_sub(1)];
                            let drag_tab_index = drag.layout_path.last().copied();

                            if drag.project_id == project_id_for_drop
                                && drag_parent == layout_path_for_drop.as_slice()
                            {
                                if let Some(from_index) = drag_tab_index
                                    && from_index != i {
                                        let target_index = if from_index < i { i - 1 } else { i };
                                        if let Some(ref dispatcher) = dispatcher_for_drop {
                                            dispatcher.dispatch(okena_core::api::ActionRequest::MoveTab {
                                                project_id: project_id_for_drop.clone(),
                                                path: layout_path_for_drop.clone(),
                                                from_index,
                                                to_index: i,
                                            }, cx);
                                        }
                                        this.start_drop_animation(target_index, cx);
                                    }
                            } else if let Some(ref dispatcher) = dispatcher_for_drop {
                                dispatcher.dispatch(okena_core::api::ActionRequest::MoveTerminalToTabGroup {
                                    project_id: drag.project_id.clone(),
                                    terminal_id: drag.terminal_id.clone(),
                                    target_path: layout_path_for_drop.clone(),
                                    position: Some(i),
                                    target_project_id: Some(project_id_for_drop.clone()),
                                }, cx);
                            }
                        }
                    }))
                })
                .on_click({
                    let workspace = workspace.clone();
                    let project_id = project_id.clone();
                    let layout_path = layout_path.clone();
                    let terminal_id = terminal_id.clone();
                    let tab_label = tab_label.clone();
                    let dispatcher_for_click = self.action_dispatcher.clone();
                    cx.listener(move |this, _, window, cx| {
                        let is_double_click = this.tab_click_detector.check(i);

                        if this.tab_rename_state.is_some() && !is_double_click {
                            let is_renaming_this = terminal_id.as_ref().is_some_and(|tid| {
                                is_renaming(&this.tab_rename_state, tid)
                            });
                            if !is_renaming_this {
                                this.cancel_tab_rename(cx);
                            }
                        }

                        if !standalone
                            && let Some(ref dispatcher) = dispatcher_for_click {
                                dispatcher.dispatch(okena_core::api::ActionRequest::SetActiveTab {
                                    project_id: project_id.clone(),
                                    path: layout_path.clone(),
                                    index: i,
                                }, cx);
                            }

                        if terminal_id.is_some() {
                            let terminal_path = if standalone {
                                layout_path.clone()
                            } else {
                                let mut p = layout_path.clone();
                                p.push(i);
                                p
                            };
                            workspace.update(cx, |ws, cx| {
                                ws.set_focused_terminal(project_id.clone(), terminal_path, cx);
                            });
                        }

                        if is_double_click
                            && let Some(ref tid) = terminal_id {
                                this.start_tab_rename(tid.clone(), tab_label.clone(), window, cx);
                            }
                    })
                })
        }).collect();

        let project_id_for_new = self.project_id.clone();
        let layout_path_for_new = self.layout_path.clone();
        let dispatcher_for_new = self.action_dispatcher.clone();

        let mut end_drop_zone = div()
            .id(ElementId::Name(format!("tab-end-drop-{:?}", self.layout_path).into()))
            .flex_1()
            .flex_shrink_0()
            .h_full()
            .min_w(px(20.0))
            .on_click(cx.listener(move |this, _, _window, cx| {
                if this.empty_area_click_detector.check(())
                    && let Some(ref dispatcher) = dispatcher_for_new {
                        dispatcher.add_tab(
                            &project_id_for_new,
                            &layout_path_for_new,
                            !standalone,
                            cx,
                        );
                    }
            }));

        if !standalone {
            let active_drag_for_end_hover = self.active_drag.clone();
            let active_drag_for_end_drop = self.active_drag.clone();
            let project_id_for_end = self.project_id.clone();
            let layout_path_for_end = self.layout_path.clone();
            let dispatcher_for_end = self.action_dispatcher.clone();

            end_drop_zone = end_drop_zone
                .drag_over::<PaneDrag>(move |style, _, _, _| {
                    if active_drag_for_end_hover.borrow().is_some() {
                        return style;
                    }
                    style
                        .border_l(px(3.0))
                        .border_color(rgb(t.border_active))
                        .bg(with_alpha(t.border_active, 0.1))
                })
                .on_drop(cx.listener(move |this, drag: &PaneDrag, _window, cx| {
                    if active_drag_for_end_drop.borrow().is_some() {
                        return;
                    }

                    let drag_parent = &drag.layout_path[..drag.layout_path.len().saturating_sub(1)];
                    let drag_tab_index = drag.layout_path.last().copied();

                    if drag.project_id == project_id_for_end
                        && drag_parent == layout_path_for_end.as_slice()
                    {
                        if let Some(from_index) = drag_tab_index {
                            let target_index = num_children;
                            if from_index != target_index - 1 {
                                if let Some(ref dispatcher) = dispatcher_for_end {
                                    dispatcher.dispatch(okena_core::api::ActionRequest::MoveTab {
                                        project_id: project_id_for_end.clone(),
                                        path: layout_path_for_end.clone(),
                                        from_index,
                                        to_index: target_index,
                                    }, cx);
                                }
                                this.start_drop_animation(num_children - 1, cx);
                            }
                        }
                    } else if let Some(ref dispatcher) = dispatcher_for_end {
                        dispatcher.dispatch(okena_core::api::ActionRequest::MoveTerminalToTabGroup {
                            project_id: drag.project_id.clone(),
                            terminal_id: drag.terminal_id.clone(),
                            target_path: layout_path_for_end.clone(),
                            position: None,
                            target_project_id: Some(project_id_for_end.clone()),
                        }, cx);
                    }
                }));
        }

        let action_ctx = TabActionContext {
            workspace: self.workspace.clone(),
            project_id: self.project_id.clone(),
            layout_path: self.layout_path.clone(),
            active_tab,
            standalone,
            action_dispatcher: self.action_dispatcher.clone(),
        };

        let terminal_id_for_actions = if standalone {
            match children.first() {
                Some(LayoutNode::Terminal { terminal_id, .. }) => terminal_id.clone(),
                _ => None,
            }
        } else {
            self.get_active_terminal_id(active_tab, cx)
        };

        let action_buttons = self.render_tab_action_buttons(action_ctx, terminal_id_for_actions.clone(), cx);

        let show_shell = terminal_view_settings(cx).show_shell_selector && !self.backend.is_remote();

        if self.last_scrolled_to_tab != Some(active_tab) {
            self.tab_scroll_handle.scroll_to_item(active_tab);
            self.last_scrolled_to_tab = Some(active_tab);
        }

        let focus_marker_color = t.border_active;

        div()
            .group("tab-bar-row")
            .flex_shrink_0()
            .px(px(0.0))
            .flex()
            .items_stretch()
            .gap(px(0.0))
            .relative()
            .border_t_1()
            .border_b_1()
            .border_color(rgb(t.border))
            .bg(rgb(tab_bar_bg(t.term_background, t.text_primary)))
            .child(
                div()
                    .id(ElementId::Name(format!("tab-scroll-{:?}", self.layout_path).into()))
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .overflow_x_scroll()
                    .track_scroll(&self.tab_scroll_handle)
                    .children(tab_elements)
                    .child(end_drop_zone),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .items_center()
                    .opacity(0.0)
                    .group_hover("tab-bar-row", |s| s.opacity(1.0))
                    .when(show_shell, |el| {
                        el.child(self.render_shell_indicator(active_tab, cx))
                    })
                    .child(action_buttons),
            )
            .when(is_pane_focused, |el| {
                el.child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .w(px(FOCUS_TRIANGLE_SIZE))
                        .h(px(FOCUS_TRIANGLE_SIZE))
                        .group_hover("tab-bar-row", |s| s.opacity(0.0))
                        .child(canvas(
                            |_bounds, _window, _cx| {},
                            move |bounds, _state, window, _cx| {
                                let tr = point(
                                    bounds.origin.x + bounds.size.width,
                                    bounds.origin.y,
                                );
                                let br = point(
                                    bounds.origin.x + bounds.size.width,
                                    bounds.origin.y + bounds.size.height,
                                );
                                let tl = bounds.origin;
                                let mut path = Path::new(tr);
                                path.line_to(br);
                                path.line_to(tl);
                                window.paint_path(path, rgb(focus_marker_color));
                            },
                        ).size_full()),
                )
            })
    }
}

const FOCUS_TRIANGLE_SIZE: f32 = 24.0;
