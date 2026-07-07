use crate::settings::open_settings_file;
use crate::theme::theme;
use crate::ui::tokens::ui_text_ms;
use crate::views::components::{dropdown_button, dropdown_option, dropdown_overlay};
use gpui::*;
use gpui_component::h_flex;
use super::SettingsPanel;
impl SettingsPanel {
    pub(super) fn render_header(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        h_flex()
            .px(px(16.0))
            .py(px(10.0))
            .border_b_1()
            .border_color(rgb(t.border))
            .items_center()
            .justify_between()
            .child(self.render_project_selector(cx))
            .child(
                h_flex()
                    .gap(px(8.0))
                    .child(
                        div()
                            .id("edit-settings-file-btn")
                            .cursor_pointer()
                            .px(px(10.0))
                            .py(px(4.0))
                            .rounded(px(4.0))
                            .bg(rgb(t.bg_secondary))
                            .hover(|s| s.bg(rgb(t.bg_hover)))
                            .text_size(ui_text_ms(cx))
                            .text_color(rgb(t.text_secondary))
                            .child("Edit in settings.json")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    open_settings_file();
                                    this.close(cx);
                                }),
                            ),
                    ),
            )
    }
    pub(super) fn render_project_selector(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let label = match &self.selected_project_id {
            None => "User".to_string(),
            Some(pid) => self
                .workspace
                .read(cx)
                .project(pid)
                .map(|p| p.name.clone())
                .unwrap_or_else(|| "Unknown".to_string()),
        };
        h_flex().gap(px(4.0)).child(
            {
                let project_bounds_setter =
                    Self::bounds_setter(cx, |s, b| s.project_button_bounds = b);
                dropdown_button(
                    "project-selector-btn",
                    &label,
                    self.project_dropdown_open,
                    &t,
                    cx,
                    project_bounds_setter,
                )
            }
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.project_dropdown_open = !this.project_dropdown_open;
                    this.font_dropdown_open = false;
                    this.shell_dropdown_open = false;
                    this.session_backend_dropdown_open = false;
                    cx.notify();
                }),
            ),
        )
    }
    pub(super) fn render_project_dropdown_overlay(
        &mut self,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx);
        let projects: Vec<(String, String)> = self
            .workspace
            .read(cx)
            .projects()
            .iter()
            .map(|p| (p.id.clone(), p.name.clone()))
            .collect();
        let is_user_selected = self.selected_project_id.is_none();
        dropdown_overlay("project-selector-dropdown", &t)
            .min_w(px(180.0))
            .max_h(px(250.0))
            .child(
                dropdown_option(
                    "project-opt-user",
                    "User (Global)",
                    is_user_selected,
                    &t,
                    cx,
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.select_project(None, cx);
                    }),
                ),
            )
            .children(projects.into_iter().map(|(id, name)| {
                let is_selected = self.selected_project_id.as_deref() == Some(&id);
                dropdown_option(format!("project-opt-{}", id), &name, is_selected, &t, cx)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener({
                            let id = id.clone();
                            move |this, _, _, cx| {
                                this.select_project(Some(id.clone()), cx);
                            }
                        }),
                    )
            }))
    }
}
