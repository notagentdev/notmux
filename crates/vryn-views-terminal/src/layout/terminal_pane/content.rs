//! Terminal content component.

use crate::elements::terminal_element::{LinkKind, SearchMatch, TerminalElement};
use crate::layout::navigation::register_pane_bounds;
use crate::terminal_view_settings;
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use vryn_files::theme::theme;
use vryn_terminal::command_tracking::{CommandStatus, TrackedCommand};
use vryn_terminal::terminal::Terminal;
use vryn_ui::color_utils::tint_color;
use vryn_workspace::state::Workspace;

use super::scrollbar::Scrollbar;
use super::url_detector::UrlDetector;

/// Events emitted by terminal content.
pub enum TerminalContentEvent {
    RequestContextMenu {
        position: Point<Pixels>,
        has_selection: bool,
        link_url: Option<String>,
    },
}

/// Terminal content view handling display and mouse interactions.
pub struct TerminalContent {
    terminal: Option<Arc<Terminal>>,
    focus_handle: FocusHandle,
    url_detector: UrlDetector,
    scrollbar: Entity<Scrollbar>,
    is_selecting: bool,
    element_bounds: Option<Bounds<Pixels>>,
    last_click: Option<(Instant, usize, i32)>,
    click_count: u8,
    cursor_visible: bool,
    search_matches: Arc<Vec<SearchMatch>>,
    search_current_index: Option<usize>,
    project_id: String,
    layout_path: Vec<usize>,
    workspace: Entity<Workspace>,
    scroll_accumulator: f32,
    mouse_down_cell: Option<(usize, i32)>,
    forwarded_button: Option<(u8, u8)>,
    hovered_command_id: Option<u64>,
    hover_position: Option<Point<Pixels>>,
}

impl TerminalContent {
    pub fn new(
        focus_handle: FocusHandle,
        project_id: String,
        layout_path: Vec<usize>,
        workspace: Entity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        let scrollbar = cx.new(Scrollbar::new);

        Self {
            terminal: None,
            focus_handle,
            url_detector: UrlDetector::new(),
            scrollbar,
            is_selecting: false,
            element_bounds: None,
            last_click: None,
            click_count: 0,
            cursor_visible: true,
            search_matches: Arc::new(Vec::new()),
            search_current_index: None,
            project_id,
            layout_path,
            workspace,
            scroll_accumulator: 0.0,
            mouse_down_cell: None,
            forwarded_button: None,
            hovered_command_id: None,
            hover_position: None,
        }
    }

    fn mouse_modifier_bits(m: &Modifiers) -> u8 {
        let mut bits = 0u8;
        if m.shift {
            bits |= 4;
        }
        if m.alt {
            bits |= 8;
        }
        if m.control {
            bits |= 16;
        }
        bits
    }

    pub fn set_terminal(&mut self, terminal: Option<Arc<Terminal>>, cx: &mut Context<Self>) {
        self.terminal = terminal.clone();
        self.scrollbar.update(cx, |scrollbar, _| {
            scrollbar.set_terminal(terminal);
        });
    }

    pub fn set_cursor_visible(&mut self, visible: bool) {
        self.cursor_visible = visible;
    }

    pub fn set_search_highlights(
        &mut self,
        matches: Arc<Vec<SearchMatch>>,
        current_index: Option<usize>,
    ) {
        self.search_matches = matches;
        self.search_current_index = current_index;
    }

    pub fn mark_scroll_activity(&mut self, cx: &mut Context<Self>) {
        self.scrollbar.update(cx, |scrollbar, _| {
            scrollbar.mark_activity();
        });
    }

    pub fn handle_scroll(&mut self, delta: f32, position: Point<Pixels>, cx: &mut Context<Self>) {
        if let Some(ref terminal) = self.terminal {
            let (cell_width, cell_height) = terminal.cell_dimensions();

            if terminal.is_mouse_mode() {
                self.scroll_accumulator += delta;
                let lines = (self.scroll_accumulator / cell_height) as i32;
                if lines != 0 {
                    self.scroll_accumulator -= lines as f32 * cell_height;
                    let (col, row) = self.pixel_to_cell_raw(position, cell_width, cell_height);
                    let button = if lines > 0 { 64u8 } else { 65u8 };
                    terminal.send_mouse_scroll(button, col, row, lines.unsigned_abs() as usize);
                }
            } else {
                self.scroll_accumulator += delta;
                let lines = (self.scroll_accumulator / cell_height) as i32;
                if lines != 0 {
                    self.scroll_accumulator -= lines as f32 * cell_height;
                    if lines > 0 {
                        terminal.scroll_up(lines);
                    } else {
                        terminal.scroll_down(-lines);
                    }
                }
            }
            self.mark_scroll_activity(cx);
            cx.notify();
        }
    }

