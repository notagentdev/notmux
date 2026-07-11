//! Git-related rendering for project column headers.
//!
//! Pure render functions extracted from `ProjectColumn` so they can be
//! reused without depending on the full view entity.

use notmux_core::theme::ThemeColors;
use notmux_files::file_tree::{
    FileTreeNode, build_file_tree, expandable_file_row_with_options, expandable_folder_row,
};
use notmux_git::{CiStatus, FileDiffSummary, GitStatus, PrState};

use gpui::prelude::*;
use gpui::*;
use gpui_component::h_flex;
use gpui_component::tooltip::Tooltip;
use std::sync::Arc;
use notmux_ui::tokens::{ui_text_ms, ui_text_sm};

use crate::settings::git_settings;

// ── Theme-dependent color traits ────────────────────────────────────────────

/// Extension trait: map `PrState` to a theme color.
pub trait PrStateColor {
    fn color(&self, t: &ThemeColors) -> u32;
}

impl PrStateColor for PrState {
    fn color(&self, t: &ThemeColors) -> u32 {
        match self {
            PrState::Open => t.term_green,
            PrState::Draft => t.text_muted,
            PrState::Merged => t.term_magenta,
            PrState::Closed => t.term_red,
        }
    }
}

/// Extension trait: map `CiStatus` to a theme color.
pub trait CiStatusColor {
    fn color(&self, t: &ThemeColors) -> u32;
}

impl CiStatusColor for CiStatus {
    fn color(&self, t: &ThemeColors) -> u32 {
        match self {
            CiStatus::Success => t.term_green,
            CiStatus::Failure => t.term_red,
            CiStatus::Pending => t.term_yellow,
        }
    }
}

// ── Commit log rendering ────────────────────────────────────────────────────

/// Commit row height.
pub const COMMIT_ROW_H: f32 = 32.0;

/// Render a ref label pill (e.g. "HEAD -> main", "origin/main", "tag: v1.0").
pub fn render_ref_label(ref_name: &str, t: &ThemeColors, cx: &App) -> AnyElement {
    let color = if ref_name.contains("HEAD") {
        t.term_cyan
    } else if ref_name.starts_with("tag:") {
        t.term_yellow
    } else if ref_name.starts_with("origin/") || ref_name.contains('/') {
        t.term_green
    } else {
        t.term_magenta
    };
    let bg = {
        let c: Hsla = rgb(color).into();
        hsla(c.h, c.s, c.l, 0.15)
    };
    div()
        .px(px(4.0))
        .py(px(1.0))
        .rounded(px(3.0))
        .bg(bg)
        .text_size(ui_text_sm(cx))
        .text_color(rgb(color))
        .flex_shrink_0()
        .max_w(px(140.0))
        .text_ellipsis()
        .overflow_hidden()
        .child(ref_name.to_string())
        .into_any_element()
}

// ── Diff popover file list ──────────────────────────────────────────────────

/// Build the diff file tree elements with click handlers attached.
///
/// `on_file_click` is called with the file path when the user clicks a file row.
/// All folders are rendered expanded (no toggle state in popovers).
#[allow(clippy::type_complexity)]
pub fn render_diff_file_list_interactive(
    summaries: &[FileDiffSummary],
    on_file_click: impl Fn(&str, &mut Window, &mut App) + 'static,
    t: &ThemeColors,
    cx: &App,
) -> Vec<AnyElement> {
    let tree = build_file_tree(summaries.iter().enumerate().map(|(i, f)| (i, &f.path)));
    let on_file_click: Arc<dyn Fn(&str, &mut Window, &mut App)> = Arc::new(on_file_click);
    render_diff_tree_node(&tree, 0, summaries, &on_file_click, t, cx)
}

