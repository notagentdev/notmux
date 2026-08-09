use crate::settings::settings_entity;
use crate::theme::right_panel_theme as theme;
use crate::ui::tokens::ui_text_md;
use crate::views::layout::split_pane::render_git_panel_divider;
use crate::views::sidebar_controller::{AnimationTarget, FRAME_TIME_MS, SidebarController};
use gpui::prelude::FluentBuilder;
use gpui::*;

use super::RootView;

/// A right-panel header action, styled exactly like the Git/Files tabs
/// (icon + label pill) but dispatching an action instead of switching views.
fn render_action_pill(
    id: &'static str,
    icon: &'static str,
    label: &'static str,
    t: &notmux_ui::theme::ThemeColors,
    cx: &mut Context<RootView>,
    on_click: impl Fn(&mut RootView, &mut Context<RootView>) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .px(px(10.0))
        .h(px(26.0))
        .rounded_md()
        .cursor_pointer()
        .hover(|s| s.bg(rgb(t.bg_hover)))
        .child(
            svg()
                .path(icon)
                .size(px(13.0))
                .text_color(rgb(t.text_muted)),
        )
        .child(
            div()
                .text_size(ui_text_md(cx))
                .text_color(rgb(t.text_muted))
                .child(label),
        )
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |this, _, _window, cx| {
            cx.stop_propagation();
            on_click(this, cx);
        }))
}

impl RootView {
    /// Project targeted by the right-panel header buttons: the project the
    /// panel is open for, else the focused one, else the first project.
    fn active_project_id(&self, cx: &App) -> Option<String> {
        if let Some(ref pid) = self.git_panel_project_id {
            return Some(pid.clone());
        }
        let ws = self.workspace.read(cx);
        ws.focus_manager
            .focused_terminal_state()
            .map(|state| state.project_id)
            .or_else(|| ws.focus_manager.focused_project_id().cloned())
            .or_else(|| ws.data.projects.first().map(|p| p.id.clone()))
    }

    /// Toggle the git panel for the given project.
    ///
    /// - If the panel is open for this project, close it.
    /// - If it's closed or showing a different project, open for this project.
    pub(super) fn toggle_git_panel(&mut self, project_id: &str, cx: &mut Context<Self>) {
        if self.git_panel_ctrl.is_open() && self.git_panel_project_id.as_deref() == Some(project_id)
        {
            // Close the panel
            let target = self.git_panel_ctrl.toggle();
            settings_entity(cx).update(cx, |s, cx| s.set_git_panel_open(false, cx));
            self.title_bar
                .update(cx, |tb, cx| tb.set_git_panel_open(false, cx));
            self.animate_git_panel_to(target, cx);

            // Close the commit log in the git header
            if let Some(col) = self.project_columns.get(project_id).cloned() {
                let gh = col.read(cx).git_header();
                gh.update(cx, |gh, cx| gh.hide_commit_log(cx));
            }
        } else {
            // Close commit log on previously active project (if any)
            if let Some(old_pid) = self.git_panel_project_id.take()
                && let Some(col) = self.project_columns.get(&old_pid).cloned()
            {
                let gh = col.read(cx).git_header();
                gh.update(cx, |gh, cx| gh.hide_commit_log(cx));
            }

            // Set the new project
            self.git_panel_project_id = Some(project_id.to_string());

            // Open commit log on the new project's git header
            if let Some(col) = self.project_columns.get(project_id).cloned() {
                let gh = col.read(cx).git_header();
                gh.update(cx, |gh, cx| gh.open_commit_log(cx));
            }

            // Open the panel (if not already open)
            if !self.git_panel_ctrl.is_open() {
                let target = self.git_panel_ctrl.toggle();
                settings_entity(cx).update(cx, |s, cx| s.set_git_panel_open(true, cx));
                self.title_bar
                    .update(cx, |tb, cx| tb.set_git_panel_open(true, cx));
                self.animate_git_panel_to(target, cx);
            } else {
                self.title_bar
                    .update(cx, |tb, cx| tb.set_git_panel_open(true, cx));
                cx.notify();
            }
        }
    }