    pub fn update_scrollbar_drag(&mut self, y: f32, cx: &mut Context<Self>) {
        if let Some(bounds) = self.element_bounds {
            let content_height = f32::from(bounds.size.height);
            self.scrollbar.update(cx, |scrollbar, cx| {
                scrollbar.update_drag(y, content_height, cx);
            });
        }
    }

    pub fn end_scrollbar_drag(&mut self, cx: &mut Context<Self>) {
        self.scrollbar.update(cx, |scrollbar, cx| {
            scrollbar.end_drag(cx);
        });
    }

    const TERMINAL_PADDING: f32 = 4.0;
    const TERMINAL_LEFT_PADDING: f32 = 22.0;

    fn pixel_to_cell(
        &self,
        pos: Point<Pixels>,
    ) -> Option<(usize, i32, alacritty_terminal::index::Side)> {
        let bounds = self.element_bounds?;
        let terminal = self.terminal.as_ref()?;
        let (cell_width, cell_height) = terminal.cell_dimensions();

        let x = (f32::from(pos.x)
            - f32::from(bounds.origin.x)
            - Self::TERMINAL_LEFT_PADDING)
            .max(0.0);
        let y = (f32::from(pos.y) - f32::from(bounds.origin.y) - Self::TERMINAL_PADDING).max(0.0);

        let col_exact = x / cell_width;
        let col = col_exact.floor() as usize;
        let row = (y / cell_height).floor() as i32;

        let size = terminal.resize_state.lock();
        let col = col.min(size.size.cols.saturating_sub(1) as usize);
        let row = row.min(size.size.rows.saturating_sub(1) as i32);

        let side = if col_exact.fract() < 0.5 {
            alacritty_terminal::index::Side::Left
        } else {
            alacritty_terminal::index::Side::Right
        };

        Some((col, row, side))
    }

    fn pixel_to_cell_raw(
        &self,
        pos: Point<Pixels>,
        cell_width: f32,
        cell_height: f32,
    ) -> (usize, usize) {
        if let Some(bounds) = self.element_bounds {
            let x = (f32::from(pos.x)
                - f32::from(bounds.origin.x)
                - Self::TERMINAL_LEFT_PADDING)
                .max(0.0);
            let y = (f32::from(pos.y) - f32::from(bounds.origin.y) - Self::TERMINAL_PADDING)
                .max(0.0);
            ((x / cell_width) as usize, (y / cell_height) as usize)
        } else {
            (0, 0)
        }
    }

    fn update_command_hover(&mut self, position: Point<Pixels>) -> bool {
        let Some(terminal) = self.terminal.as_ref() else {
            return self.clear_command_hover();
        };
        if terminal.is_alt_screen() {
            return self.clear_command_hover();
        }
        let Some(bounds) = self.element_bounds else {
            return self.clear_command_hover();
        };

        let (_cell_width, cell_height) = terminal.cell_dimensions();
        let rel_x = f32::from(position.x) - f32::from(bounds.origin.x);
        let rel_y = f32::from(position.y) - f32::from(bounds.origin.y);
        let icon_left = Self::TERMINAL_LEFT_PADDING - 17.0;
        let icon_right = icon_left + 16.0;
        if rel_x < icon_left || rel_x > icon_right || rel_y < Self::TERMINAL_PADDING {
            return self.clear_command_hover();
        }

        let display_offset = terminal.display_offset() as i32;
        let row = ((rel_y - Self::TERMINAL_PADDING) / cell_height).floor() as i32;
        let hovered = terminal
            .command_history()
            .into_iter()
            .find(|command| command.command_row + display_offset == row)
            .and_then(|command| {
                if matches!(
                    command.status,
                    CommandStatus::Running | CommandStatus::Unknown
                ) {
                    None
                } else {
                    Some(command.id)
                }
            });

        let changed = self.hovered_command_id != hovered || self.hover_position != Some(position);
        self.hovered_command_id = hovered;
        self.hover_position = hovered.map(|_| position);
        changed
    }

