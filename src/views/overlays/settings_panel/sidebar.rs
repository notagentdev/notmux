use crate::theme::theme;
use crate::ui::tokens::{ui_text_md, ui_text_xs};
use gpui::prelude::*;
use gpui::*;
use gpui_component::h_flex;
use notmux_extensions::ExtensionRegistry;
use super::SettingsPanel;
use super::categories::SettingsCategory;
impl SettingsPanel {
    pub(super) fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let static_categories: Vec<SettingsCategory> = if self.selected_project_id.is_some() {
            SettingsCategory::project_categories().to_vec()
        } else {
            SettingsCategory::all().to_vec()
        };
        let ext_categories: Vec<(SettingsCategory, String)> = if self.selected_project_id.is_none()
        {
            cx.try_global::<ExtensionRegistry>()
                .map(|registry| {
                    let settings = crate::settings::settings_entity(cx)
                        .read(cx)
                        .settings
                        .clone();
                    registry
                        .extensions()
                        .iter()
                        .filter(|ext| {
                            ext.settings_view.is_some()
                                && settings.enabled_extensions.contains(ext.manifest.id)
                        })
                        .map(|ext| {
                            (
                                SettingsCategory::Extension(ext.manifest.id.to_string()),
                                ext.manifest.name.to_string(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        div()
            .id("settings-sidebar")
            .w(px(248.0))
            .flex_shrink_0()
            .bg(rgb(t.bg_primary))
            .pt(px(16.0))
            .px(px(10.0))
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(
                div()
                    .id("settings-back")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(12.0))
                    .py(px(7.0))
                    .mb(px(8.0))
                    .rounded_md()
                    .cursor_pointer()
                    .text_color(rgb(t.text_secondary))
                    .hover(|s| s.bg(rgb(t.bg_hover)))
                    .child(SharedString::from("←  Back to app"))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.close(cx);
                        }),
                    ),
            )
            .child(
                div()
                    .px(px(12.0))
                    .py(px(6.0))
                    .text_size(ui_text_xs(cx))
                    .text_color(rgb(t.text_muted))
                    .child(SharedString::from("SETTINGS")),
            )
            .children(static_categories.iter().map(|cat| {
                let is_active = *cat == self.active_category;
                let category = cat.clone();
                let label = cat.label().to_string();
                Self::render_sidebar_item(
                    &label,
                    is_active,
                    &t,
                    cx.listener(move |this, _, _, cx| {
                        this.active_category = category.clone();
                        this.close_all_dropdowns();
                        cx.notify();
                    }),
                    cx,
                )
            }))
            .children(ext_categories.into_iter().map(|(cat, name)| {
                let is_active = self.active_category == cat;
                let category = cat;
                Self::render_sidebar_item(
                    &name,
                    is_active,
                    &t,
                    cx.listener(move |this, _, _, cx| {
                        this.active_category = category.clone();
                        this.close_all_dropdowns();
                        cx.notify();
                    }),
                    cx,
                )
            }))
    }
    fn render_sidebar_item<T: Fn(&MouseDownEvent, &mut Window, &mut App) + 'static>(
        label: &str,
        is_active: bool,
        t: &notmux_core::theme::ThemeColors,
        on_click: T,
        cx: &App,
    ) -> impl IntoElement + use<T> {
        h_flex()
            .id(ElementId::Name(format!("sidebar-{}", label).into()))
            .w_full()
            .gap(px(10.0))
            .px(px(12.0))
            .py(px(7.0))
            .rounded_md()
            .cursor_pointer()
            .text_size(ui_text_md(cx))
            .when(is_active, |d| {
                d.bg(rgb(t.bg_secondary))
                    .text_color(rgb(t.text_primary))
            })
            .when(!is_active, |d| {
                d.text_color(rgb(t.text_secondary))
                    .hover(|s| s.bg(rgb(t.bg_hover)))
            })
            .child(label.to_string())
            .on_mouse_down(MouseButton::Left, on_click)
    }
}
