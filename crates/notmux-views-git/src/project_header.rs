//! Git-related rendering for project column headers.
//!
//! Pure render functions extracted from `ProjectColumn` so they can be
//! reused without depending on the full view entity.

use notmux_core::theme::ThemeColors;
use notmux_files::file_tree::{
    FileTreeNode, build_file_tree, expandable_file_row_with_options, expandable_folder_row,
};
use notmux_git::{CiStatus, CommitLogEntry, FileDiffSummary, GitStatus, GraphRow, PrState};

use gpui::prelude::*;
use gpui::*;
use gpui_component::h_flex;
use gpui_component::tooltip::Tooltip;
use std::sync::Arc;
use notmux_ui::tokens::{ui_text_md, ui_text_ms, ui_text_sm};

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

// ── Graph rendering constants ───────────────────────────────────────────────

/// Width of each graph character column in pixels.
pub const GRAPH_CELL_W: f32 = 14.0;
/// Thickness of railway lines.
pub const RAIL_W: f32 = 2.0;
/// Diameter of commit dots.
pub const DOT_SIZE: f32 = 10.0;
/// Commit row height.
pub const COMMIT_ROW_H: f32 = 32.0;
/// Connector row height.
pub const CONNECTOR_ROW_H: f32 = 10.0;
/// Diameter of each mini-dot making up the dotted vertical rail.
const RAIL_DOT_SIZE: f32 = 2.0;
/// Vertical spacing between rail-dot centers (dot + gap).
const RAIL_DOT_PITCH: f32 = 4.0;
/// Gap between the commit node and the rail on each side.
const NODE_RAIL_GAP: f32 = 1.0;
/// Stroke width of the hollow HEAD ring.
const HEAD_RING_STROKE: f32 = 2.0;

/// Per-row render flags for the graph column.
#[derive(Default, Clone, Copy)]
pub struct GraphRowOpts {
    /// Suppress rail above the row's vertical center. Set for the first
    /// visible row so no rail pokes out above the top commit.
    pub skip_above_center: bool,
    /// Suppress rail below the vertical center. Set for the last visible
    /// row so no rail hangs off the bottom commit.
    pub skip_below_center: bool,
    /// Render the commit node as a hollow ring instead of a filled circle
    /// (VS Code's convention for HEAD).
    pub is_head: bool,
}

/// Lane color palette for graph railways.
const LANE_COLORS: &[fn(&ThemeColors) -> u32] = &[
    |t| t.term_cyan,
    |t| t.term_green,
    |t| t.term_yellow,
    |t| t.term_magenta,
    |t| t.term_blue,
    |t| t.term_red,
];

fn lane_color(lane_idx: usize, t: &ThemeColors) -> u32 {
    LANE_COLORS[lane_idx % LANE_COLORS.len()](t)
}

// ── Graph rendering ─────────────────────────────────────────────────────────

