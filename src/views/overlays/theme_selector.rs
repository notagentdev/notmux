use crate::keybindings::Cancel;
use crate::settings::settings_entity;
use crate::theme::{
    BUILTIN_THEMES, GpuiTheme, ThemeColor, get_custom_themes_dir, list_available_themes,
    load_gpui_theme, theme,
};
use crate::ui::tokens::{ui_text_md, ui_text_ms, ui_text_sm, ui_text_xl};
use crate::views::components::{
    ListOverlayAction, ListOverlayConfig, ListOverlayState, badge, handle_list_overlay_key,
    modal_backdrop, modal_content, modal_header,
};
use gpui::prelude::*;
use gpui::*;
use gpui_component::h_flex;
use notmux_ui::selectable_list::selectable_list_item;

/// Theme selection entry with preview swatches from the theme's own colors.
#[derive(Clone)]
pub(crate) struct ThemeEntry {
    /// Theme name (also the persisted identifier), e.g. "nord-midnight".
    pub(crate) name: String,
    /// True for user themes from the custom themes directory.
    pub(crate) is_custom: bool,
    /// Preview colors resolved from the theme itself: (page bg, accent, text).
    pub(crate) preview: Option<(Hsla, Hsla, Hsla)>,
}

/// All available themes: built-ins (in display order) plus custom themes.
pub(crate) fn theme_entries() -> Vec<ThemeEntry> {
    list_available_themes()
        .into_iter()
        .map(|name| {
            let is_custom = !BUILTIN_THEMES.contains(&name.as_str());
            let preview = load_gpui_theme(&name).ok().map(|p| {
                (
                    p.bg_base(),
                    p.fg(ThemeColor::Accent),
                    p.fg(ThemeColor::Text),
                )
            });
            ThemeEntry {
                name,
                is_custom,
                preview,
            }
        })
        .collect()
}

pub(crate) fn selected_theme_index(themes: &[ThemeEntry], cx: &App) -> usize {
    let current = settings_entity(cx).read(cx).settings.theme.clone();
    themes
        .iter()
        .position(|t| t.name == current)
        .unwrap_or(0)
}

/// Applies and persists a theme (rebuilds the theme global live).
pub(crate) fn apply_theme_entry(theme_entry: &ThemeEntry, cx: &mut App) {
    crate::theme::set_theme(&theme_entry.name, cx);
}

/// Live preview: swaps the theme global without persisting.
pub(crate) fn preview_theme_entry(theme_entry: &ThemeEntry, cx: &mut App) {
    if let Ok(t) = load_gpui_theme(&theme_entry.name) {
        crate::theme::apply_gpui_theme(t, cx);
    }
}

/// Restores the persisted theme (used when a preview is abandoned).
pub(crate) fn restore_persisted_theme(cx: &mut App) {
    let name = settings_entity(cx).read(cx).settings.theme.clone();
    if let Ok(t) = load_gpui_theme(&name) {
        crate::theme::apply_gpui_theme(t, cx);
    }
}

/// Theme selector overlay for choosing and previewing themes
pub struct ThemeSelector {
    focus_handle: FocusHandle,
    state: ListOverlayState<ThemeEntry>,
}

impl ThemeSelector {
    pub fn new(cx: &mut Context<Self>) -> Self {
        // Build theme list: built-in + custom
        let themes = theme_entries();

        // Find current theme index
        let selected_index = selected_theme_index(&themes, cx);

        let config = ListOverlayConfig::new("Theme")
            .subtitle("Select a color theme for the application")
            .size(480.0, 550.0)
            .centered()
            .key_context("ThemeSelector");

        let state = ListOverlayState::with_selected(themes, config, selected_index, cx);
        let focus_handle = state.focus_handle.clone();

        Self {
            focus_handle,
            state,
        }
    }

    fn close(&self, cx: &mut Context<Self>) {
        // Abandon any preview before closing
        restore_persisted_theme(cx);
        cx.emit(ThemeSelectorEvent::Close);
    }

    fn select_theme(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.state.items.len() {
            return;
        }

        let theme_entry = &self.state.items[index];
        // Apply and persist the theme
        apply_theme_entry(&theme_entry.clone(), cx);

        self.state.selected_index = index;
        cx.notify();

        // Close the dialog
        cx.emit(ThemeSelectorEvent::Close);
    }

    fn preview_theme(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.state.items.len() {
            return;
        }

        let theme_entry = self.state.items[index].clone();
        preview_theme_entry(&theme_entry, cx);
    }

