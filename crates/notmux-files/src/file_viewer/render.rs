//! Rendering logic for the file viewer overlay.

use crate::code_view::{
    build_styled_text_with_backgrounds, find_word_boundaries, get_scrollbar_geometry,
    selection_bg_ranges,
};
use crate::file_search::Cancel;
use crate::file_tree::{FileTreeNode, expandable_folder_row};
use crate::selection::{Selection1DExtension, Selection2DNonEmpty};
use crate::syntax::HighlightedLine;
// The git bridge, NOT the app-wide terminal bridge: it maps selection,
// syntax palette, and editor surfaces from the app theme, so the editor
// matches the files/diff views instead of the terminal chrome.
use notmux_ui::theme::git_theme as theme;
use gpui::prelude::*;
use gpui::*;
use gpui_component::{h_flex, v_flex};
use gpui_component::scroll::{Scrollbar, ScrollbarShow};
use std::path::PathBuf;
use std::sync::Arc;
use notmux_core::theme::ThemeColors;
use notmux_markdown::RenderedNode;
use notmux_ui::code_block::code_block_container;
use notmux_ui::vscode_icon::vscode_file_icon_sized_with_options;
use notmux_ui::modal::fullscreen_overlay;
use notmux_ui::toggle::segmented_toggle;
use notmux_ui::tokens::{ui_text, ui_text_md, ui_text_ms, ui_text_sm, ui_text_xl};

use super::context_menu::TreeNodeTarget;
use super::{DisplayMode, FileViewer, SIDEBAR_WIDTH};

const SOURCE_TEXT_PADDING_LEFT: f32 = 10.0;

#[cfg(test)]
#[::core::prelude::v1::test]
fn source_columns_preserve_tabs_and_unicode() {
    let text = "\téa";
    assert_eq!(byte_index_for_char_column(text, 1), 4);
    assert_eq!(byte_index_for_char_column(text, 2), 6);
    assert_eq!(char_column_for_byte_index(text, 3), 0);
    assert_eq!(char_column_for_byte_index(text, 4), 1);
    assert_eq!(char_column_for_byte_index(text, 6), 2);
}

/// Helper to create rgba from u32 color and alpha.
fn rgba(color: u32, alpha: f32) -> Rgba {
    let r = ((color >> 16) & 0xFF) as f32 / 255.0;
    let g = ((color >> 8) & 0xFF) as f32 / 255.0;
    let b = (color & 0xFF) as f32 / 255.0;
    Rgba { r, g, b, a: alpha }
}

fn selection_for_display(tab: &super::FileViewerTab) -> super::Selection {
    let mut selection = tab.selection.clone();
    for endpoint in [&mut selection.start, &mut selection.end].into_iter().flatten() {
        endpoint.1 = byte_index_for_char_column(tab.buffer.line_str(endpoint.0), endpoint.1);
    }
    selection
}

fn byte_index_for_char_column(text: &str, column: usize) -> usize {
    text.chars().take(column).map(|ch| if ch == '\t' { 4 } else { ch.len_utf8() }).sum()
}

fn char_column_for_byte_index(text: &str, byte_index: usize) -> usize {
    let mut offset = 0;
    for (column, ch) in text.chars().enumerate() {
        offset += if ch == '\t' { 4 } else { ch.len_utf8() };
        if byte_index < offset { return column; }
    }
    text.chars().count()
}

fn source_cursor_canvas(
    layout: TextLayout,
    cursor_byte: usize,
    visible: bool,
    color: Hsla,
) -> impl IntoElement {
    canvas(
        move |_bounds, _window, _cx| {
            let pos = layout.position_for_index(cursor_byte);
            let line_h = layout.line_height();
            (pos, line_h)
        },
        move |_bounds, (cursor_pos, line_h), window, _cx| {
            if visible && let Some(pos) = cursor_pos {
                let cursor_h = px(14.0).min(line_h);
                let y_offset = (line_h - cursor_h) * 0.5;
                window.paint_quad(fill(
                    Bounds::new(point(pos.x, pos.y + y_offset), size(px(1.0), cursor_h)),
                    color,
                ));
            }
        },
    )
    .absolute()
    .size_full()
}

impl FileViewer {
    fn render_display_mode_toggle(&self, t: &ThemeColors, cx: &mut Context<Self>) -> impl IntoElement {
        let preview = self.active_tab().display_mode == DisplayMode::Preview;
        div().id("display-mode-toggle").debug_selector(|| "display-mode-toggle".into())
            .cursor_pointer()
            .on_click(cx.listener(|this, _, window, cx| {
                this.toggle_display_mode(cx);
                window.focus(&this.focus_handle, cx);
            }))
            .child(segmented_toggle(&[("Preview", preview), ("Raw", !preview)], t, cx))
    }

    /// Whether buffer `line` is an addition vs the baseline (diff editor).
    fn line_is_added(&self, line: usize) -> bool {
        self.active_tab()
            .line_diff
            .as_ref()
            .and_then(|ld| ld.added.get(line).copied())
            .unwrap_or(false)
    }

