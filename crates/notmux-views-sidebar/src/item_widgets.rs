//! Shared widget helpers for sidebar project/folder/terminal items.
//!
//! Each helper returns a partially-built element that the caller can chain
//! additional handlers onto (e.g. `.on_click()`).

use gpui::prelude::*;
use gpui::*;
use notmux_core::theme::ThemeColors;
use notmux_ui::rename_state::{RenameState, rename_input};
use notmux_ui::simple_input::SimpleInput;
use notmux_ui::tokens::{ui_text_md, ui_text_sm, ui_text_xs};

/// Expand/collapse arrow (chevron-down/right, 16x16).
///
/// Caller chains `.on_click()` to toggle.
pub fn sidebar_expand_arrow(
    id: impl Into<ElementId>,
    is_expanded: bool,
    t: &ThemeColors,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex_shrink_0()
        .w(px(16.0))
        .h(px(20.0))
        .flex()
        .items_center()
        .justify_center()
        .child(
            svg()
                .path(if is_expanded {
                    "icons/chevron-down.svg"
                } else {
                    "icons/chevron-right.svg"
                })
                .size(px(14.0))
                .text_color(rgb(t.text_secondary)),
        )
}

/// Color indicator container (16x16, cursor_pointer, hover opacity).
///
/// `child` is the inner element -- either a colored dot or a folder SVG.
/// Caller chains `.on_click()` to show color picker.
pub fn sidebar_color_indicator(id: impl Into<ElementId>, child: impl IntoElement) -> Stateful<Div> {
    div()
        .id(id)
        .flex_shrink_0()
        .w(px(18.0))
        .h(px(20.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|s| s.opacity(0.7))
        .child(child)
}
pub fn project_folder_icon_color(folder_color: notmux_core::theme::FolderColor, t: &ThemeColors) -> u32 {
    if folder_color == notmux_core::theme::FolderColor::Default {
        t.text_secondary
    } else {
        t.get_folder_color(folder_color)
    }
}
pub fn folder_icon(color: u32) -> impl IntoElement {
    svg()
        .path("icons/folder.svg")
        .size(px(16.0))
        .text_color(rgb(color))
}

/// Rename input container with SimpleInput.
///
/// Returns `Some(element)` if renaming is active, `None` otherwise.
/// Caller chains `.on_action(Cancel)` / `.on_key_down(Enter)`.
pub fn sidebar_rename_input<T: 'static + Clone>(
    id: impl Into<ElementId>,
    rename_state: &Option<RenameState<T>>,
    t: &ThemeColors,
    cx: &App,
) -> Option<Stateful<Div>> {
    let input = rename_input(rename_state)?;
    Some(
        div()
            .id(id)
            .flex_1()
            .min_w_0()
            .bg(rgb(t.bg_hover))
            .rounded(px(2.0))
            .child(SimpleInput::new(input).text_size(ui_text_md(cx)))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_click(|_, _window, cx| {
                cx.stop_propagation();
            }),
    )
}

/// Name label with ellipsis and standard text styling.
///
/// Caller chains `.on_click()` for select / double-click rename.
pub fn sidebar_name_label(
    id: impl Into<ElementId>,
    name: impl Into<SharedString>,
    t: &ThemeColors,
    cx: &App,
) -> Stateful<Div> {
    // Default label size (14px, `text_ui`).
    div()
        .id(id)
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_size(ui_text_md(cx))
        .text_color(rgb(t.text_primary))
        .text_ellipsis()
        .child(name.into())
}

