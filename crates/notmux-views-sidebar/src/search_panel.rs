use gpui::prelude::*;
use gpui::*;
use std::sync::Arc;
use notmux_files::code_view::{
    ScrollbarDrag, get_scrollbar_geometry, start_scrollbar_drag, update_scrollbar_drag,
};
use notmux_files::content_search::{ContentSearchConfig, FileSearchResult, SearchHandle, SearchMode};
use notmux_files::project_fs::ProjectFs;
use notmux_ui::empty_state::empty_state;
use notmux_ui::selectable_list::selectable_list_item;
use notmux_ui::simple_input::{InputChangedEvent, SimpleInput, SimpleInputState};
use notmux_ui::theme::theme;
use notmux_ui::tokens::{ui_text_ms, ui_text_sm};
use notmux_ui::vscode_icon::vscode_file_icon_sized_with_options;
use notmux_workspace::request_broker::RequestBroker;
use notmux_workspace::requests::OverlayRequest;

#[derive(Clone)]
enum SearchRow {
    File {
        relative_path: String,
        match_count: usize,
    },
    Match {
        relative_path: String,
        line_number: usize,
        line_content: String,
        match_ranges: Vec<std::ops::Range<usize>>,
    },
}

pub struct ContentSearchPanel {
    project_id: String,
    project_fs: Arc<dyn ProjectFs>,
    request_broker: Entity<RequestBroker>,
    focus_handle: FocusHandle,
    search_input: Entity<SimpleInputState>,
    scroll_handle: UniformListScrollHandle,
    scrollbar_drag: Option<ScrollbarDrag>,
    monochrome_icons: bool,
    rows: Vec<SearchRow>,
    selected_index: usize,
    total_matches: usize,
    searching: bool,
    search_handle: Option<SearchHandle>,
    debounce_task: Option<Task<()>>,
    search_generation: u64,
    case_sensitive: bool,
    regex_mode: bool,
    fuzzy_mode: bool,
}