    pub(super) fn render_line(
        &self,
        line_number: usize,
        line: &HighlightedLine,
        t: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let tab = self.active_tab();
        let line_num_str = format!("{:>width$}", line_number + 1, width = tab.line_num_width);

        let font_size = self.file_font_size;
        let line_height = font_size * 1.8;
        let char_width = self.measured_char_width;
        let gutter_width = (tab.line_num_width as f32) * char_width + 16.0;

        let mut bg_ranges = selection_bg_ranges(&selection_for_display(tab), line_number, line.plain_text.len());
        bg_ranges.extend(self.search_bg_ranges_for_line(line_number));

        let plain_text = tab.buffer.line_str(line_number).to_string();
        let line_len = tab.buffer.line_char_len(line_number);
        let line_char_len = line_len;
        let cursor = tab.cursor;
        let show_cursor = !tab.loading
            && tab.error_message.is_none()
            && (!tab.is_markdown || tab.display_mode == DisplayMode::Source)
            && cursor.line == line_number;

        let styled_text = build_styled_text_with_backgrounds(&line.spans, &bg_ranges);
        let text_layout = styled_text.layout().clone();
        let cursor_byte = byte_index_for_char_column(&plain_text, cursor.column);

        // Diff-editor decorations: green background + accent bar for additions.
        // Deletions render as their own red rows (see `render_deleted_row`).
        // Colors + alphas match the git panel's inline diff (theme diff colors).
        let diff_mode = self.diff_mode;
        let added = diff_mode && self.line_is_added(line_number);
        let added_bg = rgba(t.diff_added_bg, 0.18);
        let accent_green = rgba(t.diff_added_fg, 0.7);

        div()
            .id(ElementId::Name(format!("line-{}", line_number).into()))
            .w_full()
            .flex()
            .h(px(line_height))
            .when(added, |d| d.bg(added_bg))
            .text_size(ui_text(font_size, cx))
            .font_family("monospace")
            .when(diff_mode, |d| {
                d.child(
                    div()
                        .w(px(3.0))
                        .h_full()
                        .flex_shrink_0()
                        .when(added, |b| b.bg(accent_green)),
                )
            })
            .on_mouse_down(MouseButton::Left, {
                let text_layout = text_layout.clone();
                let plain_text = plain_text.clone();
                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                    let col = char_column_for_byte_index(
                        &plain_text,
                        text_layout
                            .index_for_position(event.position)
                            .unwrap_or_else(|ix| ix),
                    )
                    .min(line_char_len);
                    let tab = this.active_tab_mut();
                    tab.cursor = tab.buffer.clamp_cursor(super::Cursor {
                        line: line_number,
                        column: col,
                    });
                    if event.click_count >= 3 {
                        tab.selection.start = Some((line_number, 0));
                        tab.selection.end = Some((line_number, line_len));
                        tab.selection.finish();
                    } else if event.click_count == 2 {
                        let (start, end) = find_word_boundaries(&plain_text, col);
                        tab.selection.start = Some((line_number, start));
                        tab.selection.end = Some((line_number, end));
                        tab.selection.finish();
                    } else {
                        tab.selection.start = Some((line_number, col));
                        tab.selection.end = Some((line_number, col));
                        tab.selection.is_selecting = true;
                    }
                    cx.notify();
                })
            })
            .on_mouse_move({
                let text_layout = text_layout.clone();
                cx.listener(move |this, event: &MouseMoveEvent, _window, cx| {
                    if this.active_tab().selection.is_selecting {
                        let col = char_column_for_byte_index(
                            &plain_text,
                            text_layout
                                .index_for_position(event.position)
                                .unwrap_or_else(|ix| ix),
                        )
                        .min(line_char_len);
                        let tab = this.active_tab_mut();
                        tab.selection.end = Some((line_number, col));
                        cx.notify();
                    }
                })
            })
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _window, cx| {
                    let tab = this.active_tab_mut();
                    tab.selection.finish();
                    tab.selection_autoscroll = None;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .w(px(gutter_width))
                    .pr(px(10.0))
                    .text_color(rgba(t.text_muted, 0.6))
                    .flex()
                    .items_center()
                    .justify_end()
                    .flex_shrink_0()
                    .child(line_num_str)
                    .child(
                        div()
                            .ml(px(10.0))
                            .w(px(1.0))
                            .h(px(line_height * 0.6))
                            .bg(rgba(t.border, 0.3))
                            .flex_shrink_0(),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .pl(px(SOURCE_TEXT_PADDING_LEFT))
                    .overflow_hidden()
                    .relative()
                    .whitespace_nowrap()
                    .line_height(px(line_height))
                    .child(styled_text)
                    .when(show_cursor, |d| {
                        d.child(source_cursor_canvas(
                            text_layout,
                            cursor_byte,
                            true,
                            rgb(t.text_primary).into(),
                        ))
                    }),
            )
    }

    /// Render a read-only deleted (baseline) row in red for the diff editor.
    /// Deleted rows carry no line number — they no longer exist in the file.
    fn render_deleted_row(&self, _old_line: usize, text: &str, t: &ThemeColors, cx: &App) -> Div {
        let tab = self.active_tab();
        let font_size = self.file_font_size;
        let line_height = font_size * 1.8;
        let char_width = self.measured_char_width;
        let gutter_width = (tab.line_num_width as f32) * char_width + 16.0;
        div()
            .w_full()
            .flex()
            .h(px(line_height))
            .bg(rgba(t.diff_removed_bg, 0.18))
            .text_size(ui_text(font_size, cx))
            .font_family("monospace")
            // Red accent bar (matches the green additions bar).
            .child(
                div()
                    .w(px(3.0))
                    .h_full()
                    .flex_shrink_0()
                    .bg(rgba(t.diff_removed_fg, 0.7)),
            )
            // Empty gutter (no number) — keeps the separator column aligned.
            .child(
                div()
                    .w(px(gutter_width))
                    .pr(px(10.0))
                    .flex()
                    .items_center()
                    .justify_end()
                    .flex_shrink_0()
                    .child(
                        div()
                            .ml(px(10.0))
                            .w(px(1.0))
                            .h(px(line_height * 0.6))
                            .bg(rgba(t.border, 0.3))
                            .flex_shrink_0(),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .pl(px(SOURCE_TEXT_PADDING_LEFT))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .line_height(px(line_height))
                    .text_color(rgb(t.diff_removed_fg))
                    .child(text.replace('\t', "    ")),
            )
    }

    /// Render visible lines for the virtualized list. In diff mode the list is
    /// the interleaved plan (buffer lines + red deleted rows); otherwise it is
    /// one row per buffer line.
    pub(super) fn render_visible_lines(
        &self,
        range: std::ops::Range<usize>,
        t: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let tab = self.active_tab();
        if self.diff_mode && tab.line_diff.is_some() {
            return range
                .filter_map(|i| match tab.diff_rows.get(i)? {
                    super::diff::DiffRow::Buffer(bi) => tab
                        .highlighted_lines
                        .get(*bi)
                        .map(|line| self.render_line(*bi, line, t, cx).into_any_element()),
                    super::diff::DiffRow::Deleted { old_line, text } => {
                        Some(self.render_deleted_row(*old_line, text, t, cx).into_any_element())
                    }
                })
                .collect();
        }
        range
            .filter_map(|i| {
                tab.highlighted_lines
                    .get(i)
                    .map(|line| self.render_line(i, line, t, cx).into_any_element())
            })
            .collect()
    }

    /// Render the file tree sidebar.
    pub(super) fn render_sidebar(
        &self,
        t: &ThemeColors,
        tree_elements: Vec<AnyElement>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active_count = self.show_ignored as u8 + self.show_hidden as u8;
        let _is_open = self.filter_popover_open;

        div()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .border_r_1()
            .border_color(rgb(t.border))
            .bg(rgb(t.bg_secondary))
            .flex()
            .flex_col()
            .child(
                div()
                    .px(px(12.0))
                    .py(px(10.0))
                    .border_b_1()
                    .border_color(rgb(t.border))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(ui_text_ms(cx))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgb(t.text_secondary))
                            .line_height(px(11.0))
                            .child("Files"),
                    )
                    .child({
                        let entity = cx.entity().downgrade();
                        let entity2 = entity.clone();
                        crate::list_overlay::file_filter_button(
                            "fv-filter-btn",
                            active_count,
                            t,
                            cx,
                            move |_, _, cx| {
                                if let Some(e) = entity.upgrade() {
                                    e.update(cx, |this, cx| {
                                        this.filter_popover_open = !this.filter_popover_open;
                                        cx.notify();
                                    });
                                }
                            },
                            move |bounds, _, cx| {
                                if let Some(e) = entity2.upgrade() {
                                    e.update(cx, |this, _| {
                                        this.filter_button_bounds = Some(bounds)
                                    });
                                }
                            },
                        )
                    }),
            )
            .child(
                div()
                    .id("file-viewer-tree")
                    .flex_1()
                    .overflow_y_scroll()
                    .track_scroll(&self.tree_scroll_handle)
                    .py(px(6.0))
                    .children(tree_elements),
            )
    }

    /// Recursively render file tree nodes with expand/collapse.
    pub(super) fn render_tree_node(
        &self,
        node: &FileTreeNode,
        depth: usize,
        parent_path: &str,
        t: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut elements: Vec<AnyElement> = Vec::new();
        let active_file_index = self.active_tab().selected_file_index;
        // Collect all file indices that have open tabs (for dimmer highlight)
        let open_file_indices: std::collections::HashSet<usize> = self
            .tabs
            .iter()
            .filter_map(|t| t.selected_file_index)
            .collect();

        for (name, child) in &node.children {
            let folder_path = if parent_path.is_empty() {
                name.clone()
            } else {
                format!("{}/{}", parent_path, name)
            };
            let is_expanded = self.expanded_folders.contains(&folder_path);
            let is_renaming = self.is_renaming_folder(&folder_path);
            let is_ctx_target = self.is_context_menu_target_folder(&folder_path);

            let indent = depth as f32 * 14.0;

            if is_renaming {
                // Build folder row with inline rename input instead of name label
                let mut row = div()
                    .id(ElementId::Name(
                        format!("fv-folder-{}-rename", folder_path).into(),
                    ))
                    .flex()
                    .items_center()
                    .h(px(26.0))
                    .pl(px(indent + 8.0))
                    .pr(px(12.0))
                    .bg(rgb(t.bg_selection))
                    .child(
                        svg()
                            .path(if is_expanded {
                                "icons/chevron-down.svg"
                            } else {
                                "icons/chevron-right.svg"
                            })
                            .size(px(14.0))
                            .text_color(rgb(t.text_muted))
                            .mr(px(4.0))
                            .flex_shrink_0(),
                    )
                    .child(
                        svg()
                            .path("icons/folder.svg")
                            .size(px(14.0))
                            .text_color(rgb(t.text_secondary))
                            .mr(px(4.0))
                            .flex_shrink_0(),
                    );
                if let Some(input) = self.render_rename_input(t, cx) {
                    row = row.child(input);
                }
                row = row.on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    if event.keystroke.key.as_str() == "enter" {
                        this.finish_rename(cx);
                    }
                }));
                elements.push(row.into_any_element());
            } else {
                let folder_path_clone = folder_path.clone();
                let folder_path_for_ctx = folder_path.clone();
                let abs_path_for_ctx =
                    PathBuf::from(self.project_fs.project_id()).join(&folder_path);

                elements.push(
                    expandable_folder_row(name, depth, is_expanded, t, cx)
                        .id(ElementId::Name(format!("fv-folder-{}", folder_path).into()))
                        .when(is_ctx_target, |d| d.bg(rgb(t.bg_selection)))
                        .on_click(cx.listener(move |this, _, _window, cx| {
                            this.toggle_folder(&folder_path_clone, cx);
                        }))
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener({
                                let folder_path = folder_path_for_ctx;
                                let abs_path = abs_path_for_ctx;
                                move |this, event: &MouseDownEvent, _, cx| {
                                    this.open_context_menu(
                                        event.position,
                                        TreeNodeTarget::Folder {
                                            folder_path: folder_path.clone(),
                                            abs_path: abs_path.clone(),
                                        },
                                        cx,
                                    );
                                    cx.stop_propagation();
                                }
                            }),
                        )
                        .into_any_element(),
                );
            }

            if is_expanded {
                elements.extend(self.render_tree_node(child, depth + 1, &folder_path, t, cx));
            }
        }

        for &file_index in &node.files {
            if let Some(file) = self.files.get(file_index) {
                let is_active = active_file_index == Some(file_index);
                let is_open = open_file_indices.contains(&file_index);
                let is_renaming = self.is_renaming_file(&file.path);
                let is_ctx_target = self.is_context_menu_target_file(&file.path);

                let highlight = is_active || is_ctx_target;
                let indent = depth as f32 * 14.0;

                if is_renaming {
                    // Build file row with inline rename input instead of name label
                    let mut row = div()
                        .id(ElementId::Name(
                            format!("fv-file-{}-rename", file_index).into(),
                        ))
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .h(px(26.0))
                        .pl(px(indent + 8.0 + 18.0))
                        .pr(px(12.0))
                        .bg(rgb(t.bg_selection))
                        .child(
                            vscode_file_icon_sized_with_options(
                                &file.filename,
                                px(16.0),
                                t,
                                self.monochrome_icons,
                                cx,
                            )
                            .mr(px(4.0)),
                        );
                    if let Some(input) = self.render_rename_input(t, cx) {
                        row = row.child(input);
                    }
                    row = row.on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                        if event.keystroke.key.as_str() == "enter" {
                            this.finish_rename(cx);
                        }
                    }));
                    elements.push(row.into_any_element());
                } else {
                    let file_path_for_ctx = file.path.clone();
                    elements.push(
                        crate::file_tree::expandable_file_row_with_options(
                            &file.filename,
                            depth,
                            None,
                            is_open || is_active,
                            self.monochrome_icons,
                            t,
                            cx,
                        )
                        .id(ElementId::Name(format!("fv-file-{}", file_index).into()))
                        .when(highlight, |d| d.bg(rgba(t.bg_selection, 0.5)))
                        .on_click(cx.listener(move |this, _, _window, cx| {
                            this.select_file(file_index, cx);
                        }))
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener({
                                let path = file_path_for_ctx;
                                move |this, event: &MouseDownEvent, _, cx| {
                                    this.open_context_menu(
                                        event.position,
                                        TreeNodeTarget::File { path: path.clone() },
                                        cx,
                                    );
                                    cx.stop_propagation();
                                }
                            }),
                        )
                        .into_any_element(),
                    );
                }
            }
        }

        elements
    }

    /// Render scrollbar thumb.
    pub(super) fn render_scrollbar(
        &self,
        t: &ThemeColors,
        thumb_y: f32,
        thumb_height: f32,
        is_dragging: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id("file-viewer-scrollbar-track")
            .absolute()
            .top_0()
            .bottom_0()
            .right_0()
            .w(px(12.0))
            .cursor(CursorStyle::Arrow)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                    let y = f32::from(event.position.y);
                    this.start_scrollbar_drag(y, cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                if this.active_tab().scrollbar_drag.is_some() {
                    let y = f32::from(event.position.y);
                    this.update_scrollbar_drag(y, cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _window, cx| this.end_scrollbar_drag(cx)),
            )
            .child(
                div()
                    .absolute()
                    .top(px(thumb_y))
                    .right(px(3.0))
                    .w(px(6.0))
                    .h(px(thumb_height))
                    .rounded(px(3.0))
                    .bg(rgb(if is_dragging {
                        t.scrollbar_hover
                    } else {
                        t.scrollbar
                    }))
                    .hover(|s| s.bg(rgb(t.scrollbar_hover))),
            )
    }

    /// Render the tab bar (styled like terminal tabs).
    fn render_tab_bar(&self, t: &ThemeColors, cx: &mut Context<Self>) -> impl IntoElement {
        let mut tab_elements: Vec<AnyElement> = Vec::new();

        for (i, tab) in self.tabs.iter().enumerate() {
            let is_active = i == self.active_tab;
            let label = tab.filename();

            tab_elements.push(
                div()
                    .id(ElementId::Name(format!("fv-tab-{}", i).into()))
                    .h(px(28.0))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .px(px(8.0))
                    .border_r_1()
                    .border_color(rgb(t.border))
                    .cursor_pointer()
                    .when(is_active, |d| {
                        d.bg(rgb(t.bg_secondary)).text_color(rgb(t.text_primary))
                    })
                    .when(!is_active, |d| {
                        d.bg(rgb(t.bg_header))
                            .text_color(rgb(t.text_secondary))
                            .hover(|s| s.bg(rgb(t.bg_hover)))
                    })
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.set_active_tab(i, cx);
                    }))
                    .on_mouse_down(
                        MouseButton::Middle,
                        cx.listener(move |this, _, _window, cx| {
                            this.close_tab(i, cx);
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                            this.tab_context_menu = Some(super::context_menu::TabContextMenu {
                                position: event.position,
                                tab_index: i,
                            });
                            cx.notify();
                        }),
                    )
                    .child(
                        h_flex()
                            .gap(px(6.0))
                            .items_center()
                            // File type icon
                            .child(vscode_file_icon_sized_with_options(
                                &label,
                                px(16.0),
                                t,
                                self.monochrome_icons,
                                cx,
                            ))
                            // Filename
                            .child(
                                div()
                                    .text_size(ui_text_md(cx))
                                    .max_w(px(160.0))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .child(label),
                            ),
                    )
                    // Close button
                    .child(
                        div()
                            .id(ElementId::Name(format!("fv-tab-close-{}", i).into()))
                            .cursor_pointer()
                            .ml(px(4.0))
                            .w(px(16.0))
                            .h(px(16.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(3.0))
                            .hover(|s| s.bg(rgb(t.bg_hover)))
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                this.close_tab(i, cx);
                            }))
                            .child(
                                svg()
                                    .path("icons/close.svg")
                                    .size(px(12.0))
                                    .text_color(rgb(t.text_muted)),
                            ),
                    )
                    .into_any_element(),
            );
        }

        h_flex()
            .id("fv-tabs-scroll")
            .h(px(28.0))
            .flex_shrink_0()
            .min_w_0()
            .overflow_x_scroll()
            .bg(rgb(t.bg_header))
            .border_b_1()
            .border_color(rgb(t.border))
            .children(tab_elements)
    }
}