/// Render graph prefix as a single relatively-positioned container with
/// absolutely-positioned railway elements. This ensures lines connect
/// across lane centers regardless of character cell boundaries.
pub fn render_graph_column(
    graph: &str,
    max_len: usize,
    row_h: f32,
    opts: GraphRowOpts,
    t: &ThemeColors,
) -> Div {
    let padded: String = if graph.len() < max_len {
        format!("{:<width$}", graph, width = max_len)
    } else {
        graph.to_string()
    };

    // X coordinate of the rail's left edge for a given column position
    let rail_x = |pos: usize| -> f32 { pos as f32 * GRAPH_CELL_W + (GRAPH_CELL_W - RAIL_W) / 2.0 };

    let mid_y = (row_h - RAIL_W) / 2.0;
    let row_center_y = row_h / 2.0;

    let mut elements: Vec<AnyElement> = Vec::new();

    // Build a dotted vertical rail for a single lane. Renders a column of
    // small dots along `[y_start, y_end)` using the full lane color so the
    // trail stays visible against the background but doesn't drown out the
    // more prominent commit circles.
    let push_dotted_rail =
        |elements: &mut Vec<AnyElement>, pos: usize, color: u32, y_start: f32, y_end: f32| {
            if y_end <= y_start + 0.01 {
                return;
            }
            let rail_center_x = pos as f32 * GRAPH_CELL_W + (GRAPH_CELL_W - RAIL_DOT_SIZE) / 2.0;
            // Align dots to a stable grid so consecutive rows render a
            // continuous dashed line instead of shifting per row.
            let first_y = (y_start / RAIL_DOT_PITCH).ceil() * RAIL_DOT_PITCH;
            let mut y = first_y;
            while y + RAIL_DOT_SIZE <= y_end + 0.01 {
                elements.push(
                    div()
                        .absolute()
                        .left(px(rail_center_x))
                        .top(px(y))
                        .w(px(RAIL_DOT_SIZE))
                        .h(px(RAIL_DOT_SIZE))
                        .rounded(px(RAIL_DOT_SIZE / 2.0))
                        .bg(rgb(color))
                        .into_any_element(),
                );
                y += RAIL_DOT_PITCH;
            }
        };

    // Clip the full-row range `[0, row_h]` to the visible portion based on
    // the current row's top/bottom clipping flags (first/last row in view).
    let clip_range = |y_start: f32, y_end: f32| -> Option<(f32, f32)> {
        let mut s = y_start;
        let mut e = y_end;
        if opts.skip_above_center {
            s = s.max(row_center_y);
        }
        if opts.skip_below_center {
            e = e.min(row_center_y);
        }
        if e > s { Some((s, e)) } else { None }
    };

    for (pos, ch) in padded.chars().enumerate() {
        let lane_idx = pos / 2;
        let color = lane_color(lane_idx, t);

        match ch {
            '|' => {
                // Passing-through rail — clipped if this is the first/last row.
                if let Some((s, e)) = clip_range(0.0, row_h) {
                    push_dotted_rail(&mut elements, pos, color, s, e);
                }
            }
            '*' => {
                let dot_y = (row_h - DOT_SIZE) / 2.0;
                // Rail above the commit node (suppressed on the top row).
                if !opts.skip_above_center {
                    push_dotted_rail(&mut elements, pos, color, 0.0, dot_y - NODE_RAIL_GAP);
                }
                // Rail below (suppressed on the bottom row).
                if !opts.skip_below_center {
                    push_dotted_rail(
                        &mut elements,
                        pos,
                        color,
                        dot_y + DOT_SIZE + NODE_RAIL_GAP,
                        row_h,
                    );
                }
                // Commit node: hollow ring for HEAD, filled circle otherwise.
                let dot_x = pos as f32 * GRAPH_CELL_W + (GRAPH_CELL_W - DOT_SIZE) / 2.0;
                if opts.is_head {
                    elements.push(
                        div()
                            .absolute()
                            .left(px(dot_x))
                            .top(px(dot_y))
                            .w(px(DOT_SIZE))
                            .h(px(DOT_SIZE))
                            .rounded(px(DOT_SIZE / 2.0))
                            .border(px(HEAD_RING_STROKE))
                            .border_color(rgb(color))
                            .bg(rgb(t.bg_primary))
                            .into_any_element(),
                    );
                } else {
                    elements.push(
                        div()
                            .absolute()
                            .left(px(dot_x))
                            .top(px(dot_y))
                            .w(px(DOT_SIZE))
                            .h(px(DOT_SIZE))
                            .rounded(px(DOT_SIZE / 2.0))
                            .bg(rgb(color))
                            .into_any_element(),
                    );
                }
            }
            '\\' => {
                // Fork: S-curve from left lane (top) to right lane (bottom)
                let diag_color = lane_color(pos.div_ceil(2), t);
                let lx = rail_x(pos.saturating_sub(1));
                let rx = rail_x(pos + 1);

                // Top vertical: left lane center -> middle
                elements.push(
                    div()
                        .absolute()
                        .left(px(lx))
                        .top(px(0.0))
                        .w(px(RAIL_W))
                        .h(px(mid_y + RAIL_W))
                        .bg(rgb(diag_color))
                        .into_any_element(),
                );
                // Horizontal bridge: left lane center -> right lane center
                elements.push(
                    div()
                        .absolute()
                        .left(px(lx))
                        .top(px(mid_y))
                        .w(px(rx + RAIL_W - lx))
                        .h(px(RAIL_W))
                        .bg(rgb(diag_color))
                        .into_any_element(),
                );
                // Bottom vertical: right lane center -> bottom
                elements.push(
                    div()
                        .absolute()
                        .left(px(rx))
                        .top(px(mid_y))
                        .w(px(RAIL_W))
                        .h(px(row_h - mid_y))
                        .bg(rgb(diag_color))
                        .into_any_element(),
                );
            }
            '/' => {
                // Merge: S-curve from right lane (top) to left lane (bottom)
                let diag_color = lane_color(pos.div_ceil(2), t);
                let lx = rail_x(pos.saturating_sub(1));
                let rx = rail_x(pos + 1);

                // Top vertical: right lane center -> middle
                elements.push(
                    div()
                        .absolute()
                        .left(px(rx))
                        .top(px(0.0))
                        .w(px(RAIL_W))
                        .h(px(mid_y + RAIL_W))
                        .bg(rgb(diag_color))
                        .into_any_element(),
                );
                // Horizontal bridge
                elements.push(
                    div()
                        .absolute()
                        .left(px(lx))
                        .top(px(mid_y))
                        .w(px(rx + RAIL_W - lx))
                        .h(px(RAIL_W))
                        .bg(rgb(diag_color))
                        .into_any_element(),
                );
                // Bottom vertical: left lane center -> bottom
                elements.push(
                    div()
                        .absolute()
                        .left(px(lx))
                        .top(px(mid_y))
                        .w(px(RAIL_W))
                        .h(px(row_h - mid_y))
                        .bg(rgb(diag_color))
                        .into_any_element(),
                );
            }
            '_' => {
                // Horizontal connector
                elements.push(
                    div()
                        .absolute()
                        .left(px(pos as f32 * GRAPH_CELL_W))
                        .top(px(mid_y))
                        .w(px(GRAPH_CELL_W))
                        .h(px(RAIL_W))
                        .bg(rgb(color))
                        .into_any_element(),
                );
            }
            _ => {} // space -- nothing
        }
    }

    div().relative().flex_shrink_0().children(elements)
}

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