    fn clear_command_hover(&mut self) -> bool {
        let changed = self.hovered_command_id.is_some() || self.hover_position.is_some();
        self.hovered_command_id = None;
        self.hover_position = None;
        changed
    }

    fn handle_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);

        if let Some(ref terminal) = self.terminal
            && let Some((col, row, side)) = self.pixel_to_cell(event.position)
        {
            self.mouse_down_cell = Some((col, row));

            if (event.modifiers.platform || event.modifiers.control)
                && let Some(url_match) = self.url_detector.find_at(col, row)
            {
                match &url_match.kind {
                    LinkKind::Url => {
                        UrlDetector::open_url(&url_match.url);
                    }
                    LinkKind::FilePath { line, col } => {
                        let file_opener = terminal_view_settings(cx).file_opener.clone();
                        UrlDetector::open_file(&url_match.url, *line, *col, &file_opener);
                    }
                }
                self.mouse_down_cell = None;
                return;
            }

            if terminal.is_mouse_mode() {
                let mods = Self::mouse_modifier_bits(&event.modifiers);
                let button: u8 = 0; // left
                terminal.send_mouse_button(button, true, col, row as usize, mods);
                self.forwarded_button = Some((button, mods));
                self.mouse_down_cell = None;
                self.is_selecting = false;
                return;
            }

            let now = Instant::now();

            let click_count = if let Some((last_time, last_col, last_row)) = self.last_click {
                let elapsed = now.duration_since(last_time).as_millis();
                let same_position =
                    (col as i32 - last_col as i32).abs() <= 1 && (row - last_row).abs() <= 0;
                if elapsed < 400 && same_position {
                    if self.click_count >= 3 {
                        1
                    } else {
                        self.click_count + 1
                    }
                } else {
                    1
                }
            } else {
                1
            };

            self.last_click = Some((now, col, row));
            self.click_count = click_count;

            terminal.clear_selection();

            match click_count {
                2 => {
                    terminal.start_word_selection(col, row);
                    self.is_selecting = false;
                }
                3 => {
                    terminal.start_line_selection(col, row);
                    self.is_selecting = false;
                }
                _ => {
                    terminal.start_selection(col, row, side);
                    self.is_selecting = true;
                }
            }
            cx.notify();
        }
    }

    fn handle_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if let Some((col, row, _side)) = self.pixel_to_cell(event.position) {
            if self.update_command_hover(event.position) {
                cx.notify();
            }
            if self.url_detector.update_hover(col, row) {
                cx.notify();
            }
        } else if self.url_detector.clear_hover() {
            if self.clear_command_hover() {
                cx.notify();
            }
            cx.notify();
        }

        if let Some((button, mods)) = self.forwarded_button {
            if let Some(ref terminal) = self.terminal
                && terminal.supports_mouse_drag()
                && let Some((col, row, _side)) = self.pixel_to_cell(event.position)
            {
                terminal.send_mouse_drag(button, col, row as usize, mods);
            }
            return;
        }

        if self.is_selecting {
            if event.pressed_button != Some(MouseButton::Left) {
                if let Some(ref terminal) = self.terminal {
                    terminal.end_selection();
                    if !terminal.has_selection()
                        || terminal
                            .get_selected_text()
                            .map(|s| s.is_empty())
                            .unwrap_or(true)
                    {
                        terminal.clear_selection();
                    }
                }
                self.is_selecting = false;
                cx.notify();
                return;
            }

            if let Some(ref terminal) = self.terminal
                && let Some((col, row, side)) = self.pixel_to_cell(event.position)
            {
                terminal.update_selection(col, row, side);
                cx.notify();
            }
        }
    }

    fn handle_mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        if let Some((button, _)) = self.forwarded_button.take() {
            if let Some(ref terminal) = self.terminal {
                let mods = Self::mouse_modifier_bits(&event.modifiers);
                if let Some((col, row, _side)) = self.pixel_to_cell(event.position) {
                    terminal.send_mouse_button(button, false, col, row as usize, mods);
                }
            }
            self.mouse_down_cell = None;
            return;
        }

        if self.is_selecting
            && let Some(ref terminal) = self.terminal
        {
            terminal.end_selection();
            self.is_selecting = false;

            let empty_selection = !terminal.has_selection()
                || terminal
                    .get_selected_text()
                    .map(|s| s.is_empty())
                    .unwrap_or(true);

            if empty_selection {
                terminal.clear_selection();

                // Click-to-cursor: on a clean single click (no drag), move cursor
                if self.click_count == 1
                    && let Some((col, row)) = self.mouse_down_cell.take()
                    && !terminal.is_mouse_mode()
                    && !terminal.is_alt_screen()
                    && !terminal.has_running_child()
                {
                    terminal.move_cursor_to_click(col, row);
                }
            }
            cx.notify();
        }
        self.mouse_down_cell = None;
    }
}