/// Collapsible group header (e.g. "Terminals (3)" or "Services (2)").
///
/// Returns a `Stateful<Div>` so the caller can chain `.on_click()` to toggle collapse.
#[allow(clippy::too_many_arguments)]
pub fn sidebar_group_header(
    id: impl Into<ElementId>,
    label: &str,
    count: usize,
    is_collapsed: bool,
    is_cursor: bool,
    left_padding: f32,
    t: &ThemeColors,
    cx: &App,
) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(20.0))
        // Same rounded inset pill as the project/terminal rows (they combine
        // mx(6) with their padding, so subtract the margin here to keep the
        // caller-provided absolute indent).
        .mx(px(6.0))
        .pl(px((left_padding - 6.0).max(0.0)))
        .pr(px(8.0))
        .flex()
        .items_center()
        .gap(px(4.0))
        .rounded_lg()
        .cursor_pointer()
        .hover(|s| s.bg(rgb(t.bg_hover)))
        .when(is_cursor, |d: Stateful<Div>| d.bg(rgb(t.bg_hover)))
        .child(
            // Expand/collapse chevron (smaller than project arrow)
            svg()
                .path(if is_collapsed {
                    "icons/chevron-right.svg"
                } else {
                    "icons/chevron-down.svg"
                })
                .size(px(10.0))
                .text_color(rgb(t.text_muted))
                .flex_shrink_0(),
        )
        .child(
            // Group label
            div()
                .text_size(ui_text_sm(cx))
                .text_color(rgb(t.text_muted))
                .child(label.to_string()),
        )
        .child(
            // Item count badge
            div()
                .flex_shrink_0()
                .px(px(3.0))
                .py(px(0.0))
                .rounded(px(3.0))
                .bg(rgb(t.bg_secondary))
                .text_size(ui_text_xs(cx))
                .text_color(rgb(t.text_muted))
                .child(format!("{}", count)),
        )
}

/// Idle dot badge (6x6 circle in border_idle color).
///
/// Used to indicate terminals waiting for input when a project/folder is collapsed.
pub fn sidebar_idle_dot(t: &ThemeColors) -> Div {
    div()
        .flex_shrink_0()
        .w(px(6.0))
        .h(px(6.0))
        .rounded(px(3.0))
        .bg(rgb(t.border_idle))
}

/// Worktree count badge (git-branch icon + number).
/// Shown on parent projects that have active worktrees.
pub fn sidebar_worktree_badge(count: usize, t: &ThemeColors, cx: &App) -> impl IntoElement {
    div()
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(2.0))
        .child(
            svg()
                .path("icons/git-branch.svg")
                .size(px(10.0))
                .text_color(rgb(t.text_muted)),
        )
        .child(
            div()
                .text_size(ui_text_sm(cx))
                .text_color(rgb(t.text_muted))
                .child(format!("{}", count)),
        )
}

/// Terminal count badge for hidden/inactive projects.
/// Shown inline after the project name as a small highlighted badge.
pub fn sidebar_terminal_count_badge(count: usize, t: &ThemeColors, cx: &App) -> Div {
    div()
        .flex_shrink_0()
        .ml(px(4.0))
        .px(px(4.0))
        .rounded(px(3.0))
        .bg(rgb(t.bg_header))
        .text_size(ui_text_sm(cx))
        .text_color(rgb(t.text_primary))
        .child(format!("{}", count))
}

/// Project/worktree name with optional terminal count badge.
///
/// When `hide_badge` is false and the project has terminals, renders the name
/// alongside a count badge. Otherwise returns the name_label as-is.
pub fn sidebar_name_or_badge(
    name_label: Stateful<Div>,
    name: &str,
    hide_badge: bool,
    terminal_count: usize,
    t: &ThemeColors,
    cx: &App,
) -> AnyElement {
    if !hide_badge && terminal_count > 0 {
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(2.0))
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(ui_text_md(cx))
                    .text_color(rgb(t.text_primary))
                    .child(name.to_string()),
            )
            .child(sidebar_terminal_count_badge(terminal_count, t, cx))
            .into_any_element()
    } else {
        name_label.into_any_element()
    }
}

/// Empty spacer matching expand arrow dimensions (12x16).
pub fn sidebar_expand_spacer() -> Div {
    div().flex_shrink_0().w(px(16.0)).h(px(20.0))
}
#[cfg(test)]
mod tests {
    use super::project_folder_icon_color;
    use notmux_core::theme::{FolderColor, LIGHT_THEME};
    #[test]
    fn default_folder_icon_uses_neutral_theme_color() {
        assert_eq!(
            project_folder_icon_color(FolderColor::Default, &LIGHT_THEME),
            LIGHT_THEME.text_secondary,
        );
        assert_eq!(
            project_folder_icon_color(FolderColor::Blue, &LIGHT_THEME),
            LIGHT_THEME.folder_blue,
        );
    }
}