impl ContentSearchPanel {
    pub fn new(
        project_id: String,
        project_fs: Arc<dyn ProjectFs>,
        request_broker: Entity<RequestBroker>,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_input = cx.new(|cx| {
            SimpleInputState::new(cx)
                .placeholder("Search file contents...")
                .icon("icons/search.svg")
        });

        cx.subscribe(
            &search_input,
            |this: &mut Self, _, _: &InputChangedEvent, cx| {
                this.trigger_search(cx);
            },
        )
        .detach();

        Self {
            project_id,
            project_fs,
            request_broker,
            focus_handle: cx.focus_handle(),
            search_input,
            scroll_handle: UniformListScrollHandle::new(),
            scrollbar_drag: None,
            monochrome_icons: false,
            rows: Vec::new(),
            selected_index: 0,
            total_matches: 0,
            searching: false,
            search_handle: None,
            debounce_task: None,
            search_generation: 0,
            case_sensitive: false,
            regex_mode: false,
            fuzzy_mode: false,
        }
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    pub fn is_searching(&self) -> bool {
        self.searching
    }

    pub fn is_case_sensitive(&self) -> bool {
        self.case_sensitive
    }

    pub fn is_regex_mode(&self) -> bool {
        self.regex_mode
    }

    pub fn is_fuzzy_mode(&self) -> bool {
        self.fuzzy_mode
    }

    pub fn set_monochrome_icons(&mut self, monochrome_icons: bool, cx: &mut Context<Self>) {
        if self.monochrome_icons == monochrome_icons {
            return;
        }
        self.monochrome_icons = monochrome_icons;
        cx.notify();
    }

    pub fn toggle_case_sensitive(&mut self, cx: &mut Context<Self>) {
        self.case_sensitive = !self.case_sensitive;
        self.trigger_search(cx);
        cx.notify();
    }

    pub fn toggle_regex_mode(&mut self, cx: &mut Context<Self>) {
        self.regex_mode = !self.regex_mode;
        if self.regex_mode {
            self.fuzzy_mode = false;
        }
        self.trigger_search(cx);
        cx.notify();
    }

    pub fn toggle_fuzzy_mode(&mut self, cx: &mut Context<Self>) {
        self.fuzzy_mode = !self.fuzzy_mode;
        if self.fuzzy_mode {
            self.regex_mode = false;
        }
        self.trigger_search(cx);
        cx.notify();
    }

    fn trigger_search(&mut self, cx: &mut Context<Self>) {
        self.search_generation = self.search_generation.wrapping_add(1);
        let generation = self.search_generation;

        if let Some(handle) = self.search_handle.take() {
            handle.cancel();
        }

        let query = self.search_input.read(cx).value().to_string();
        if query.is_empty() {
            self.rows.clear();
            self.total_matches = 0;
            self.searching = false;
            self.selected_index = 0;
            cx.notify();
            return;
        }

        self.debounce_task = Some(cx.spawn(
            async move |this: WeakEntity<ContentSearchPanel>, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(200))
                    .await;
                this.update(cx, |this, cx| {
                    if this.search_generation == generation {
                        this.run_search(generation, cx);
                    }
                })
                .ok();
            },
        ));
    }

    fn run_search(&mut self, generation: u64, cx: &mut Context<Self>) {
        let query = self.search_input.read(cx).value().to_string();
        if query.is_empty() {
            return;
        }

        let handle = SearchHandle::new();
        self.search_handle = Some(handle.clone());
        self.searching = true;
        cx.notify();

        let mode = if self.fuzzy_mode {
            SearchMode::Fuzzy
        } else if self.regex_mode {
            SearchMode::Regex
        } else {
            SearchMode::Literal
        };

        let config = ContentSearchConfig {
            case_sensitive: self.case_sensitive,
            mode,
            max_results: 1000,
            file_glob: None,
            context_lines: 0,
            show_ignored: false,
            show_hidden: false,
        };

        let project_fs = self.project_fs.clone();
        let cancelled = handle.flag();

        cx.spawn(async move |entity: WeakEntity<ContentSearchPanel>, cx| {
            let results = cx
                .background_executor()
                .spawn(async move {
                    let mut results: Vec<FileSearchResult> = Vec::new();
                    project_fs.search_content(&query, &config, &cancelled, &mut |result| {
                        results.push(result);
                    });
                    results.sort_by_key(|b| std::cmp::Reverse(b.best_score));
                    results
                })
                .await;

            entity
                .update(cx, |this, cx| {
                    if this.search_generation == generation
                        && this
                            .search_handle
                            .as_ref()
                            .is_some_and(|h| !h.is_cancelled())
                    {
                        this.apply_results(results);
                        this.searching = false;
                        cx.notify();
                    }
                })
                .ok();
        })
        .detach();
    }

    fn apply_results(&mut self, results: Vec<FileSearchResult>) {
        self.rows.clear();
        self.total_matches = 0;

        for file_result in results {
            self.rows.push(SearchRow::File {
                relative_path: file_result.relative_path.clone(),
                match_count: file_result.matches.len(),
            });

            for m in file_result.matches {
                self.total_matches += 1;
                self.rows.push(SearchRow::Match {
                    relative_path: file_result.relative_path.clone(),
                    line_number: m.line_number,
                    line_content: m.line_content,
                    match_ranges: m.match_ranges,
                });
            }
        }

        self.selected_index = if self.rows.is_empty() {
            0
        } else {
            1.min(self.rows.len() - 1)
        };
    }

    fn select_prev(&mut self) {
        if self.rows.is_empty() || self.selected_index == 0 {
            return;
        }
        self.selected_index -= 1;
        self.scroll_handle
            .scroll_to_item(self.selected_index, ScrollStrategy::Nearest);
    }

    fn select_next(&mut self) {
        if self.selected_index + 1 >= self.rows.len() {
            return;
        }
        self.selected_index += 1;
        self.scroll_handle
            .scroll_to_item(self.selected_index, ScrollStrategy::Nearest);
    }

    fn open_selected(&self, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(self.selected_index) else {
            return;
        };

        let file = match row {
            SearchRow::File { relative_path, .. } | SearchRow::Match { relative_path, .. } => {
                relative_path.clone()
            }
        };

        self.request_broker.update(cx, |broker, cx| {
            broker.push_overlay_request(
                OverlayRequest::MainFileViewer {
                    project_id: self.project_id.clone(),
                    file,
                },
                cx,
            );
        });
    }

    fn start_scrollbar_drag(&mut self, y: f32, cx: &mut Context<Self>) {
        let mut drag = start_scrollbar_drag(&self.scroll_handle);
        drag.start_y = y;
        self.scrollbar_drag = Some(drag);
        cx.notify();
    }

    fn update_scrollbar_drag(&mut self, y: f32, cx: &mut Context<Self>) {
        let Some(drag) = self.scrollbar_drag else {
            return;
        };
        update_scrollbar_drag(&self.scroll_handle, drag, y);
        cx.notify();
    }

    fn end_scrollbar_drag(&mut self, cx: &mut Context<Self>) {
        self.scrollbar_drag = None;
        cx.notify();
    }

    fn render_scrollbar(
        &self,
        thumb_y: f32,
        thumb_height: f32,
        is_dragging: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let t = theme(cx);

        div()
            .id("content-search-scrollbar-track")
            .absolute()
            .top_0()
            .bottom_0()
            .right_0()
            .w(px(12.0))
            .cursor(CursorStyle::Arrow)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                    if get_scrollbar_geometry(&this.scroll_handle).is_some() {
                        this.start_scrollbar_drag(f32::from(event.position.y), cx);
                    }
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                if this.scrollbar_drag.is_some() {
                    this.update_scrollbar_drag(f32::from(event.position.y), cx);
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

    fn render_file_row(
        &self,
        idx: usize,
        relative_path: &str,
        match_count: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx);
        let filename = std::path::Path::new(relative_path)
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| relative_path.to_string());

        selectable_list_item(
            ElementId::Name(format!("content-search-file-{idx}").into()),
            idx == self.selected_index,
            &t,
        )
        .h(px(30.0))
        .py(px(0.0))
        .pr(px(18.0))
        .gap(px(8.0))
        .w_full()
        .overflow_hidden()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, _window, cx| {
                this.selected_index = idx;
                this.open_selected(cx);
            }),
        )
        .child(vscode_file_icon_sized_with_options(
            &filename,
            px(18.0),
            &t,
            self.monochrome_icons,
            cx,
        ))
        .child(
            div()
                .flex_1()
                .flex()
                .items_center()
                .gap(px(8.0))
                .min_w_0()
                .overflow_hidden()
                .child(
                    div()
                        .text_size(ui_text_ms(cx))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(t.text_primary))
                        .truncate()
                        .child(relative_path.to_string()),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(ui_text_sm(cx))
                        .text_color(rgb(t.text_muted))
                        .child(format!(
                            "{} match{}",
                            match_count,
                            if match_count == 1 { "" } else { "es" }
                        )),
                ),
        )
        .into_any_element()
    }

    fn render_match_row(
        &self,
        idx: usize,
        line_number: usize,
        line_content: &str,
        match_ranges: &[std::ops::Range<usize>],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx);
        let highlights = match_ranges
            .iter()
            .filter(|range| range.end <= line_content.len())
            .map(|range| {
                (
                    range.clone(),
                    HighlightStyle {
                        background_color: Some(rgb(t.search_match_bg).into()),
                        ..Default::default()
                    },
                )
            })
            .collect::<Vec<_>>();

        selectable_list_item(
            ElementId::Name(format!("content-search-match-{idx}").into()),
            idx == self.selected_index,
            &t,
        )
        .h(px(30.0))
        .py(px(0.0))
        .pr(px(18.0))
        .pl(px(28.0))
        .gap(px(8.0))
        .w_full()
        .overflow_hidden()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                this.selected_index = idx;
                if event.click_count >= 2 {
                    this.open_selected(cx);
                }
                cx.notify();
            }),
        )
        .child(
            div()
                .text_size(ui_text_sm(cx))
                .text_color(rgb(t.text_muted))
                .min_w(px(34.0))
                .flex_shrink_0()
                .child(format!("{line_number:>4}")),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(ui_text_ms(cx))
                .font_family("monospace")
                .text_color(rgb(t.text_primary))
                .child(StyledText::new(line_content.to_string()).with_highlights(highlights)),
        )
        .into_any_element()
    }
}