impl Render for TerminalContent {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let base_bg = if self.focus_handle.is_focused(window) {
            t.term_background
        } else {
            t.term_background_unfocused
        };

        let settings = crate::terminal_view_settings(cx);
        let bg_tint = if settings.color_tinted_background {
            let ws = self.workspace.read(cx);
            ws.project(&self.project_id).and_then(|p| {
                let color = ws.effective_folder_color(p);
                if color != vryn_core::theme::FolderColor::Default {
                    Some(t.get_folder_color(color))
                } else {
                    None
                }
            })
        } else {
            None
        };
        let term_bg = match bg_tint {
            Some(tint) => tint_color(base_bg, tint, 0.025),
            None => base_bg,
        };

        self.url_detector.update_matches(&self.terminal);

        let Some(ref terminal) = self.terminal else {
            return div()
                .flex_1()
                .min_h(px(200.0))
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(t.text_muted))
                .child("Creating terminal...")
                .into_any_element();
        };

        let terminal_clone = terminal.clone();
        let hovered_command = if terminal.is_alt_screen() {
            None
        } else {
            self.hovered_command_id.and_then(|id| {
                terminal
                    .command_history()
                    .into_iter()
                    .find(|command| command.id == id)
            })
        };
        let focus_handle = self.focus_handle.clone();
        let zoom_level = self
            .workspace
            .read(cx)
            .get_terminal_zoom(&self.project_id, &self.layout_path);

        let element_bounds_setter = {
            let entity = cx.entity().downgrade();
            let project_id = self.project_id.clone();
            let layout_path = self.layout_path.clone();
            let fh = self.focus_handle.clone();
            move |bounds: Bounds<Pixels>, _window: &mut Window, cx: &mut App| {
                register_pane_bounds(
                    project_id.clone(),
                    layout_path.clone(),
                    bounds,
                    Some(fh.clone()),
                );

                if let Some(entity) = entity.upgrade() {
                    entity.update(cx, |this, _| {
                        this.element_bounds = Some(bounds);
                    });
                }
            }
        };

        let render_settings = crate::terminal_view_settings(cx);

        div()
            .id("terminal-content")
            .size_full()
            .min_h_0()
            .relative()
            .bg(rgb(t.bg_primary))
            .cursor(CursorStyle::Arrow)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    if this.scrollbar.read(cx).is_dragging() {
                        this.end_scrollbar_drag(cx);
                        return;
                    }
                    this.handle_mouse_down(event, window, cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                if this.scrollbar.read(cx).is_dragging() {
                    this.update_scrollbar_drag(f32::from(event.position.y), cx);
                    return;
                }
                this.handle_mouse_move(event, cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, _window, cx| {
                    if this.scrollbar.read(cx).is_dragging() {
                        this.end_scrollbar_drag(cx);
                        return;
                    }
                    this.handle_mouse_up(event, cx);
                }),
            )
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _window, cx| {
                if event.modifiers.shift {
                    return;
                }
                let delta = event.delta.pixel_delta(px(17.0));
                if event.modifiers.control {
                    let current_zoom = this
                        .workspace
                        .read(cx)
                        .get_terminal_zoom(&this.project_id, &this.layout_path);
                    let zoom_delta = if f32::from(delta.y) > 0.0 { 0.1 } else { -0.1 };
                    let new_zoom = (current_zoom + zoom_delta).clamp(0.5, 3.0);
                    let project_id = this.project_id.clone();
                    let layout_path = this.layout_path.clone();
                    this.workspace.update(cx, |workspace, cx| {
                        workspace.set_terminal_zoom(&project_id, &layout_path, new_zoom, cx);
                    });
                } else {
                    this.handle_scroll(f32::from(delta.y), event.position, cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                    let has_selection = this
                        .terminal
                        .as_ref()
                        .map(|t| t.has_selection())
                        .unwrap_or(false);
                    let link_url =
                        this.pixel_to_cell(event.position)
                            .and_then(|(col, row, _side)| {
                                this.url_detector
                                    .find_at(col, row)
                                    .filter(|m| m.kind == LinkKind::Url)
                                    .map(|m| m.url)
                            });
                    cx.emit(TerminalContentEvent::RequestContextMenu {
                        position: event.position,
                        has_selection,
                        link_url,
                    });
                }),
            )
            .child(
                canvas(element_bounds_setter, |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .child(
                div()
                    .size_full()
                    .pt(px(Self::TERMINAL_PADDING))
                    .pr(px(Self::TERMINAL_PADDING))
                    .pb(px(Self::TERMINAL_PADDING))
                    .pl(px(Self::TERMINAL_LEFT_PADDING))
                    .bg(rgb(term_bg))
                    .child(
                        TerminalElement::new(terminal_clone, focus_handle)
                            .with_zoom(zoom_level)
                            .with_bg_tint(bg_tint)
                            .with_search(self.search_matches.clone(), self.search_current_index)
                            .with_urls(
                                self.url_detector.matches_arc(),
                                self.url_detector.hovered_group(),
                            )
                            .with_hovered_command(self.hovered_command_id)
                            .with_cursor_visible(self.cursor_visible)
                            .with_cursor_style(render_settings.cursor_style),
                    ),
            )
            .when_some(
                hovered_command.zip(self.hover_position),
                |el, (command, position)| {
                    el.child(command_hover(command, position, &t))
                },
            )
            .child(self.scrollbar.clone())
            .into_any_element()
    }
}

