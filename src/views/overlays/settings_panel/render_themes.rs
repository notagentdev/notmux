use crate::theme::theme;
use crate::ui::tokens::{ui_text, ui_text_md, ui_text_ms, ui_text_sm, ui_text_xl};
use crate::views::overlays::theme_selector::{apply_theme_entry, selected_theme_index, theme_entries, ThemeEntry};
use gpui::*;
use gpui::prelude::*;
use gpui_component::h_flex;

use super::components::*;
use super::SettingsPanel;

impl SettingsPanel {
    pub(super) fn render_themes(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let themes = theme_entries();
        let selected_index = selected_theme_index(&themes, cx);

        div()
            .child(section_header("Themes", &t, cx))
            .child(
                section_container(&t)
                    .children(themes.iter().enumerate().map(|(index, entry)| {
                        self.render_theme_row(index, entry, index == selected_index, cx)
                    }))
            )
            .child(
                div()
                    .mt(px(12.0))
                    .text_size(ui_text_sm(cx))
                    .text_color(rgb(t.text_muted))
                    .child("Custom themes are loaded from your Vryn themes directory and appear here automatically."),
            )
    }

    fn render_theme_row(
        &self,
        index: usize,
        entry: &ThemeEntry,
        is_selected: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let t = theme(cx);
        let colors = entry.colors;
        let entry_for_click = entry.clone();
        let is_custom = entry.info.id.starts_with("custom:");

        div()
            .id(ElementId::Name(format!("settings-theme-{}", index).into()))
            .px(px(12.0))
            .py(px(10.0))
            .flex()
            .items_center()
            .gap(px(12.0))
            .cursor_pointer()
            .when(index + 1 < theme_entries().len(), |d| d.border_b_1().border_color(rgb(t.border)))
            .when(is_selected, |d| d.bg(rgb(t.bg_secondary)))
            .hover(|s| s.bg(rgb(t.bg_hover)))
            .on_mouse_down(MouseButton::Left, cx.listener(move |_this, _, _, cx| {
                apply_theme_entry(&entry_for_click, cx);
                cx.notify();
            }))
            .child(Self::render_theme_preview(colors, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .child(
                        h_flex()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .text_size(ui_text_xl(cx))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(rgb(t.text_primary))
                                    .child(entry.info.name.clone()),
                            )
                            .when(is_custom, |d| {
                                d.child(
                                    div()
                                        .px(px(6.0))
                                        .py(px(2.0))
                                        .rounded(px(4.0))
                                        .bg(rgb(t.bg_header))
                                        .text_size(ui_text_ms(cx))
                                        .text_color(rgb(t.text_secondary))
                                        .child("Custom"),
                                )
                            }),
                    )
                    .child(
                        div()
                            .text_size(ui_text_md(cx))
                            .text_color(rgb(t.text_muted))
                            .child(entry.info.description.clone()),
                    ),
            )
            .when(is_selected, |d| {
                d.child(
                    div()
                        .text_size(ui_text_xl(cx))
                        .text_color(rgb(t.border_active))
                        .child("✓"),
                )
            })
    }

    fn render_theme_preview(colors: crate::theme::ThemeColors, cx: &App) -> impl IntoElement {
        div()
            .w(px(92.0))
            .h(px(54.0))
            .rounded(px(5.0))
            .bg(rgb(colors.bg_primary))
            .border_1()
            .border_color(rgb(colors.border))
            .p(px(5.0))
            .flex()
            .flex_col()
            .gap(px(3.0))
            .overflow_hidden()
            .child(
                div()
                    .h(px(9.0))
                    .rounded(px(2.0))
                    .bg(rgb(colors.bg_header))
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .px(px(2.0))
                    .child(div().w(px(4.0)).h(px(4.0)).rounded_full().bg(rgb(colors.term_red)))
                    .child(div().w(px(4.0)).h(px(4.0)).rounded_full().bg(rgb(colors.term_yellow)))
                    .child(div().w(px(4.0)).h(px(4.0)).rounded_full().bg(rgb(colors.term_green))),
            )
            .child(
                h_flex()
                    .gap(px(3.0))
                    .child(
                        div()
                            .text_size(ui_text(6.0, cx))
                            .text_color(rgb(colors.term_green))
                            .child("$"),
                    )
                    .child(
                        div()
                            .text_size(ui_text(6.0, cx))
                            .text_color(rgb(colors.text_primary))
                            .child("vryn"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap(px(5.0))
                    .child(
                        div()
                            .text_size(ui_text(5.0, cx))
                            .text_color(rgb(colors.term_blue))
                            .child("src"),
                    )
                    .child(
                        div()
                            .text_size(ui_text(5.0, cx))
                            .text_color(rgb(colors.text_secondary))
                            .child("main.rs"),
                    ),
            )
    }
}