/// Render a single commit graph row (either a commit entry or a connector line).
///
/// `on_commit_click` is called with `(commit_hash, commit_message, commit_index)`
/// when the user clicks a commit row.
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
pub fn render_graph_row(
    row: &GraphRow,
    index: usize,
    max_graph_len: usize,
    all_commits: &[CommitLogEntry],
    on_commit_click: Option<Arc<dyn Fn(&str, &str, usize, &mut Window, &mut App)>>,
    opts: GraphRowOpts,
    t: &ThemeColors,
    cx: &App,
) -> AnyElement {
    let graph_width = max_graph_len as f32 * GRAPH_CELL_W;

    match row {
        GraphRow::Commit(entry) => {
            let row_el = h_flex()
                .id(ElementId::Name(format!("graph-row-{}", index).into()))
                .pl(px(4.0))
                .pr(px(12.0))
                .h(px(COMMIT_ROW_H))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(t.bg_hover)))
                .child(
                    render_graph_column(&entry.graph, max_graph_len, COMMIT_ROW_H, opts, t)
                        .w(px(graph_width))
                        .h(px(COMMIT_ROW_H)),
                )
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .h(px(COMMIT_ROW_H))
                        .items_center()
                        .gap(px(6.0))
                        .child(
                            div()
                                .text_size(ui_text_md(cx))
                                .text_color(rgb(t.text_primary))
                                .text_ellipsis()
                                .overflow_hidden()
                                .flex_shrink_1()
                                .min_w_0()
                                .child(entry.message.clone()),
                        )
                        .children(entry.refs.iter().map(|r| render_ref_label(r, t, cx)))
                        .child(
                            div()
                                .text_size(ui_text_ms(cx))
                                .text_color(rgb(t.text_muted))
                                .flex_shrink_0()
                                .child(entry.author.clone()),
                        ),
                );

            if let Some(cb) = on_commit_click {
                let hash = entry.hash.clone();
                let msg = entry.message.clone();
                let commit_idx = all_commits
                    .iter()
                    .position(|c| c.hash == entry.hash)
                    .unwrap_or(0);
                row_el
                    .on_click(move |_, window, cx| {
                        cb(&hash, &msg, commit_idx, window, cx);
                    })
                    .into_any_element()
            } else {
                row_el.cursor_default().into_any_element()
            }
        }
        GraphRow::Connector(graph) => div()
            .pl(px(4.0))
            .h(px(CONNECTOR_ROW_H))
            .child(
                render_graph_column(graph, max_graph_len, CONNECTOR_ROW_H, opts, t)
                    .w(px(graph_width))
                    .h(px(CONNECTOR_ROW_H)),
            )
            .into_any_element(),
    }
}