    /// Open the tabbed right panel to a specific view (opening it if closed).
    pub(super) fn open_right_panel(&mut self, view: super::RightView, cx: &mut Context<Self>) {
        self.right_view = view;
        // The Git tab needs a bound project + an open commit log.
        if view == super::RightView::Git {
            let pid = self
                .workspace
                .read(cx)
                .focus_manager
                .focused_terminal_state()
                .map(|f| f.project_id.clone())
                .or_else(|| {
                    self.workspace
                        .read(cx)
                        .visible_projects()
                        .first()
                        .map(|p| p.id.clone())
                });
            if let Some(pid) = pid {
                self.git_panel_project_id = Some(pid.clone());
                if let Some(col) = self.project_columns.get(&pid).cloned() {
                    let gh = col.read(cx).git_header();
                    gh.update(cx, |gh, cx| gh.open_commit_log(cx));
                }
            }
        }
        if !self.git_panel_ctrl.is_open() {
            let target = self.git_panel_ctrl.toggle();
            settings_entity(cx).update(cx, |s, cx| s.set_git_panel_open(true, cx));
            self.title_bar
                .update(cx, |tb, cx| tb.set_git_panel_open(true, cx));
            self.animate_git_panel_to(target, cx);
        } else {
            cx.notify();
        }
    }

    /// Close the tabbed right panel.
    pub(super) fn close_right_panel(&mut self, cx: &mut Context<Self>) {
        if self.git_panel_ctrl.is_open() {
            let target = self.git_panel_ctrl.toggle();
            settings_entity(cx).update(cx, |s, cx| s.set_git_panel_open(false, cx));
            self.title_bar
                .update(cx, |tb, cx| tb.set_git_panel_open(false, cx));
            self.animate_git_panel_to(target, cx);
        }
    }

    /// Toggle a right-panel tab: open+activate it, or close if it's already the
    /// active view.
    pub(super) fn toggle_right_panel(&mut self, view: super::RightView, cx: &mut Context<Self>) {
        if self.git_panel_ctrl.is_open() && self.right_view == view {
            self.close_right_panel(cx);
        } else {
            self.open_right_panel(view, cx);
        }
    }

    /// Render the git panel content.
    pub(super) fn render_git_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let show_panel = self.git_panel_ctrl.should_render();

        // The bound project may have been deleted — drop the binding so the
        // block below adopts the (re)focused project instead.
        if show_panel
            && self
                .git_panel_project_id
                .as_ref()
                .is_some_and(|pid| self.workspace.read(cx).project(pid).is_none())
        {
            self.git_panel_project_id = None;
        }

        // If the panel was restored open from settings but no project is
        // bound yet (fresh session), adopt the focused project, or fall back
        // to the first visible one, so content actually appears.
        if show_panel && self.git_panel_project_id.is_none() {
            let workspace = self.workspace.read(cx);
            let candidate = workspace
                .focus_manager
                .focused_project_id()
                .cloned()
                .or_else(|| workspace.visible_projects().first().map(|p| p.id.clone()));
            if let Some(pid) = candidate
                && self.project_columns.contains_key(&pid)
            {
                self.git_panel_project_id = Some(pid.clone());
                if let Some(col) = self.project_columns.get(&pid).cloned() {
                    let gh = col.read(cx).git_header();
                    gh.update(cx, |gh, cx| {
                        gh.open_commit_log(cx);
                        gh.refresh_working_tree_status(cx);
                    });
                }
            }
        }

        // Panel closed -> take zero space, no divider.
        if !show_panel {
            return div()
                .id("git-panel-wrapper")
                .w(px(0.0))
                .h_full()
                .flex_shrink_0()
                .into_any_element();
        }

