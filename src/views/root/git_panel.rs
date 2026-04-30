use crate::settings::settings_entity;
use crate::theme::theme;
use crate::ui::tokens::ui_text_md;
use crate::views::layout::split_pane::render_git_panel_divider;
use crate::views::sidebar_controller::{AnimationTarget, FRAME_TIME_MS, SidebarController};
use gpui::*;

use super::RootView;

impl RootView {
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

    /// Toggle the git panel without binding it to a project.
    /// Used when no project is available — the panel shows a placeholder.
    pub(super) fn toggle_git_panel_empty(&mut self, cx: &mut Context<Self>) {
        let target = self.git_panel_ctrl.toggle();
        let is_open = self.git_panel_ctrl.is_open();
        settings_entity(cx).update(cx, |s, cx| s.set_git_panel_open(is_open, cx));
        self.title_bar
            .update(cx, |tb, cx| tb.set_git_panel_open(is_open, cx));
        self.animate_git_panel_to(target, cx);
    }

    /// Render the git panel content.
    pub(super) fn render_git_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let show_panel = self.git_panel_ctrl.should_render();

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

        let has_content = self
            .git_panel_project_id
            .as_ref()
            .and_then(|pid| self.project_columns.get(pid))
            .is_some();

        let git_panel_width = self.git_panel_ctrl.current_width();
        let configured_width = self.git_panel_ctrl.width();
        let t = theme(cx);

        let content: AnyElement = if has_content {
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

        let panel_container = div()
            .id("git-panel-container")
            .h_full()
            .w(px(git_panel_width))
            .overflow_hidden()
            .flex_shrink_0()
            .child(div().w(px(configured_width)).h_full().child(content));

        div()
            .id("git-panel-wrapper")
            .flex()
            .h_full()
            .flex_shrink_0()
            .child(render_git_panel_divider(&self.active_drag, cx))
            .child(panel_container)
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
