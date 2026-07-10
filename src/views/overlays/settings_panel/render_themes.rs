use super::SettingsPanel;
use super::components::*;
use crate::theme::{GpuiTheme, ThemeColor, theme};
use crate::ui::tokens::ui_text_sm;
use crate::views::overlays::theme_selector::{
    ThemeEntry, apply_theme_entry, selected_theme_index, theme_entries,
};
use gpui::prelude::*;
use gpui::*;
use gpui_component::v_flex;

impl SettingsPanel {
    pub(super) fn render_themes(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let themes = theme_entries();
        let selected_index = selected_theme_index(&themes, cx);
        v_flex()
            .gap(px(24.0))
            .child(section_header("Themes", &t, cx))
            .child(
                v_flex()
                    .gap(px(10.0))
                    .child(subsection_label("AVAILABLE THEMES", &t, cx))
                    .child(
                        // Theme grid — same card layout as the notagent theme
                        // settings: mini preview window with swatches from the
                        // theme's own colors, active card outlined in accent.
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .gap(px(14.0))
                            .children(themes.iter().enumerate().map(|(index, entry)| {
                                self.render_theme_card(index, entry, index == selected_index, cx)
                            })),
                    )
                    .child(
                        div()
                            .text_size(ui_text_sm(cx))
                            .text_color(rgb(t.text_muted))
                            .child("Custom themes are loaded from your themes directory and appear here automatically."),
                    ),
            )
    }

    fn render_theme_card(
        &self,
        index: usize,
        entry: &ThemeEntry,
        is_selected: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let g = cx.global::<GpuiTheme>().clone();
        let accent = g.fg(ThemeColor::Accent);
        let text = g.fg(ThemeColor::Text);
        let outline = g.outline();
        let (bg, ac, tx) = entry.preview.unwrap_or((g.bg_base(), accent, text));
        let label = SharedString::from(entry.name.clone());
        let entry_for_click = entry.clone();
        let hover_bg = g.surface_2();

        div()
            .id(ElementId::Name(format!("settings-theme-{}", index).into()))
            .w(px(168.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .p(px(8.0))
            .rounded_lg()
            .bg(g.surface_1())
            .border_2()
            .border_color(if is_selected { accent } else { outline })
            .cursor_pointer()
            .hover(move |s| s.bg(hover_bg))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |_this, _, _, cx| {
                    apply_theme_entry(&entry_for_click, cx);
                    cx.notify();
                }),
            )
            .child(
                // Mini preview window.
                div()
                    .h(px(72.0))
                    .w_full()
                    .rounded_md()
                    .bg(bg)
                    .p(px(8.0))
                    .flex()
                    .flex_col()
                    .gap(px(5.0))
                    .child(div().w(px(60.0)).h(px(7.0)).rounded_full().bg(ac))
                    .child(div().w(px(96.0)).h(px(7.0)).rounded_full().bg(tx))
                    .child(div().w(px(78.0)).h(px(7.0)).rounded_full().bg(tx)),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(div().text_color(text).child(label))
                    .when(is_selected, |d| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(accent)
                                .child(SharedString::from("●")),
                        )
                    }),
            )
    }
}