        // Pinned view: the panel follows the focused project.
        let pinned_view_active = self.workspace.read(cx).data.pinned_view_active;
        if pinned_view_active {
            let focused_pid = self
                .workspace
                .read(cx)
                .focus_manager
                .focused_terminal_state()
                .map(|f| f.project_id);
            if let Some(pid) = focused_pid
                && self.git_panel_project_id.as_ref() != Some(&pid)
                && self.project_columns.contains_key(&pid)
            {
                self.git_panel_project_id = Some(pid.clone());
                if let Some(col) = self.project_columns.get(&pid).cloned() {
                    let gh = col.read(cx).git_header();
                    gh.update(cx, |gh, cx| {
                        gh.open_commit_log(cx);
                        gh.refresh_working_tree_status(cx);
                    });
                }
            }
        }

        let has_content = self
            .git_panel_project_id
            .as_ref()
            .and_then(|pid| self.project_columns.get(pid))
            .is_some();

        let git_panel_width = self.git_panel_ctrl.current_width();
        let configured_width = self.git_panel_ctrl.width();
        let t = theme(cx);

        let git_content: AnyElement = if has_content {
            let pid = self
                .git_panel_project_id
                .clone()
                .expect("has_content guard verified Some");
            let col = self
                .project_columns
                .get(&pid)
                .cloned()
                .expect("has_content guard verified column exists");
            let gh = col.read(cx).git_header();
            gh.update(cx, |gh, cx| gh.render_commit_log_panel(&t, cx))
                .into_any_element()
        } else {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(ui_text_md(cx))
                .text_color(rgb(t.text_muted))
                .child("No project selected")
                .into_any_element()
        };

        // Body switches on the active right-panel tab.
        let body: AnyElement = match self.right_view {
            super::RightView::Git => git_content,
            super::RightView::Files => self.render_right_files_tab(cx),
        };