impl Render for ContentSearchPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let input_focus = self.search_input.read(cx).focus_handle(cx);
        if !input_focus.is_focused(window) {
            self.search_input
                .update(cx, |input, cx| input.focus(window, cx));
        }

        let rows = self.rows.clone();
        let file_count = rows
            .iter()
            .filter(|row| matches!(row, SearchRow::File { .. }))
            .count();
        let view = cx.entity().clone();
        let scrollbar_geometry = get_scrollbar_geometry(&self.scroll_handle);
        let needs_scrollbar_measurement =
            !rows.is_empty() && self.scroll_handle.0.borrow().last_item_size.is_none();
        if needs_scrollbar_measurement {
            cx.notify();
        }

        div()
            .track_focus(&self.focus_handle)
            .key_context("ContentSearchPanel")
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                match event.keystroke.key.as_str() {
                    "up" => {
                        this.select_prev();
                        cx.notify();
                    }
                    "down" => {
                        this.select_next();
                        cx.notify();
                    }
                    "enter" => this.open_selected(cx),
                    _ => {}
                }
            }))
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(rgb(t.bg_secondary))
            .child(
                div()
                    .px(px(10.0))
                    .py(px(8.0))
                    .border_b_1()
                    .border_color(rgb(t.border))
                    .child(SimpleInput::new(&self.search_input).text_size(ui_text_ms(cx))),
            )
            .child(if rows.is_empty() {
                div()
                    .flex_1()
                    .child(empty_state(
                        if self.searching {
                            "Searching..."
                        } else if self.search_input.read(cx).value().is_empty() {
                            "Type to search file contents"
                        } else {
                            "No matching results"
                        },
                        &t,
                        cx,
                    ))
                    .into_any_element()
            } else {
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .h(px(28.0))
                            .px(px(12.0))
                            .flex()
                            .items_center()
                            .border_b_1()
                            .border_color(rgb(t.border))
                            .text_size(ui_text_ms(cx))
                            .text_color(rgb(t.text_secondary))
                            .child(format!(
                                "{} result{} in {} file{}",
                                self.total_matches,
                                if self.total_matches == 1 { "" } else { "s" },
                                file_count,
                                if file_count == 1 { "" } else { "s" },
                            )),
                    )
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .size_full()
                            .child(
                                uniform_list(
                                    "content-search-panel-list",
                                    rows.len(),
                                    move |range, _window, cx| {
                                        view.update(cx, |this, cx| {
                                            range
                                                .map(|idx| match &rows[idx] {
                                                    SearchRow::File {
                                                        relative_path,
                                                        match_count,
                                                    } => this.render_file_row(
                                                        idx,
                                                        relative_path,
                                                        *match_count,
                                                        cx,
                                                    ),
                                                    SearchRow::Match {
                                                        line_number,
                                                        line_content,
                                                        match_ranges,
                                                        ..
                                                    } => this.render_match_row(
                                                        idx,
                                                        *line_number,
                                                        line_content,
                                                        match_ranges,
                                                        cx,
                                                    ),
                                                })
                                                .collect()
                                        })
                                    },
                                )
                                .size_full()
                                .h_full()
                                .map(|mut list| {
                                    list.style().restrict_scroll_to_axis = Some(true);
                                    list
                                })
                                .track_scroll(&self.scroll_handle),
                            )
                            .when(scrollbar_geometry.is_some(), |d| {
                                let (_, _, thumb_y, thumb_height) =
                                    scrollbar_geometry.expect("guarded by is_some() in when()");
                                d.child(self.render_scrollbar(
                                    thumb_y,
                                    thumb_height,
                                    self.scrollbar_drag.is_some(),
                                    cx,
                                ))
                            }),
                    )
                    .into_any_element()
            })
    }
}