#[allow(clippy::type_complexity)]
fn render_diff_tree_node(
    node: &FileTreeNode,
    depth: usize,
    summaries: &[FileDiffSummary],
    on_file_click: &Arc<dyn Fn(&str, &mut Window, &mut App)>,
    t: &ThemeColors,
    cx: &App,
) -> Vec<AnyElement> {
    let mut elements: Vec<AnyElement> = Vec::new();

    for (name, child) in &node.children {
        elements.push(expandable_folder_row(name, depth, true, t, cx).into_any_element());
        elements.extend(render_diff_tree_node(
            child,
            depth + 1,
            summaries,
            on_file_click,
            t,
            cx,
        ));
    }

    for &file_index in &node.files {
        if let Some(summary) = summaries.get(file_index) {
            let filename = summary.path.rsplit('/').next().unwrap_or(&summary.path);
            let is_deleted = summary.removed > 0 && summary.added == 0;

            let name_color = if summary.is_new {
                Some(t.diff_added_fg)
            } else if is_deleted {
                Some(t.diff_removed_fg)
            } else {
                None
            };

            let file_path = summary.path.clone();
            let cb = on_file_click.clone();
            elements.push(
                expandable_file_row_with_options(
                    filename,
                    depth,
                    name_color,
                    false,
                    git_settings(cx).monochrome_icons,
                    t,
                    cx,
                )
                    .id(ElementId::Name(format!("diff-file-{}", file_index).into()))
                    .on_click(move |_, window, cx| {
                        cb(&file_path, window, cx);
                    })
                    // Line counts
                    .when(summary.added > 0 || summary.removed > 0, |d| {
                        d.child(
                            h_flex()
                                .gap(px(4.0))
                                .text_size(ui_text_ms(cx))
                                .flex_shrink_0()
                                .when(summary.added > 0, |d| {
                                    d.child(
                                        div()
                                            .text_color(rgb(t.diff_added_fg))
                                            .child(format!("+{}", summary.added)),
                                    )
                                })
                                .when(summary.removed > 0, |d| {
                                    d.child(
                                        div()
                                            .text_color(rgb(t.diff_removed_fg))
                                            .child(format!("-{}", summary.removed)),
                                    )
                                }),
                        )
                    })
                    .into_any_element(),
            );
        }
    }

    elements
}

// ── Git status bar ──────────────────────────────────────────────────────────

/// Render the git branch status pill (branch name + PR badge + CI status).
///
/// `on_pr_click` is called when the user clicks the PR link (if any).
pub fn render_branch_status(
    status: &GitStatus,
    on_pr_click: Option<impl Fn(&mut Window, &mut App) + 'static>,
    t: &ThemeColors,
) -> AnyElement {
    let pr_info = status.pr_info.clone();
    let (icon_path, icon_color) = if let Some(ref pr) = pr_info {
        ("icons/git-pull-request.svg", pr.state.color(t))
    } else {
        ("icons/git-branch.svg", t.text_muted)
    };
    let pr_number = pr_info.as_ref().map(|p| p.number);
    let ci_checks = pr_info.as_ref().and_then(|p| p.ci_checks.clone());
    let has_pr = pr_info.is_some();

    let el = h_flex()
        .id("branch-status")
        .gap(px(3.0))
        .when(has_pr, |d: Stateful<Div>| {
            d.cursor_pointer()
                .rounded(px(3.0))
                .hover(|s| s.bg(rgb(t.bg_hover)))
                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                    cx.stop_propagation();
                })
        })
        .child(
            svg()
                .path(icon_path)
                .size(px(10.0))
                .text_color(rgb(icon_color)),
        )
        .child(
            div()
                .text_color(rgb(t.text_secondary))
                .max_w(px(100.0))
                .text_ellipsis()
                .overflow_hidden()
                .child(status.branch.clone().unwrap_or_default()),
        )
        .when_some(pr_number, |d, num| {
            d.child(div().text_color(rgb(t.text_muted)).child(format!("#{num}")))
        })
        .when_some(ci_checks, |d, checks| {
            let tooltip = checks.tooltip_text();
            d.child(
                div()
                    .id("ci-status")
                    .child(
                        svg()
                            .path(checks.status.icon())
                            .size(px(8.0))
                            .text_color(rgb(checks.status.color(t))),
                    )
                    .tooltip(move |_window, cx| Tooltip::new(tooltip.clone()).build(_window, cx)),
            )
        });

    if let Some(cb) = on_pr_click {
        el.on_click(move |_, window, cx| {
            cb(window, cx);
        })
        .into_any_element()
    } else {
        el.into_any_element()
    }
}

/// Render the diff stats badge (`+N / -M`).
///
/// Returns a `Div` (not yet stateful). The caller should:
/// - Assign an `id(...)` and attach hover/click handlers
/// - Attach a canvas to capture bounds for popover positioning
pub fn render_diff_stats_badge(lines_added: usize, lines_removed: usize, t: &ThemeColors) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(3.0))
        .px(px(4.0))
        .py(px(1.0))
        .rounded(px(3.0))
        .child(
            div()
                .text_color(rgb(t.term_green))
                .child(format!("+{}", lines_added)),
        )
        .child(div().text_color(rgb(t.text_muted)).child("/"))
        .child(
            div()
                .text_color(rgb(t.term_red))
                .child(format!("-{}", lines_removed)),
        )
}