/// Render the "loading..." or "no commits" content, or the list of commit graph rows.
///
/// `on_commit_click` is called with `(commit_hash, commit_message, commit_index)`
/// when the user clicks on a commit row.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn render_commit_log_content(
    entries: &[GraphRow],
    loading: bool,
    on_commit_click: Option<Arc<dyn Fn(&str, &str, usize, &mut Window, &mut App)>>,
    t: &ThemeColors,
    cx: &App,
) -> AnyElement {
    if loading && entries.is_empty() {
        return div()
            .px(px(14.0))
            .py(px(16.0))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .text_size(ui_text_ms(cx))
                    .text_color(rgb(t.text_muted))
                    .child("Loading\u{2026}"),
            )
            .into_any_element();
    }

    if entries.is_empty() {
        return div()
            .px(px(14.0))
            .py(px(16.0))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .text_size(ui_text_ms(cx))
                    .text_color(rgb(t.text_muted))
                    .child("No commits"),
            )
            .into_any_element();
    }

    let max_graph_len = entries
        .iter()
        .map(|row| match row {
            GraphRow::Commit(e) => e.graph.len(),
            GraphRow::Connector(g) => g.len(),
        })
        .max()
        .unwrap_or(0);

    let all_commits: Vec<CommitLogEntry> = entries
        .iter()
        .filter_map(|r| match r {
            GraphRow::Commit(e) => Some(e.clone()),
            _ => None,
        })
        .collect();

    // Locate the first and last commit rows so rail drawing can be clipped
    // above the first and below the last — otherwise dashes extend off the
    // top/bottom of the visible log like a headless/tailless line.
    let first_commit_idx = entries
        .iter()
        .position(|r| matches!(r, GraphRow::Commit(_)));
    let last_commit_idx = entries
        .iter()
        .rposition(|r| matches!(r, GraphRow::Commit(_)));

    div()
        .children(entries.iter().enumerate().map(|(i, row)| {
            let before_first = first_commit_idx.is_none_or(|f| i < f);
            let after_last = last_commit_idx.is_none_or(|l| i > l);
            let is_first_commit_row = Some(i) == first_commit_idx;
            let is_last_commit_row = Some(i) == last_commit_idx;
            let opts = GraphRowOpts {
                skip_above_center: is_first_commit_row || before_first,
                skip_below_center: is_last_commit_row || after_last,
                is_head: is_first_commit_row,
            };
            render_graph_row(
                row,
                i,
                max_graph_len,
                &all_commits,
                on_commit_click.clone(),
                opts,
                t,
                cx,
            )
        }))
        .when(loading, |d| {
            d.child(
                div()
                    .w_full()
                    .h(px(24.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .text_size(ui_text_ms(cx))
                            .text_color(rgb(t.text_muted))
                            .child("Loading\u{2026}"),
                    ),
            )
        })
        .into_any_element()
}

/// Render the commit log popover header row (icon + "GRAPH" label).
pub fn render_commit_log_header(t: &ThemeColors, cx: &App) -> Div {
    h_flex()
        .px(px(10.0))
        .py(px(6.0))
        .gap(px(6.0))
        .items_center()
        .border_b_1()
        .border_color(rgb(t.border))
        .child(
            svg()
                .path("icons/git-commit.svg")
                .size(px(11.0))
                .text_color(rgb(t.text_muted)),
        )
        .child(
            div()
                .text_size(ui_text_ms(cx))
                .text_color(rgb(t.text_secondary))
                .child("GRAPH"),
        )
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