impl Render for FileViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Check for externally modified files (throttled to 1/sec)
        self.check_active_tab_freshness();

        // First render after a diff-mode load: compute decorations once (edits
        // keep them fresh thereafter). Cheap — guarded to run only when missing.
        if self.diff_mode && self.active_tab().line_diff.is_none() && !self.active_tab().loading {
            self.recompute_active_diff();
        }

        let t = theme(cx);
        let focus_handle = self.focus_handle.clone();
        let tab = self.active_tab();
        let tab_loading = tab.loading;
        let has_error = tab.error_message.is_some();
        let error_message = tab.error_message.clone();
        let is_markdown = tab.is_markdown;
        let display_mode = tab.display_mode;
        let is_preview_mode = display_mode == DisplayMode::Preview;
        let embedded = self.embedded;
        let sidebar_visible = self.sidebar_visible && !embedded;
        let show_tabs = self.tabs.len() > 1;

        let filename = tab
            .file_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "File".to_string());
        let dirty = tab.buffer.is_dirty();
        let save_error = tab.save_error.clone();

        let relative_path = self
            .files
            .iter()
            .find(|f| f.path == tab.file_path)
            .map(|f| f.relative_path.clone())
            .unwrap_or_else(|| tab.file_path.to_string_lossy().to_string());

        // Measure actual monospace character width from font metrics
        let font = Font {
            family: "monospace".into(),
            weight: FontWeight::NORMAL,
            style: FontStyle::Normal,
            ..Default::default()
        };
        let font_size = self.file_font_size;
        let line_height = font_size * 1.8;
        let text_system = window.text_system();
        let font_id = text_system.resolve_font(&font);
        self.measured_char_width = text_system
            .advance(font_id, px(font_size), 'm')
            .map(|size| f32::from(size.width))
            .unwrap_or(font_size * 0.6);

        // Virtualization setup
        let tab = self.active_tab();
        // In diff mode the virtualized list includes the interleaved red
        // deleted rows, so size it to the render plan.
        let line_count = if self.diff_mode && tab.line_diff.is_some() {
            tab.diff_rows.len()
        } else {
            tab.line_count
        };
        let theme_colors = Arc::new(t);
        let view = cx.entity().clone();
        let scrollbar_geometry = get_scrollbar_geometry(&tab.source_scroll_handle);
        let is_dragging_scrollbar = tab.scrollbar_drag.is_some();

        // Pre-render tree elements for sidebar
        let tree_elements = if sidebar_visible {
            self.render_tree_node(&self.file_tree.clone(), 0, "", &t, cx)
        } else {
            Vec::new()
        };

        // Pre-render markdown preview with selection
        let tab = self.active_tab();
        let preview_nodes: Vec<RenderedNode> = if !has_error && is_preview_mode && is_markdown {
            tab.markdown_doc
                .as_ref()
                .map(|doc| {
                    let selection = tab.markdown_selection.normalized_non_empty();
                    doc.render_nodes_with_offsets(&t, cx, selection)
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        // Render tab bar
        let tab_bar: Option<AnyElement> = if show_tabs {
            Some(self.render_tab_bar(&t, cx).into_any_element())
        } else {
            None
        };

        // Fullscreen viewer owns focus. Embedded viewer must not steal focus from side panels.
        if !embedded
            && self.rename_state.is_none()
            && self.search_state.is_none()
            && !focus_handle.is_focused(window)
        {
            window.focus(&focus_handle, cx);
        }

        let root = if embedded {
            div()
                .id("file-viewer-embedded")
                .occlude()
                .size_full()
                .bg(rgb(t.bg_primary))
                .flex()
                .flex_col()
        } else {
            // Start below the title-bar strip so the overlay aligns with the
            // app chrome instead of covering its bottom edge.
            fullscreen_overlay("file-viewer", &t).when(
                cfg!(target_os = "macos") && !window.is_fullscreen(),
                |d| d.top(px(notmux_ui::tokens::TITLE_BAR_STRIP_H)),
            )
        };

        root
            .track_focus(&focus_handle)
            .key_context("FileViewer")
            .when(!is_preview_mode, |d| d.cursor(CursorStyle::IBeam))
            // Selections must finish on ANY left mouse-up. The per-line and
            // per-markdown-node up handlers miss releases over gaps or other
            // elements, leaving `is_selecting` stuck — after which every mouse
            // move notifies and the whole (markdown) document re-renders per
            // frame, making the app laggy while an editor is open.
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _window, cx| {
                    let tab = this.active_tab_mut();
                    if tab.selection.is_selecting {
                        tab.selection.finish();
                        tab.selection_autoscroll = None;
                        cx.notify();
                    }
                    if tab.markdown_selection.is_selecting {
                        tab.markdown_selection.finish();
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _window, cx| {
                    let tab = this.active_tab_mut();
                    if tab.selection.is_selecting {
                        tab.selection.finish();
                        tab.selection_autoscroll = None;
                        cx.notify();
                    }
                    if tab.markdown_selection.is_selecting {
                        tab.markdown_selection.finish();
                        cx.notify();
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseDownEvent, window, cx| {
                    if this.embedded {
                        window.focus(&this.focus_handle, cx);
                        if let Some(on_click) = &this.on_click_embedded {
                            on_click(window, cx);
                        }
                    }
                }),
            )
            .on_action(cx.listener(|this, _: &Cancel, window, cx| {
                // Dismiss overlays in priority order before default close behavior
                if this.editor_context_menu.take().is_some() { cx.notify(); return; } if this.tab_context_menu.is_some() {
                    this.tab_context_menu = None;
                    cx.notify();
                    return;
                }
                if this.context_menu.is_some() {
                    this.close_context_menu(cx);
                    return;
                }
                if this.rename_state.is_some() {
                    this.cancel_rename(cx);
                    return;
                }
                if this.delete_confirm.is_some() {
                    this.cancel_delete(cx);
                    return;
                }
                if this.search_state.is_some() {
                    this.close_search(window, cx);
                    return;
                }

                let tab = this.active_tab();
                let is_preview = tab.display_mode == DisplayMode::Preview;
                if is_preview && tab.markdown_selection.normalized_non_empty().is_some() {
                    this.active_tab_mut().markdown_selection.clear();
                    cx.notify();
                } else if this.active_tab().selection.normalized_non_empty().is_some() {
                    this.active_tab_mut().selection.clear();
                    cx.notify();
                } else {
                    this.close(cx);
                }
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                // Don't intercept keys when search input is focused
                if this.search_state.as_ref().is_some_and(|s| {
                    s.input.read(cx).focus_handle(cx).is_focused(window)
                }) {
                    return;
                }

                let key = event.keystroke.key.as_str();
                let modifiers = &event.keystroke.modifiers;
                let tab = this.active_tab();
                let is_preview = tab.display_mode == DisplayMode::Preview;
                let is_md = tab.is_markdown;
                let is_source_editing = !is_preview;

                match key {
                    "s" if modifiers.platform || modifiers.control => {
                        this.save_active_tab(cx);
                    }
                    "f" if modifiers.platform || modifiers.control => {
                        if !is_preview {
                            this.open_search(window, cx);
                        }
                    }
                    "tab" if is_md && !modifiers.control && !modifiers.shift => {
                        this.toggle_display_mode(cx);
                    }
                    "tab" if modifiers.control && modifiers.shift => {
                        this.prev_tab(cx);
                    }
                    "tab" if modifiers.control => {
                        this.next_tab(cx);
                    }
                    "b" if !is_source_editing && !embedded && !modifiers.platform && !modifiers.control => {
                        this.toggle_sidebar(cx);
                    }
                    "x" if modifiers.platform || modifiers.control => { this.cut_selection(cx); } "v" if modifiers.platform || modifiers.control => { this.paste_clipboard(cx); } "c" if modifiers.platform || modifiers.control => {
                        if is_preview {
                            this.copy_markdown_selection(cx);
                        } else {
                            this.copy_selection(cx);
                        }
                    }
                    "a" if modifiers.platform || modifiers.control => {
                        if is_preview {
                            this.select_all_markdown(cx);
                        } else {
                            this.select_all(cx);
                        }
                    }
                    "w" if modifiers.platform || modifiers.control => {
                        this.close_active_tab(cx);
                    }
                    "r" if !is_source_editing && !modifiers.platform && !modifiers.control => {
                        this.refresh_file_tree_async(cx);
                    }
                    "left" if modifiers.alt => {
                        this.go_back(cx);
                    }
                    "right" if modifiers.alt => {
                        this.go_forward(cx);
                    }
                    "enter" if is_source_editing => {
                        this.insert_text_at_cursor("\n", cx);
                    }
                    "backspace" if is_source_editing => {
                        this.delete_backward_at_cursor(cx);
                    }
                    "delete" if is_source_editing => {
                        this.delete_forward_at_cursor(cx);
                    }
                    "left" if is_source_editing => {
                        this.move_cursor_left(cx);
                    }
                    "right" if is_source_editing => {
                        this.move_cursor_right(cx);
                    }
                    "up" if is_source_editing => {
                        this.move_cursor_up(cx);
                    }
                    "down" if is_source_editing => {
                        this.move_cursor_down(cx);
                    }
                    "home" if is_source_editing => {
                        this.move_cursor_line_start(cx);
                    }
                    "end" if is_source_editing => {
                        this.move_cursor_line_end(cx);
                    }
                    _ => {
                        if is_source_editing
                            && !modifiers.platform
                            && !modifiers.control
                            && !modifiers.alt
                            && let Some(ref s) = event.keystroke.key_char
                            && !s.is_empty()
                            && !s.chars().next().is_none_or(|c| c.is_control() && c != ' ')
                        {
                            this.insert_text_at_cursor(s, cx);
                        }
                    }
                }
            }))
            .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _window, cx| {
                if this.active_tab().scrollbar_drag.is_some() {
                    let y = f32::from(event.position.y);
                    this.update_scrollbar_drag(y, cx);
                } else if !is_preview_mode
                    && this.active_tab().selection.is_selecting
                    && let Some(bounds) = this.source_content_bounds
                {
                    let pointer_y = f32::from(event.position.y - bounds.origin.y);
                    let viewport_height = f32::from(bounds.size.height);
                    this.update_source_selection_from_pointer(
                        pointer_y,
                        viewport_height,
                        line_height,
                        cx,
                    );
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _window, cx| {
                    if this.active_tab().scrollbar_drag.is_some() {
                        this.end_scrollbar_drag(cx);
                    }
                    if this.active_tab().selection.is_selecting {
                        let tab = this.active_tab_mut();
                        tab.selection.finish();
                        tab.selection_autoscroll = None;
                        cx.notify();
                    }
                }),
            )
            // Header — only in the fullscreen overlay. The embedded editor
            // pane already shows the file name in its layout tab bar.
            .child(if embedded { h_flex().when(is_markdown, |d| d.h(px(32.0)).flex_shrink_0().px(px(8.0)).child(self.render_display_mode_toggle(&t, cx))).into_any_element() } else {
                div()
                    .px(px(16.0))
                    .py(px(12.0))
                    .border_b_1()
                    .border_color(rgb(t.border))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        h_flex()
                            .gap(px(10.0))
                            .when(!embedded, |d| d.child(
                                div()
                                    .id("sidebar-toggle")
                                    .cursor_pointer()
                                    .w(px(28.0))
                                    .h(px(28.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(6.0))
                                    .bg(rgb(if sidebar_visible {
                                        t.bg_selection
                                    } else {
                                        t.bg_secondary
                                    }))
                                    .hover(|s| s.bg(rgb(t.bg_hover)))
                                    .on_click(cx.listener(|this, _, _window, cx| {
                                        this.toggle_sidebar(cx);
                                    }))
                                    .child(
                                        svg()
                                            .path("icons/chevron-right.svg")
                                            .size(px(14.0))
                                            .text_color(rgb(t.text_muted)),
                                    ),
                            ))
                            .child(
                                v_flex()
                                    .gap(px(2.0))
                                    .child(
                                        div()
                                            .text_size(ui_text_xl(cx))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(rgb(t.text_primary))
                                            .child(if dirty {
                                                format!("{} *", filename)
                                            } else {
                                                filename.clone()
                                            }),
                                    )
                                    .child(
                                        div()
                                            .text_size(ui_text_ms(cx))
                                            .text_color(rgb(t.text_muted))
                                            .child(relative_path),
                                    ),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap(px(12.0))
                            .when(is_markdown, |d| {
                                d.child(
                                    self.render_display_mode_toggle(&t, cx),
                                )
                            })
                            .child(
                                div()
                                    .id("close-button")
                                    .cursor_pointer()
                                    .px(px(8.0))
                                    .py(px(4.0))
                                    .rounded(px(4.0))
                                    .hover(|s| s.bg(rgb(t.bg_secondary)))
                                    .on_click(cx.listener(|this, _, _window, cx| this.close(cx)))
                                    .child(
                                        div()
                                            .text_size(ui_text(18.0, cx))
                                            .text_color(rgb(t.text_muted))
                                            .child("\u{00d7}"),
                                    ),
                            ),
                    )
                    .into_any_element()
            })
            .when_some(save_error, |d, err| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .px(px(10.0))
                        .py(px(5.0))
                        .bg(rgb(t.error))
                        .text_color(rgb(t.bg_primary))
                        .text_size(ui_text_sm(cx))
                        .child(err),
                )
            })
            // Main content area: sidebar + (tab bar + content)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .when(sidebar_visible, |d| {
                        d.child(self.render_sidebar(&t, tree_elements, cx))
                    })
                    .child(
                        v_flex()
                            .flex_1()
                            .h_full()
                            .min_h_0()
                            .min_w_0()
                            // Tab bar (above editor, not above sidebar)
                            .when_some(tab_bar, |d, tab_bar| d.child(tab_bar))
                            // In-file search bar
                            .when(self.search_state.is_some(), |d| {
                                d.child(self.render_search_bar(&t, cx))
                            })
                            .when(tab_loading, |d| {
                                d.child(
                                    div()
                                        .flex_1()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(
                                            div()
                                                .text_size(ui_text_sm(cx))
                                                .text_color(rgb(t.text_muted))
                                                .child("Loading…"),
                                        ),
                                )
                            })
                            .when(!tab_loading && has_error, |d| {
                                d.child(
                                    div()
                                        .flex_1()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(
                                            div()
                                                .text_size(ui_text_xl(cx))
                                                .text_color(rgb(t.text_muted))
                                                .child(error_message.unwrap_or_default()),
                                        ),
                                )
                            })
                            .when(!tab_loading && !has_error && !is_preview_mode, |d| {
                                let tc = theme_colors.clone();
                                let view_clone = view.clone();
                                d.child(
                                    div()
                                        .id("file-content").on_mouse_down(MouseButton::Right, cx.listener(Self::show_editor_context_menu))
                                        .flex_1()
                                        .min_h_0()
                                        .relative()
                                        .child(canvas({
                                            let entity = cx.entity().downgrade();
                                            move |bounds, _, cx| {
                                                if let Some(entity) = entity.upgrade() {
                                                    entity.update(cx, |this, _| {
                                                        this.source_content_bounds = Some(bounds);
                                                    });
                                                }
                                            }
                                        }, |_, _, _, _| {}).absolute().size_full())
                                        .child(
                                            uniform_list(
                                                "file-lines",
                                                line_count,
                                                move |range, _window, cx| {
                                                    let tc = tc.clone();
                                                    view_clone.update(cx, |this, cx| {
                                                        this.render_visible_lines(range, &tc, cx)
                                                    })
                                                },
                                            )
                                            .size_full()
                                            .bg(rgb(t.bg_secondary))
                                            .cursor(CursorStyle::IBeam)
                                            .track_scroll(
                                                &self.active_tab().source_scroll_handle,
                                            ),
                                        )
                                        .when(scrollbar_geometry.is_some(), |d| {
                                            let (_, _, thumb_y, thumb_height) =
                                                scrollbar_geometry.expect("guarded by is_some() in when()");
                                            d.child(self.render_scrollbar(
                                                &t,
                                                thumb_y,
                                                thumb_height,
                                                is_dragging_scrollbar,
                                                cx,
                                            ))
                                        }),
                                )
                            })
                            .when(!tab_loading && !has_error && is_preview_mode, |d| {
                                let mut content_children: Vec<AnyElement> = Vec::new();
                                let mut node_idx = 0usize;

                                for rendered_node in preview_nodes {
                                    match rendered_node {
                                        RenderedNode::Simple {
                                            div: node_div,
                                            start_offset,
                                            end_offset,
                                        } => {
                                            let node_end = end_offset.saturating_sub(1);
                                            let idx = node_idx;
                                            content_children.push(
                                                div()
                                                    .id(ElementId::Name(
                                                        format!("md-node-{}", idx).into(),
                                                    ))
                                                    .w_full()
                                                    .on_mouse_down(
                                                        MouseButton::Left,
                                                        cx.listener(
                                                            move |this,
                                                                  event: &MouseDownEvent,
                                                                  _window,
                                                                  cx| {
                                                                let tab = this.active_tab_mut();
                                                                if event.click_count == 2 {
                                                                    tab.markdown_selection.start =
                                                                        Some(start_offset);
                                                                    tab.markdown_selection.end =
                                                                        Some(node_end);
                                                                    tab.markdown_selection
                                                                        .finish();
                                                                } else {
                                                                    tab.markdown_selection.start =
                                                                        Some(start_offset);
                                                                    tab.markdown_selection.end =
                                                                        Some(start_offset);
                                                                    tab.markdown_selection
                                                                        .is_selecting = true;
                                                                }
                                                                cx.notify();
                                                            },
                                                        ),
                                                    )
                                                    .on_mouse_move(cx.listener(
                                                        move |this,
                                                              _event: &MouseMoveEvent,
                                                              _window,
                                                              cx| {
                                                            let tab = this.active_tab_mut();
                                                            if tab.markdown_selection.is_selecting
                                                                && let Some(sel_start) =
                                                                    tab.markdown_selection.start
                                                                {
                                                                    if start_offset >= sel_start {
                                                                        tab.markdown_selection
                                                                            .end = Some(node_end);
                                                                    } else {
                                                                        tab.markdown_selection
                                                                            .end =
                                                                            Some(start_offset);
                                                                    }
                                                                    cx.notify();
                                                                }
                                                        },
                                                    ))
                                                    .on_mouse_up(
                                                        MouseButton::Left,
                                                        cx.listener(
                                                            |this,
                                                             _event: &MouseUpEvent,
                                                             _window,
                                                             cx| {
                                                                this.active_tab_mut()
                                                                    .markdown_selection
                                                                    .finish();
                                                                cx.notify();
                                                            },
                                                        ),
                                                    )
                                                    .child(node_div)
                                                    .into_any_element(),
                                            );
                                            node_idx += 1;
                                        }
                                        RenderedNode::CodeBlock {
                                            language, lines, ..
                                        } => {
                                            let idx = node_idx;
                                            let line_children: Vec<AnyElement> = lines
                                                .into_iter()
                                                .enumerate()
                                                .map(
                                                    |(line_idx, (line_div, start_offset, end_offset))| {
                                                        let line_end =
                                                            end_offset.saturating_sub(1);
                                                        div()
                                                        .id(ElementId::Name(format!("md-code-{}-line-{}", idx, line_idx).into()))
                                                        .on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                                                            let tab = this.active_tab_mut();
                                                            if event.click_count == 2 {
                                                                tab.markdown_selection.start = Some(start_offset);
                                                                tab.markdown_selection.end = Some(line_end);
                                                                tab.markdown_selection.finish();
                                                            } else {
                                                                tab.markdown_selection.start = Some(start_offset);
                                                                tab.markdown_selection.end = Some(start_offset);
                                                                tab.markdown_selection.is_selecting = true;
                                                            }
                                                            cx.notify();
                                                        }))
                                                        .on_mouse_move(cx.listener(move |this, _event: &MouseMoveEvent, _window, cx| {
                                                            let tab = this.active_tab_mut();
                                                            if tab.markdown_selection.is_selecting
                                                                && let Some(sel_start) = tab.markdown_selection.start {
                                                                    if start_offset >= sel_start {
                                                                        tab.markdown_selection.end = Some(line_end);
                                                                    } else {
                                                                        tab.markdown_selection.end = Some(start_offset);
                                                                    }
                                                                    cx.notify();
                                                                }
                                                        }))
                                                        .on_mouse_up(MouseButton::Left, cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                                                            this.active_tab_mut().markdown_selection.finish();
                                                            cx.notify();
                                                        }))
                                                        .child(line_div)
                                                        .into_any_element()
                                                    },
                                                )
                                                .collect();

                                            let code_block =
                                                code_block_container(language.as_deref(), &t, cx)
                                                    .id(ElementId::Name(
                                                        format!("md-codeblock-{}", idx).into(),
                                                    ))
                                                    .child(
                                                        div()
                                                            .p(px(12.0))
                                                            .font_family("monospace")
                                                            .text_size(ui_text(
                                                                self.file_font_size,
                                                                cx,
                                                            ))
                                                            .text_color(rgb(t.text_secondary))
                                                            .flex()
                                                            .flex_col()
                                                            .children(line_children),
                                                    );

                                            content_children.push(code_block.into_any_element());
                                            node_idx += 1;
                                        }
                                        RenderedNode::Table { header, rows } => {
                                            let idx = node_idx;
                                            let mut table_rows: Vec<AnyElement> = Vec::new();

                                            if let Some((header_div, start_offset, end_offset)) =
                                                header
                                            {
                                                let row_end = end_offset.saturating_sub(1);
                                                table_rows.push(
                                                    div()
                                                        .id(ElementId::Name(format!("md-table-{}-header", idx).into()))
                                                        .on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                                                            let tab = this.active_tab_mut();
                                                            if event.click_count == 2 {
                                                                tab.markdown_selection.start = Some(start_offset);
                                                                tab.markdown_selection.end = Some(row_end);
                                                                tab.markdown_selection.finish();
                                                            } else {
                                                                tab.markdown_selection.start = Some(start_offset);
                                                                tab.markdown_selection.end = Some(start_offset);
                                                                tab.markdown_selection.is_selecting = true;
                                                            }
                                                            cx.notify();
                                                        }))
                                                        .on_mouse_move(cx.listener(move |this, _event: &MouseMoveEvent, _window, cx| {
                                                            let tab = this.active_tab_mut();
                                                            if tab.markdown_selection.is_selecting
                                                                && let Some(sel_start) = tab.markdown_selection.start {
                                                                    if start_offset >= sel_start {
                                                                        tab.markdown_selection.end = Some(row_end);
                                                                    } else {
                                                                        tab.markdown_selection.end = Some(start_offset);
                                                                    }
                                                                    cx.notify();
                                                                }
                                                        }))
                                                        .on_mouse_up(MouseButton::Left, cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                                                            this.active_tab_mut().markdown_selection.finish();
                                                            cx.notify();
                                                        }))
                                                        .child(header_div)
                                                        .into_any_element()
                                                );
                                            }

                                            for (row_idx, (row_div, start_offset, end_offset)) in
                                                rows.into_iter().enumerate()
                                            {
                                                let row_end = end_offset.saturating_sub(1);
                                                table_rows.push(
                                                    div()
                                                        .id(ElementId::Name(format!("md-table-{}-row-{}", idx, row_idx).into()))
                                                        .on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                                                            let tab = this.active_tab_mut();
                                                            if event.click_count == 2 {
                                                                tab.markdown_selection.start = Some(start_offset);
                                                                tab.markdown_selection.end = Some(row_end);
                                                                tab.markdown_selection.finish();
                                                            } else {
                                                                tab.markdown_selection.start = Some(start_offset);
                                                                tab.markdown_selection.end = Some(start_offset);
                                                                tab.markdown_selection.is_selecting = true;
                                                            }
                                                            cx.notify();
                                                        }))
                                                        .on_mouse_move(cx.listener(move |this, _event: &MouseMoveEvent, _window, cx| {
                                                            let tab = this.active_tab_mut();
                                                            if tab.markdown_selection.is_selecting
                                                                && let Some(sel_start) = tab.markdown_selection.start {
                                                                    if start_offset >= sel_start {
                                                                        tab.markdown_selection.end = Some(row_end);
                                                                    } else {
                                                                        tab.markdown_selection.end = Some(start_offset);
                                                                    }
                                                                    cx.notify();
                                                                }
                                                        }))
                                                        .on_mouse_up(MouseButton::Left, cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                                                            this.active_tab_mut().markdown_selection.finish();
                                                            cx.notify();
                                                        }))
                                                        .child(row_div)
                                                        .into_any_element()
                                                );
                                            }

                                            let table = div()
                                                .id(ElementId::Name(
                                                    format!("md-table-{}", idx).into(),
                                                ))
                                                .flex()
                                                .flex_col()
                                                .rounded(px(4.0))
                                                .border_1()
                                                .border_color(rgb(t.border))
                                                .overflow_hidden()
                                                .children(table_rows);

                                            content_children.push(table.into_any_element());
                                            node_idx += 1;
                                        }
                                    }
                                }

                                let content_div = v_flex()
                                    .gap(px(12.0))
                                    .p(px(16.0))
                                    .max_w(px(900.0))
                                    .children(content_children);

                                d.child(
                                    div().flex_1().min_h_0().relative().child(div()
                                        .id("markdown-preview").on_mouse_down(MouseButton::Right, cx.listener(Self::show_editor_context_menu))
                                        .size_full()
                                        .overflow_y_scroll()
                                        .overflow_x_scroll()
                                        .track_scroll(
                                            &self.active_tab().markdown_scroll_handle,
                                        )
                                        .bg(rgb(t.bg_secondary))
                                        .cursor(CursorStyle::IBeam)
                                        .on_mouse_up(
                                            MouseButton::Left,
                                            cx.listener(
                                                |this, _event: &MouseUpEvent, _window, cx| {
                                                    this.active_tab_mut()
                                                        .markdown_selection
                                                        .finish();
                                                    cx.notify();
                                                },
                                            ),
                                        )
                                        .child(content_div)).child(div().absolute().inset_0().child(Scrollbar::vertical(&self.active_tab().markdown_scroll_handle).scrollbar_show(ScrollbarShow::Always))),
                                )
                            }),
                    ),
            )
            // Filter popover backdrop + overlay (at fullscreen overlay level)
            .when(self.filter_popover_open, |d| {
                d.child(
                    div()
                        .id("fv-filter-popover-backdrop")
                        .absolute()
                        .inset_0()
                        .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                            this.filter_popover_open = false;
                            cx.notify();
                        }))
                )
            })
            .when(self.filter_popover_open && self.filter_button_bounds.is_some(), |d| {
                let bounds = self.filter_button_bounds.expect("guarded by is_some() in when()");
                let entity = cx.entity().downgrade();
                d.child(crate::list_overlay::file_filter_popover(
                    bounds, self.show_ignored, self.show_hidden, &t, cx,
                    move |filter, _, cx| {
                        if let Some(e) = entity.upgrade() {
                            e.update(cx, |this, cx| this.toggle_filter(filter, cx));
                        }
                    },
                ))
            })
            .children(self.render_editor_context_menu(&t, cx)).when_some(self.render_context_menu(&t, cx), |d, menu| d.child(menu))
            .when_some(self.render_tab_context_menu(&t, cx), |d, menu| d.child(menu))
            .when_some(self.render_delete_confirm(&t, cx), |d, dialog| d.child(dialog))
    }
}