    /// Mini preview window built from the theme's own colors — the same
    /// swatch layout as the notagent theme grid (accent pill + text pills on
    /// the page background).
    fn render_theme_preview(&self, entry: &ThemeEntry, cx: &App) -> impl IntoElement {
        let t = theme(cx);
        let (bg, ac, tx) = entry.preview.unwrap_or((
            rgb(t.bg_primary).into(),
            rgb(t.border_active).into(),
            rgb(t.text_primary).into(),
        ));
        div()
            .w(px(80.0))
            .h(px(50.0))
            .rounded(px(4.0))
            .bg(bg)
            .border_1()
            .border_color(rgb(t.border))
            .p(px(6.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .overflow_hidden()
            .child(div().w(px(36.0)).h(px(5.0)).rounded_full().bg(ac))
            .child(div().w(px(56.0)).h(px(5.0)).rounded_full().bg(tx))
            .child(div().w(px(46.0)).h(px(5.0)).rounded_full().bg(tx))
    }

    fn render_theme_row(
        &self,
        index: usize,
        entry: &ThemeEntry,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let t = theme(cx);
        let accent = cx.global::<GpuiTheme>().fg(ThemeColor::Accent);
        let is_selected = index == self.state.selected_index;
        let name = entry.name.clone();
        let is_custom = entry.is_custom;

        selectable_list_item(
            ElementId::Name(format!("theme-{}", index).into()),
            is_selected,
            &t,
        )
        .gap(px(12.0))
        .py(px(10.0))
        .border_b_1()
        .border_color(rgb(t.border))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, _window, cx| {
                this.select_theme(index, cx);
            }),
        )
        .child(self.render_theme_preview(entry, cx))
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    h_flex()
                        .gap(px(8.0))
                        .child(
                            div()
                                .text_size(ui_text_xl(cx))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(rgb(t.text_primary))
                                .child(name),
                        )
                        .when(is_custom, |d| d.child(badge("Custom", &t)))
                        .when(is_selected, |d| {
                            d.child(
                                div()
                                    .text_size(ui_text_md(cx))
                                    .text_color(accent)
                                    .child("✓"),
                            )
                        }),
                ),
        )
    }
}

pub enum ThemeSelectorEvent {
    Close,
}

impl EventEmitter<ThemeSelectorEvent> for ThemeSelector {}

impl Render for ThemeSelector {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let focus_handle = self.focus_handle.clone();
        let themes_dir = get_custom_themes_dir();
        let config_width = self.state.config.width;
        let config_max_height = self.state.config.max_height;
        let config_title = self.state.config.title.clone();
        let config_subtitle = self.state.config.subtitle.clone();

        if !focus_handle.is_focused(window) {
            window.focus(&focus_handle, cx);
        }

        modal_backdrop("theme-selector-backdrop", &t)
            .track_focus(&focus_handle)
            .key_context("ThemeSelector")
            .items_center()
            .on_action(cx.listener(|this, _: &Cancel, _window, cx| {
                this.close(cx);
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                match handle_list_overlay_key(&mut this.state, event, &[]) {
                    ListOverlayAction::Close => this.close(cx),
                    ListOverlayAction::SelectPrev | ListOverlayAction::SelectNext => {
                        this.preview_theme(this.state.selected_index, cx);
                        cx.notify();
                    }
                    ListOverlayAction::Confirm => {
                        let index = this.state.selected_index;
                        this.select_theme(index, cx);
                    }
                    _ => {}
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _window, cx| {
                    this.close(cx);
                }),
            )
            .child(
                modal_content("theme-selector-modal", &t)
                    .w(px(config_width))
                    .max_h(px(config_max_height))
                    .child(modal_header(
                        config_title,
                        config_subtitle,
                        &t,
                        cx,
                        cx.listener(|this, _, _window, cx| this.close(cx)),
                    ))
                    .child(
                        // Theme list
                        div()
                            .id("theme-list")
                            .flex_1()
                            .overflow_y_scroll()
                            .children(self.state.filtered.iter().enumerate().map(
                                |(i, filter_result)| {
                                    let entry = self.state.items[filter_result.index].clone();
                                    self.render_theme_row(i, &entry, cx)
                                },
                            )),
                    )
                    .child(
                        // Footer - custom themes info
                        div()
                            .px(px(16.0))
                            .py(px(10.0))
                            .border_t_1()
                            .border_color(rgb(t.border))
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .child(
                                div()
                                    .text_size(ui_text_ms(cx))
                                    .text_color(rgb(t.text_muted))
                                    .child("Add custom themes by placing JSON files in:"),
                            )
                            .child(
                                div()
                                    .text_size(ui_text_sm(cx))
                                    .font_family("monospace")
                                    .text_color(rgb(t.text_secondary))
                                    .child(themes_dir.display().to_string()),
                            ),
                    ),
            )
    }
}

impl_focusable!(ThemeSelector);