impl EventEmitter<TerminalContentEvent> for TerminalContent {}

fn command_hover(
    command: TrackedCommand,
    position: Point<Pixels>,
    t: &vryn_core::theme::ThemeColors,
) -> impl IntoElement {
    deferred(
        anchored()
            .position(point(position.x + px(12.0), position.y + px(12.0)))
            .snap_to_window()
            .child(
                div()
                    .id("terminal-command-hover")
                    .max_w(px(420.0))
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(t.border))
                    .bg(rgb(t.bg_secondary))
                    .shadow_md()
                    .text_size(px(12.0))
                    .text_color(rgb(t.text_primary))
                    .child(
                        div()
                            .font_weight(FontWeight::BOLD)
                            .child(command.command.clone()),
                    )
                    .child(
                        div()
                            .mt_1()
                            .text_color(rgb(t.text_muted))
                            .child(command_hover_meta(&command)),
                    ),
            ),
    )
}

fn command_hover_meta(command: &TrackedCommand) -> String {
    let status = match command.status {
        CommandStatus::Running => "running".to_string(),
        CommandStatus::Success => "success".to_string(),
        CommandStatus::Error => match command.exit_code {
            Some(code) => format!("failed ({code})"),
            None => "failed".to_string(),
        },
        CommandStatus::Unknown => "unknown".to_string(),
    };
    let duration = command
        .duration
        .map(format_duration)
        .unwrap_or_else(|| "running".to_string());
    let started = format_system_time(command.started_at);
    match command.cwd.as_deref() {
        Some(cwd) => format!("{status} · {duration} · {started} · {cwd}"),
        None => format!("{status} · {duration} · {started}"),
    }
}

fn format_duration(duration: Duration) -> String {
    let millis = duration.as_millis();
    if millis < 1_000 {
        format!("{millis} ms")
    } else {
        format!("{:.1} s", millis as f64 / 1_000.0)
    }
}

fn format_system_time(time: SystemTime) -> String {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => format!("started {}", duration.as_secs()),
        Err(_) => "started unknown".to_string(),
    }
}