        // One tab (icon + label) in the right-panel tab bar.
        let render_tab = |this: &Self,
                          view: super::RightView,
                          icon: &'static str,
                          label: &'static str,
                          cx: &mut Context<Self>| {
            let is_active = this.right_view == view;
            let fg = if is_active { t.text_primary } else { t.text_muted };
            div()
                .id(label)
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .px(px(10.0))
                .h(px(26.0))
                .rounded_md()
                .cursor_pointer()
                .when(is_active, |d| d.bg(rgb(t.bg_hover)))
                .when(!is_active, |d| d.hover(|s| s.bg(rgb(t.bg_hover))))
                .child(svg().path(icon).size(px(13.0)).text_color(rgb(fg)))
                .child(
                    div()
                        .text_size(ui_text_md(cx))
                        .text_color(rgb(fg))
                        .child(label),
                )
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.right_view = view;
                    cx.notify();
                }))
        };

        // Tab bar at the top (42px, aligned with the title-bar overlay). Right
        // pad clears the git/settings controls floating over the top-right.
        let tab_bar = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .h(px(42.0))
            .flex_shrink_0()
            .pl(px(8.0))
            .pr(px(76.0))
            .border_b_1()
            .border_color(rgb(t.border))
            .child(render_tab(
                self,
                super::RightView::Git,
                "icons/git-branch.svg",
                "Git",
                cx,
            ))
            .child(render_tab(
                self,
                super::RightView::Files,
                "icons/folder.svg",
                "Files",
                cx,
            ))
            // Directly after the view tabs, styled exactly like them: open a
            // fresh editor / browser pane in the active project.
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(4.0))
                    .child(render_action_pill(
                        "right-panel-open-editor",
                        "icons/file.svg",
                        "Editor",
                        &t,
                        cx,
                        |this: &mut Self, cx| {
                            if let Some(project_id) = this.active_project_id(cx) {
                                this.workspace.update(cx, |ws, cx| {
                                    // Empty path = fresh untitled buffer.
                                    ws.add_editor_right(&project_id, "", false, cx);
                                });
                            }
                        },
                    ))
                    .child(render_action_pill(
                        "right-panel-open-browser",
                        "icons/globe.svg",
                        "Browser",
                        &t,
                        cx,
                        |this: &mut Self, cx| {
                            if let Some(project_id) = this.active_project_id(cx) {
                                this.workspace.update(cx, |ws, cx| {
                                    // Empty URL = blank browser with the URL
                                    // bar ready for input.
                                    ws.add_browser_right(&project_id, "", cx);
                                });
                            }
                        },
                    )),
            );

        // Pinned view: title bar with the focused project's name. Sits at the
        // very top (toolbar height), aligned with the project title bars in
        // the grid; tinted with the project's folder color like those.
        let pinned_title_bar = if pinned_view_active {
            let (name, pin_color) = {
                let ws = self.workspace.read(cx);
                let project = self
                    .git_panel_project_id
                    .as_ref()
                    .and_then(|pid| ws.project(pid));
                let name = project.map(|p| p.name.clone()).unwrap_or_default();
                // Colored projects color the pin icon (not the bar background)
                let color = match project {
                    Some(p) => {
                        let c = ws.effective_folder_color(p);
                        if c != crate::theme::FolderColor::Default {
                            rgb(t.get_folder_color(c))
                        } else {
                            rgb(t.text_secondary)
                        }
                    }
                    None => rgb(t.text_secondary),
                };
                (name, color)
            };
            Some(
                div()
                    .h(px(notmux_ui::tokens::TITLE_BAR_STRIP_H))
                    .pl(px(12.0))
                    // Clear the git/settings controls floating over the top-right
                    .pr(px(76.0))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .bg(rgb(t.bg_header))
                    .border_b_1()
                    .border_color(rgb(t.border))
                    .child(
                        svg()
                            .path("icons/pinned.svg")
                            .size(px(12.0))
                            .text_color(pin_color),
                    )
                    .child(
                        div()
                            .text_size(ui_text_md(cx))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(t.text_primary))
                            .text_ellipsis()
                            .child(name),
                    ),
            )
        } else {
            None
        };

        let panel_container = div()
            .id("git-panel-container")
            .h_full()
            .w(px(git_panel_width))
            // Match the opaque center surface used by the reference right panel.
            .bg(rgb(t.bg_primary))
            .overflow_hidden()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .children(pinned_title_bar)
            .child(tab_bar)
            .child(div().w(px(configured_width)).flex_1().min_h_0().child(body));
        // Right status segment (remote/zoom/clock) at the panel's bottom —
        // disabled for now, kept for easy restore (the Pair button moved to
        // the sidebar's top strip):
        // .child(
        //     div()
        //         .w(px(configured_width))
        //         .flex_shrink_0()
        //         .child(self.status_bar_right.clone()),
        // );
        let _ = &self.status_bar_right;

        div()
            .id("git-panel-wrapper")
            .flex()
            .h_full()
            .flex_shrink_0()
            .child(render_git_panel_divider(&self.active_drag, cx))
            .child(panel_container)
            .into_any_element()
    }

    /// Render the Files tab: an Explorer / Search sub-tab switch (like the git
    /// panel's Changes / History header) over the focused project's file
    /// explorer or content-search panel. Both are wired to RootView's request
    /// broker so a click opens the central editor (`main_file_viewer`) rather
    /// than a viewer inside this panel.
    fn render_right_files_tab(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx);
        let proj = {
            let ws = self.workspace.read(cx);
            ws.focus_manager
                .focused_terminal_state()
                .map(|f| f.project_id)
                .and_then(|pid| ws.project(&pid).map(|p| (pid, p.path.clone())))
                .or_else(|| {
                    ws.visible_projects()
                        .first()
                        .map(|p| (p.id.clone(), p.path.clone()))
                })
        };
        let Some((pid, path)) = proj else {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(ui_text_md(cx))
                .text_color(rgb(t.text_muted))
                .child("No project")
                .into_any_element();
        };

        let body: AnyElement = match self.files_view {
            super::FilesView::Explorer => self.render_right_explorer(&pid, &path, cx),
            super::FilesView::Search => self.render_right_search(&pid, cx),
        };

        // One sub-tab (icon + label pill) in the Files header.
        let render_sub_tab = |this: &Self,
                              view: super::FilesView,
                              icon: &'static str,
                              label: &'static str,
                              cx: &mut Context<Self>| {
            let is_active = this.files_view == view;
            let fg = if is_active { t.text_primary } else { t.text_muted };
            div()
                .id(label)
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .px(px(10.0))
                .h(px(24.0))
                .rounded_md()
                .cursor_pointer()
                .when(is_active, |d| d.bg(rgb(t.bg_hover)))
                .when(!is_active, |d| d.hover(|s| s.bg(rgb(t.bg_hover))))
                .child(svg().path(icon).size(px(12.0)).text_color(rgb(fg)))
                .child(
                    div()
                        .text_size(ui_text_md(cx))
                        .text_color(rgb(fg))
                        .child(label),
                )
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.files_view = view;
                    cx.notify();
                }))
        };

        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(34.0))
            .px(px(6.0))
            .gap(px(4.0))
            .flex_shrink_0()
            .border_b_1()
            .border_color(rgb(t.border))
            .child(render_sub_tab(
                self,
                super::FilesView::Explorer,
                "icons/folder.svg",
                "Explorer",
                cx,
            ))
            .child(render_sub_tab(
                self,
                super::FilesView::Search,
                "icons/search.svg",
                "Search",
                cx,
            ))
            .when(self.files_view == super::FilesView::Search, |d| {
                d.child(div().flex_1())
                    .child(self.render_right_search_toggles(&pid, cx))
            });

        div()
            .size_full()
            .flex()
            .flex_col()
            .min_h_0()
            .child(header)
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }

    /// The Files tab's Explorer sub-view: per-project optimized file explorer.
    fn render_right_explorer(&mut self, pid: &str, path: &str, cx: &mut Context<Self>) -> AnyElement {
        if !self.right_explorers.contains_key(pid) {
            let broker = self.request_broker.clone();
            let workspace = self.workspace.clone();
            let explorer = cx.new({
                let pid = pid.to_string();
                let path = path.to_string();
                move |cx| {
                    crate::views::panels::right_files::file_explorer::FileExplorer::new_with_workspace(
                      pid,
                      std::path::PathBuf::from(&path),
                      notmux_workspace::requests::ExplorerHost::FilesTab,
                      broker,
                      workspace,
                      cx,
                    )
                }
            });
            cx.observe(&explorer, |_, _, cx| cx.notify()).detach();
            self.right_explorers.insert(pid.to_string(), explorer);
        }
        let explorer = self
            .right_explorers
            .get(pid)
            .cloned()
            .expect("just inserted");
        AnyView::from(explorer)
            .cached(StyleRefinement::default().size_full())
            .into_any_element()
    }

    /// The Files tab's Search sub-view: per-project content-search panel
    /// (reuses the sidebar's panel; result clicks open the central editor).
    fn render_right_search(&mut self, pid: &str, cx: &mut Context<Self>) -> AnyElement {
        let Some(panel) = self.ensure_right_search_panel(pid, cx) else {
            let t = theme(cx);
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(ui_text_md(cx))
                .text_color(rgb(t.text_muted))
                .child("Search unavailable")
                .into_any_element();
        };
        AnyView::from(panel)
            .cached(StyleRefinement::default().size_full())
            .into_any_element()
    }

    /// Get or lazily create the content-search panel for a project.
    fn ensure_right_search_panel(
        &mut self,
        pid: &str,
        cx: &mut Context<Self>,
    ) -> Option<Entity<notmux_views_sidebar::search_panel::ContentSearchPanel>> {
        if let Some(existing) = self.right_search_panels.get(pid) {
            return Some(existing.clone());
        }
        let fs = self.build_project_fs(pid, cx)?;
        let broker = self.request_broker.clone();
        let panel = cx.new({
            let pid = pid.to_string();
            move |cx| {
                notmux_views_sidebar::search_panel::ContentSearchPanel::new(pid, fs, broker, cx)
            }
        });
        cx.observe(&panel, |_, _, cx| cx.notify()).detach();
        self.right_search_panels.insert(pid.to_string(), panel.clone());
        Some(panel)
    }

    /// Right-aligned Aa / .* / ~ mode toggles for the Search sub-tab header.
    fn render_right_search_toggles(&mut self, pid: &str, cx: &mut Context<Self>) -> AnyElement {
        let Some(panel) = self.ensure_right_search_panel(pid, cx) else {
            return div().into_any_element();
        };
        let t = theme(cx);
        let (case_sensitive, regex_mode, fuzzy_mode) = {
            let p = panel.read(cx);
            (p.is_case_sensitive(), p.is_regex_mode(), p.is_fuzzy_mode())
        };

        let toggle = |id: &'static str,
                      label: &'static str,
                      active: bool,
                      panel: Entity<notmux_views_sidebar::search_panel::ContentSearchPanel>,
                      cx: &mut Context<Self>| {
            div()
                .id(ElementId::Name(format!("files-search-toggle-{id}").into()))
                .cursor_pointer()
                .px(px(7.0))
                .py(px(3.0))
                .rounded(px(4.0))
                .text_size(ui_text_md(cx))
                .when(active, |d| {
                    d.bg(rgb(t.border_active)).text_color(rgb(t.text_primary))
                })
                .when(!active, |d| d.text_color(rgb(t.text_muted)))
                .hover(|s| s.bg(rgb(t.bg_hover)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |_this, _, _window, cx| {
                        panel.update(cx, |panel, cx| match id {
                            "case" => panel.toggle_case_sensitive(cx),
                            "regex" => panel.toggle_regex_mode(cx),
                            "fuzzy" => panel.toggle_fuzzy_mode(cx),
                            _ => {}
                        });
                        cx.stop_propagation();
                    }),
                )
                .child(label)
        };

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .child(toggle("case", "Aa", case_sensitive, panel.clone(), cx))
            .child(toggle("regex", ".*", regex_mode, panel.clone(), cx))
            .child(toggle("fuzzy", "~", fuzzy_mode, panel, cx))
            .into_any_element()
    }

    /// Animate git panel to target if needed
    pub(super) fn animate_git_panel_to(&mut self, target: AnimationTarget, cx: &mut Context<Self>) {
        if let Some(target_value) = target.value() {
            self.animate_git_panel(target_value, cx);
        }
    }

    /// Animate git panel to target value (0.0 = collapsed, 1.0 = expanded)
    pub(super) fn animate_git_panel(&mut self, target: f32, cx: &mut Context<Self>) {
        let current = self.git_panel_ctrl.animation();

        if (current - target).abs() < 0.01 {
            self.git_panel_ctrl.set_animation(target);
            cx.notify();
            return;
        }

        let steps = SidebarController::animation_steps();
        let step_duration = std::time::Duration::from_millis(FRAME_TIME_MS);

        cx.spawn(async move |this: WeakEntity<RootView>, cx| {
            for i in 1..=steps {
                smol::Timer::after(step_duration).await;

                let progress = SidebarController::ease_progress(current, target, i, steps);

                let result = this.update(cx, |this, cx| {
                    this.git_panel_ctrl.set_animation(progress);
                    cx.notify();
                });
                if result.is_err() {
                    break;
                }
            }

            let _ = this.update(cx, |this, cx| {
                this.git_panel_ctrl.set_animation(target);
                cx.notify();
            });
        })
        .detach();
    }
}
