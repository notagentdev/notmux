use alacritty_terminal::event::{Event as TermEvent, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config as TermConfig, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor};
use parking_lot::Mutex;
use regex::Regex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

const REPLAY_BUFFER_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Transport trait for terminal I/O operations.
/// Implemented by PtyManager (local) and RemoteTransport (remote).
pub trait TerminalTransport: Send + Sync {
    fn send_input(&self, terminal_id: &str, data: &[u8]);
    fn resize(&self, terminal_id: &str, cols: u16, rows: u16);
    fn uses_mouse_backend(&self) -> bool;
    /// Debounce interval for transport resize calls (ms).
    /// Local PTY uses 16ms (just enough to batch rapid resizes).
    /// Remote uses longer interval to avoid flooding the network.
    fn resize_debounce_ms(&self) -> u64 {
        16
    }
}

/// Process-global resize authority. "Last to interact wins" across all terminals
/// in this process: whichever side most recently typed or clicked gets to drive
/// resize for every terminal. No time-based reclaim — the origin side takes over
/// by actually interacting, not by waiting.
///
/// Implemented with a monotonically-increasing sequence counter to avoid
/// timestamp collisions. Each claim bumps the counter and records the new value
/// on the claiming side. Higher value wins. Both zero (initial) resolves to
/// Local, so terminals behave normally before any interaction happens.
static RESIZE_AUTH_SEQ: AtomicU64 = AtomicU64::new(0);
static LAST_LOCAL_SEQ: AtomicU64 = AtomicU64::new(0);
static LAST_REMOTE_SEQ: AtomicU64 = AtomicU64::new(0);

pub fn claim_resize_authority_local() {
    let seq = RESIZE_AUTH_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    LAST_LOCAL_SEQ.store(seq, Ordering::Relaxed);
}

pub fn claim_resize_authority_remote() {
    let seq = RESIZE_AUTH_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    LAST_REMOTE_SEQ.store(seq, Ordering::Relaxed);
}

pub fn is_resize_authority_local() -> bool {
    LAST_LOCAL_SEQ.load(Ordering::Relaxed) >= LAST_REMOTE_SEQ.load(Ordering::Relaxed)
}

/// Resolver for OSC color queries (OSC 4/10/11/12). Returns packed 0xRRGGBB.
/// Index meaning matches alacritty: 0..=255 ANSI palette, 256 fg, 257 bg, 258 cursor.
/// Registered by the host app so the terminal can answer queries like `OSC 11 ; ? ST`
/// from apps that theme themselves to the terminal background.
static COLOR_RESOLVER: OnceLock<Arc<dyn Fn(usize) -> u32 + Send + Sync>> = OnceLock::new();

pub fn register_color_resolver(resolver: Arc<dyn Fn(usize) -> u32 + Send + Sync>) {
    let _ = COLOR_RESOLVER.set(resolver);
}

fn resolve_osc_color(index: usize) -> u32 {
    if let Some(resolver) = COLOR_RESOLVER.get() {
        return resolver(index);
    }
    match index {
        256 => 0xcccccc,
        257 => 0x1e1e1e,
        258 => 0xaeafad,
        _ => 0,
    }
}
#[derive(Clone, Debug)]
pub struct TerminalNotification {
    pub title: String,
    pub body: String,
    pub timestamp: Instant,
}
fn extract_osc_notifications(data: &[u8]) -> Vec<TerminalNotification> {
    let mut out = Vec::new();
    let bytes = data;
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] != 0x1b || bytes[i + 1] != b']' {
            i += 1;
            continue;
        }
        let start = i + 2;
        let mut num_end = start;
        while num_end < bytes.len() && bytes[num_end].is_ascii_digit() {
            num_end += 1;
        }
        if num_end == start || num_end >= bytes.len() {
            i += 1;
            continue;
        }
        let Ok(code) = std::str::from_utf8(&bytes[start..num_end]) else {
            i += 1;
            continue;
        };
        let code: u32 = match code.parse() {
            Ok(c) => c,
            Err(_) => {
             i += 1;
             continue;
            }
        };
        let mut content_start = num_end;
        if content_start < bytes.len() && bytes[content_start] == b';' {
            content_start += 1;
        }
        let mut end = content_start;
        while end < bytes.len() {
            if bytes[end] == 0x07 {
             break;
            }
            if bytes[end] == 0x1b
             && end + 1 < bytes.len()
             && bytes[end + 1] == b'\\'
            {
             break;
            }
            end += 1;
        }
        if end >= bytes.len() {
            i += 1;
            continue;
        }
        let raw = &bytes[content_start..end];
        let payload = String::from_utf8_lossy(raw).to_string();
        match code {
            9 if !payload.trim().is_empty() => {
             out.push(TerminalNotification {
                title: "Notification".to_string(),
                body: payload,
                timestamp: Instant::now(),
             });
            }
            99 => {
             let body = payload
                .split(';')
                .next_back()
                .unwrap_or("")
                .trim_matches(|c: char| !c.is_alphanumeric() && c != ' ');
             if !body.is_empty() {
                out.push(TerminalNotification {
                 title: "Notification".to_string(),
                 body: body.to_string(),
                 timestamp: Instant::now(),
                });
             }
            }
            777 => {
             let parts: Vec<&str> = payload.splitn(3, ';').collect();
             if parts.len() >= 2 && parts.first() == Some(&"notify") {
                let title = parts.get(1).copied().unwrap_or("").to_string();
                let body = parts.get(2).copied().unwrap_or("").to_string();
                out.push(TerminalNotification {
                 title,
                 body,
                 timestamp: Instant::now(),
                });
             }
            }
            _ => {}
        }
        i = end + 1;
    }
    out
}

#[cfg(test)]
fn reset_resize_authority() {
    RESIZE_AUTH_SEQ.store(0, Ordering::Relaxed);
    LAST_LOCAL_SEQ.store(0, Ordering::Relaxed);
    LAST_REMOTE_SEQ.store(0, Ordering::Relaxed);
}

/// Terminal size in cells and pixels
#[derive(Clone, Copy, Debug)]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
    pub cell_width: f32,
    pub cell_height: f32,
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self {
            cols: 80,
            rows: 24,
            cell_width: 8.0,
            cell_height: 16.0,
        }
    }
}

/// Event listener for alacritty_terminal that captures title changes, bell, and PTY write requests
pub struct ZedEventListener {
    /// Shared title storage - OSC 0/1/2 sequences update this
    title: Arc<Mutex<Option<String>>>,
    /// Bell notification flag
    has_bell: Arc<Mutex<bool>>,
    /// Transport for writing responses back to the terminal
    transport: Arc<dyn TerminalTransport>,
    /// Suppress emulator-generated replies while replaying persisted history.
    suppress_pty_responses: Arc<AtomicBool>,
    /// Terminal ID for PTY write operations
    terminal_id: String,
}

impl ZedEventListener {
    pub fn new(
        title: Arc<Mutex<Option<String>>>,
        has_bell: Arc<Mutex<bool>>,
        transport: Arc<dyn TerminalTransport>,
        suppress_pty_responses: Arc<AtomicBool>,
        terminal_id: String,
    ) -> Self {
        Self {
            title,
            has_bell,
            transport,
            suppress_pty_responses,
            terminal_id,
        }
    }
}

impl EventListener for ZedEventListener {
    fn send_event(&self, event: TermEvent) {
        match event {
            TermEvent::Title(title) => {
                *self.title.lock() = Some(title);
            }
            TermEvent::ResetTitle => {
                *self.title.lock() = None;
            }
            TermEvent::Bell => {
                // Ignore bells emitted while replaying persisted scrollback on
                // startup — that content is historical and must not light up the
                // notification ring. `suppress_pty_responses` is set for the
                // duration of replay's `advance()` (see `process_output_inner`).
                if !self.suppress_pty_responses.load(Ordering::Relaxed) {
                    *self.has_bell.lock() = true;
                }
            }
            TermEvent::PtyWrite(data) => {
                if self.suppress_pty_responses.load(Ordering::Relaxed) {
                    return;
                }
                // Write response back to PTY (e.g., cursor position report)
                log::debug!("PtyWrite event: {:?}", data);
                self.transport
                    .send_input(&self.terminal_id, data.as_bytes());
            }
            TermEvent::ColorRequest(index, format) => {
                if self.suppress_pty_responses.load(Ordering::Relaxed) {
                    return;
                }
                let hex = resolve_osc_color(index);
                let rgb = alacritty_terminal::vte::ansi::Rgb {
                    r: ((hex >> 16) & 0xFF) as u8,
                    g: ((hex >> 8) & 0xFF) as u8,
                    b: (hex & 0xFF) as u8,
                };
                let response = format(rgb);
                self.transport
                    .send_input(&self.terminal_id, response.as_bytes());
            }
            _ => {
                // Ignore other events
            }
        }
    }
}

/// Selection state for the terminal
#[derive(Clone, Debug, Default)]
pub struct SelectionState {
    pub start: Option<(usize, i32)>,
    pub end: Option<(usize, i32)>,
    pub is_selecting: bool,
}

/// A detected link in terminal content (URL or file path)
#[derive(Clone, Debug)]
pub struct DetectedLink {
    pub line: i32,
    pub col: usize,
    pub len: usize,
    pub text: String,
    pub file_line: Option<u32>,
    pub file_col: Option<u32>,
    pub is_url: bool,
    /// Segments of the same wrapped URL share the same wrap_group.
    /// Different occurrences of the same URL get different wrap_groups.
    pub wrap_group: usize,
}

/// Trim trailing punctuation from a URL/path, handling balanced parentheses.
///
/// Ghostty-style: strip trailing `.,:;!?)` but keep closing parens if they have
/// a matching opening paren inside the URL (e.g. Wikipedia links).
fn trim_url_trailing(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut end = bytes.len();

    loop {
        if end == 0 {
            break;
        }
        let c = bytes[end - 1];
        match c {
            b'.' | b',' | b':' | b';' | b'!' | b'?' => {
                end -= 1;
            }
            b')' => {
                // Only strip closing paren if unbalanced
                let open = s[..end].matches('(').count();
                let close = s[..end].matches(')').count();
                if close > open {
                    end -= 1;
                } else {
                    break;
                }
            }
            _ => break,
        }
    }

    &s[..end]
}

/// Parse optional `:line:col` suffix from a file path string.
/// Returns (display_text_including_suffix, optional_line, optional_col).
fn parse_path_line_col(s: &str) -> (String, Option<u32>, Option<u32>) {
    // Try to match :line:col at the end
    if let Some(colon_pos) = s.rfind(':') {
        let after = &s[colon_pos + 1..];
        if let Ok(num) = after.parse::<u32>() {
            let before = &s[..colon_pos];
            // Check for another :line before this
            if let Some(colon_pos2) = before.rfind(':') {
                let after2 = &before[colon_pos2 + 1..];
                if let Ok(line_num) = after2.parse::<u32>() {
                    // path:line:col
                    return (s.to_string(), Some(line_num), Some(num));
                }
            }
            // path:line
            return (s.to_string(), Some(num), None);
        }
    }
    (s.to_string(), None, None)
}

/// Consolidated resize-related state, protected by a single mutex
pub struct ResizeState {
    pub size: TerminalSize,
    last_pty_resize: std::time::Instant,
    pending_pty_resize: Option<(u16, u16)>,
    /// True when a background flush timer is scheduled to send the pending resize.
    flush_timer_active: bool,
    /// Timestamp of the last local resize (from TerminalElement::paint).
    /// Used to suppress redundant server resize echoes in remote mode.
    pub last_local_resize: std::time::Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotReason {
    AppQuit,
}

#[derive(Clone, Copy, Debug)]
pub struct SnapshotStats {
    pub cols: u16,
    pub rows: u16,
    pub pending_output_len: usize,
    pub replay_buffer_len: usize,
    pub restored_replay_buffer_len: usize,
    pub restored_from_snapshot: bool,
    pub had_user_input: bool,
    pub content_generation: u64,
    pub resize_stable: bool,
}

/// A terminal instance wrapping alacritty_terminal
pub struct Terminal {
    term: Arc<Mutex<Term<ZedEventListener>>>,
    processor: Mutex<Processor>,
    pub terminal_id: String,
    pub resize_state: Arc<Mutex<ResizeState>>,
    transport: Arc<dyn TerminalTransport>,
    selection_state: Mutex<SelectionState>,
    scroll_offset: Mutex<i32>,
    title: Arc<Mutex<Option<String>>>,
    has_bell: Arc<Mutex<bool>>,
    last_notification: Arc<Mutex<Option<TerminalNotification>>>,
    /// True when an OSC-announced notification has not yet been forwarded to
    /// the OS notification system (drained via `take_unposted_notification`).
    notification_unposted: AtomicBool,
    /// True while an agent (claude/codex/…) is actively working a turn, driven
    /// by its hooks. Surfaces as a spinner in the sidebar list.
    agent_working: Arc<Mutex<bool>>,
    pending_output: Mutex<Vec<u8>>,
    /// Rolling PTY byte stream used for persistent scrollback snapshots.
    ///
    /// This mirrors VS Code's revive path at a smaller scale: persist the bytes
    /// that fed the terminal emulator rather than reconstructing bytes from the
    /// already-rendered grid, which bakes stale column spacing into snapshots.
    replay_buffer: Mutex<Vec<u8>>,
    /// Raw bytes loaded from a persisted snapshot during terminal revival.
    ///
    /// Mirrors VS Code's `rawReviveBuffer`: while the terminal has only been
    /// replayed and not interacted with, persistence writes this raw buffer
    /// back instead of the visible buffer that includes restore UI text and a
    /// freshly spawned shell prompt.
    restored_replay_buffer: Mutex<Vec<u8>>,
    /// Whether this terminal instance was revived from a persisted snapshot.
    ///
    /// Kept separate from `restored_replay_buffer` so an empty raw revive buffer
    /// still has explicit lifecycle semantics and fresh terminals are never
    /// mistaken for restored sessions.
    restored_from_snapshot: AtomicBool,
    /// True while replaying persisted bytes into the local emulator.
    suppress_pty_responses: Arc<AtomicBool>,
    /// Dirty flag - set when terminal content changes, cleared after render
    dirty: AtomicBool,
    /// Content generation counter - incremented on each process_output call.
    /// Used by UrlDetector to skip redundant detect_urls() when content hasn't changed.
    content_generation: AtomicU64,
    /// Initial working directory (for resolving relative file paths in URL detection)
    initial_cwd: String,
    /// Timestamp of last terminal output (for idle detection)
    last_output_time: Arc<Mutex<Instant>>,
    /// Shell process PID (for foreground process check)
    shell_pid: Mutex<Option<u32>>,
    /// Cached "waiting for input" state — updated by background loop, read by renderers
    waiting_for_input: AtomicBool,
    /// Whether the user has ever sent input to this terminal (prevents flagging fresh terminals)
    had_user_input: AtomicBool,
    /// Best-effort tracking of the current shell input line.
    current_input_line: Mutex<String>,
    /// Last submitted shell command, used to persist alt-screen sessions without
    /// replaying TUI control sequences.
    last_submitted_command: Mutex<Option<String>>,
    /// Set after an alternate-screen app returns to the normal screen. The next
    /// normal-buffer output may need to be moved below preserved lines under the cursor.
    pending_cursor_below_guard: AtomicBool,
    /// Timestamp of when the user last viewed this terminal (on blur)
    last_viewed_time: Arc<Mutex<Instant>>,
}

impl Terminal {
    /// Create a new terminal
    pub fn new(
        terminal_id: String,
        size: TerminalSize,
        transport: Arc<dyn TerminalTransport>,
        initial_cwd: String,
    ) -> Self {
        let config = TermConfig::default();
        let term_size = TermSize::new(size.cols as usize, size.rows as usize);

        // Create shared storage for OSC sequence handling and bell
        let title = Arc::new(Mutex::new(None));
        let has_bell = Arc::new(Mutex::new(false));
        let last_notification = Arc::new(Mutex::new(None));
        let agent_working = Arc::new(Mutex::new(false));
        let suppress_pty_responses = Arc::new(AtomicBool::new(false));
        let event_listener = ZedEventListener::new(
            title.clone(),
            has_bell.clone(),
            transport.clone(),
            suppress_pty_responses.clone(),
            terminal_id.clone(),
        );
        let term = Term::new(config, &term_size, event_listener);

        Self {
            term: Arc::new(Mutex::new(term)),
            processor: Mutex::new(Processor::new()),
            terminal_id,
            resize_state: Arc::new(Mutex::new(ResizeState {
                size,
                // Use a time in the past so the first resize from paint() always
                // passes the debounce check and sends SIGWINCH to the PTY immediately
                last_pty_resize: std::time::Instant::now() - std::time::Duration::from_secs(1),
                flush_timer_active: false,
                pending_pty_resize: None,
                last_local_resize: std::time::Instant::now() - std::time::Duration::from_secs(1),
            })),
            transport,
            selection_state: Mutex::new(SelectionState::default()),
            scroll_offset: Mutex::new(0),
            title,
            has_bell,
            last_notification,
            notification_unposted: AtomicBool::new(false),
            agent_working,
            pending_output: Mutex::new(Vec::new()),
            replay_buffer: Mutex::new(Vec::new()),
            restored_replay_buffer: Mutex::new(Vec::new()),
            restored_from_snapshot: AtomicBool::new(false),
            suppress_pty_responses,
            dirty: AtomicBool::new(false),
            content_generation: AtomicU64::new(0),
            initial_cwd,
            last_output_time: Arc::new(Mutex::new(Instant::now())),
            shell_pid: Mutex::new(None),
            waiting_for_input: AtomicBool::new(false),
            had_user_input: AtomicBool::new(false),
            current_input_line: Mutex::new(String::new()),
            last_submitted_command: Mutex::new(None),
            pending_cursor_below_guard: AtomicBool::new(false),
            last_viewed_time: Arc::new(Mutex::new(Instant::now())),
        }
    }

    /// Capture up to `max_lines` of replayable PTY output bytes. The `cwd` is
    /// embedded in the snapshot header so the next launch can spawn the shell
    /// in the same directory the user was last working in.
    pub fn capture_scrollback(&self, max_lines: u32, cwd: Option<&str>) -> Vec<u8> {
        let replay_bytes = self.replay_buffer.lock().clone();
        let size = self.resize_state.lock().size;
        crate::scrollback_snapshot::capture_raw(&replay_bytes, size.cols, size.rows, max_lines, cwd)
    }

    /// Capture scrollback by merging the existing persisted snapshot with new
    /// live PTY output, then trimming the combined stream to `max_lines`.
    pub fn capture_scrollback_merged(
        &self,
        dir: &std::path::Path,
        key: &str,
        max_lines: u32,
        cwd: Option<&str>,
    ) -> Vec<u8> {
        let size = self.resize_state.lock().size;

        if self.is_alt_screen() {
            return self.capture_alt_screen_command(size, max_lines, cwd);
        }

        if self.restored_from_snapshot.load(Ordering::Relaxed)
            && !self.had_user_input.load(Ordering::Relaxed)
        {
            let restored_replay_bytes = self.restored_replay_buffer.lock().clone();
            return crate::scrollback_snapshot::capture_raw(
                &restored_replay_bytes,
                size.cols,
                size.rows,
                max_lines,
                cwd,
            );
        }
        let replay_bytes = self.replay_buffer.lock().clone();
        crate::scrollback_snapshot::merge_existing_with_capture(
            crate::scrollback_snapshot::MergeCapture {
                dir,
                key,
                restored_replay_bytes: &[],
                replay_bytes: &replay_bytes,
                cols: size.cols,
                rows: size.rows,
                max_lines,
                cwd,
            },
        )
    }

    fn capture_alt_screen_command(
        &self,
        size: TerminalSize,
        max_lines: u32,
        cwd: Option<&str>,
    ) -> Vec<u8> {
        let replay_bytes = self.replay_buffer.lock().clone();
        let prefix = replay_prefix_before_alt_screen(&replay_bytes).unwrap_or(&[]);
        let command = self.last_submitted_command.lock().clone();
        let command = command.as_deref().map(str::trim).filter(|cmd| !cmd.is_empty());

        if prefix.is_empty() && command.is_none() {
            return Vec::new();
        }

        let mut bytes =
            Vec::with_capacity(prefix.len() + command.map(|cmd| cmd.len() + 2).unwrap_or(0));
        bytes.extend_from_slice(prefix);

        if let Some(command) = command
            && !byte_slice_contains(prefix, command.as_bytes())
        {
            if !bytes.is_empty() && !bytes.ends_with(b"\n") && !bytes.ends_with(b"\r") {
                bytes.extend_from_slice(b"\r\n");
            }
            bytes.extend_from_slice(command.as_bytes());
            bytes.extend_from_slice(b"\r\n");
        }

        crate::scrollback_snapshot::capture_raw(&bytes, size.cols, size.rows, max_lines, cwd)
    }

    pub fn capture_scrollback_snapshot(
        &self,
        dir: &std::path::Path,
        key: &str,
        max_lines: u32,
        cwd: Option<&str>,
        _reason: SnapshotReason,
    ) -> (Vec<u8>, SnapshotStats) {
        self.drain_pending_output_for_snapshot();
        let stats = self.snapshot_stats();

        let bytes = self.capture_scrollback_merged(dir, key, max_lines, cwd);
        (bytes, stats)
    }

    /// Process output from PTY
    pub fn process_output(&self, data: &[u8]) {
        self.process_output_inner(data, true, false);
    }

    /// Replay previously persisted terminal output.
    ///
    /// Unlike live PTY output this must not be appended to the replay buffer
    /// again, and emulator-generated replies must not be sent to the new shell.
    pub fn replay_output(&self, data: &[u8]) {
        self.process_output_inner(data, false, true);
    }

    /// Replay persisted scrollback, keep it for the next snapshot, and suppress
    /// emulator-generated replies so the live shell never receives them.
    pub fn restore_scrollback_output(&self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        self.restored_from_snapshot.store(true, Ordering::Relaxed);
        self.restored_replay_buffer.lock().extend_from_slice(data);
        self.record_replay_bytes(data);
        self.process_output_inner(data, false, true);
    }

    /// Restore raw scrollback plus visible revive text.
    ///
    /// The raw bytes are retained separately for replay-only persistence, while
    /// the visible replay buffer is initialized with exactly what the user sees.
    pub fn restore_scrollback_with_initial_text(&self, raw: &[u8], initial_text: &[u8]) {
        if raw.is_empty() && initial_text.is_empty() {
            return;
        }

        self.restored_from_snapshot.store(true, Ordering::Relaxed);
        self.restored_replay_buffer.lock().extend_from_slice(raw);
        self.process_output_inner(raw, false, true);

        let initial_text = self.initial_text_after_restored_cursor(initial_text);
        {
            let mut replay = self.replay_buffer.lock();
            replay.extend_from_slice(raw);
            replay.extend_from_slice(&initial_text);
            if replay.len() > REPLAY_BUFFER_MAX_BYTES {
                let overflow = replay.len() - REPLAY_BUFFER_MAX_BYTES;
                replay.drain(..overflow);
            }
        }

        self.process_output_inner(&initial_text, false, true);
    }

    fn initial_text_after_restored_cursor(&self, initial_text: &[u8]) -> Vec<u8> {
        if initial_text.is_empty() {
            return Vec::new();
        }

        let blank_lines = self.lines_needed_below_cursor_locked(&self.term.lock());
        if blank_lines == 0 {
            return initial_text.to_vec();
        }

        let mut out = Vec::with_capacity(blank_lines * 2 + initial_text.len());
        for _ in 0..blank_lines {
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(initial_text);
        out
    }

    fn output_after_cursor_guarded(&self, data: &[u8]) -> Vec<u8> {
        if data.is_empty() || !self.pending_cursor_below_guard.swap(false, Ordering::Relaxed) {
            return data.to_vec();
        }

        let blank_lines = self.lines_needed_below_cursor_locked(&self.term.lock());
        if blank_lines == 0 {
            return data.to_vec();
        }

        let mut out = Vec::with_capacity(blank_lines * 2 + data.len());
        for _ in 0..blank_lines {
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(data);
        out
    }

    fn lines_needed_below_cursor_locked(&self, term: &Term<ZedEventListener>) -> usize {
        let grid = term.grid();
        let cursor_line = grid.cursor.point.line.0;
        let screen_lines = grid.screen_lines() as i32;
        let cols = grid.columns();
        let mut last_text_line = None;

        for row in (cursor_line + 1)..screen_lines {
            for col in 0..cols {
                let cell = &grid[Point::new(Line(row), Column(col))];
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                if cell.c != ' ' || cell.zerowidth().is_some_and(|chars| !chars.is_empty()) {
                    last_text_line = Some(row);
                    break;
                }
            }
        }

        last_text_line
            .map(|line| (line - cursor_line + 1).max(0) as usize)
            .unwrap_or(0)
    }

    fn process_output_inner(&self, data: &[u8], record_replay: bool, suppress_replies: bool) {
        if data.is_empty() {
            return;
        }
        if record_replay
            && !suppress_replies
            && let Some((before_exit, after_exit)) = split_after_alt_screen_exit(data)
        {
            self.process_output_inner(before_exit, record_replay, suppress_replies);
            self.process_output_inner(after_exit, record_replay, suppress_replies);
            return;
        }
        let was_alt_screen = self.is_alt_screen();
        let data = if record_replay {
            self.output_after_cursor_guarded(data)
        } else {
            data.to_vec()
        };
        if record_replay && !suppress_replies && let Some(notif) = extract_osc_notifications(&data).into_iter().last() {
             *self.last_notification.lock() = Some(notif);
             *self.has_bell.lock() = true;
             self.notification_unposted.store(true, Ordering::Relaxed);
            }
        if suppress_replies {
            self.suppress_pty_responses.store(true, Ordering::Relaxed);
        }
        if record_replay {
            self.record_replay_bytes(&data);
        }
        let mut term = self.term.lock();
        let mut processor = self.processor.lock();

        processor.advance(&mut *term, &data);
        let is_alt_screen = term.mode().contains(TermMode::ALT_SCREEN);
        drop(processor);
        drop(term);

        if !is_alt_screen && (was_alt_screen || contains_alt_screen_exit(&data)) {
            self.pending_cursor_below_guard
                .store(true, Ordering::Relaxed);
        }
        if suppress_replies {
            self.suppress_pty_responses.store(false, Ordering::Relaxed);
        }
        self.dirty.store(true, Ordering::Relaxed);
        self.content_generation.fetch_add(1, Ordering::Relaxed);
        *self.last_output_time.lock() = Instant::now();
    }

    fn record_replay_bytes(&self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let mut buffer = self.replay_buffer.lock();
        buffer.extend_from_slice(data);
        if buffer.len() > REPLAY_BUFFER_MAX_BYTES {
            let overflow = buffer.len() - REPLAY_BUFFER_MAX_BYTES;
            buffer.drain(..overflow);
        }
    }

    fn mark_session_interaction(&self) {
        self.had_user_input.store(true, Ordering::Relaxed);
        self.restored_from_snapshot.store(false, Ordering::Relaxed);
        self.restored_replay_buffer.lock().clear();
    }

    fn record_user_input_for_command(&self, data: &[u8]) {
        if data.is_empty() {
            return;
        }

        let Ok(text) = std::str::from_utf8(data) else {
            return;
        };
        if text.contains('\u{1b}') {
            return;
        }

        let mut line = self.current_input_line.lock();
        for ch in text.chars() {
            match ch {
                '\r' | '\n' => {
                    let submitted = line.trim();
                    if !submitted.is_empty() {
                        *self.last_submitted_command.lock() = Some(submitted.to_string());
                    }
                    line.clear();
                }
                '\u{7f}' | '\u{8}' => {
                    line.pop();
                }
                '\u{3}' | '\u{4}' => {
                    line.clear();
                }
                ch if !ch.is_control() => line.push(ch),
                _ => {}
            }
        }
    }

    /// Enqueue output data for deferred processing.
    ///
    /// Used by the remote client's tokio reader thread so it never holds
    /// `term.lock()`. The pending data is drained and parsed on the GPUI
    /// thread just before rendering (see `with_content`).
    pub fn enqueue_output(&self, data: &[u8]) {
        self.pending_output.lock().extend_from_slice(data);
        self.dirty.store(true, Ordering::Relaxed);
        *self.last_output_time.lock() = Instant::now();
    }

    /// Drain all pending output and feed it into the terminal emulator before
    /// capturing a snapshot.
    pub fn drain_pending_output_for_snapshot(&self) {
        let data = {
            let mut pending = self.pending_output.lock();
            if pending.is_empty() {
                return;
            }
            std::mem::take(&mut *pending)
        };
        self.process_output_inner(&data, true, false);
    }

    /// Drain all pending output and feed it into the terminal emulator.
    ///
    /// Called automatically by `with_content` before rendering.
    fn drain_pending_output(&self) {
        self.drain_pending_output_for_snapshot();
    }

    pub fn snapshot_stats(&self) -> SnapshotStats {
        let rs = self.resize_state.lock();
        let resize_stable = rs.pending_pty_resize.is_none()
            && !rs.flush_timer_active
            && rs.last_local_resize.elapsed().as_millis() >= 500;
        let size = rs.size;
        drop(rs);

        SnapshotStats {
            cols: size.cols,
            rows: size.rows,
            pending_output_len: self.pending_output.lock().len(),
            replay_buffer_len: self.replay_buffer.lock().len(),
            restored_replay_buffer_len: self.restored_replay_buffer.lock().len(),
            restored_from_snapshot: self.restored_from_snapshot.load(Ordering::Relaxed),
            had_user_input: self.had_user_input.load(Ordering::Relaxed),
            content_generation: self.content_generation.load(Ordering::Relaxed),
            resize_stable,
        }
    }

    /// Check if terminal has pending changes (and clear the flag).
    /// Used by PTY event loop for direct content pane notification.
    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::Relaxed)
    }

    /// Get the current content generation counter.
    pub fn content_generation(&self) -> u64 {
        self.content_generation.load(Ordering::Relaxed)
    }

    /// Send input to the PTY
    /// Automatically scrolls to bottom if scrolled into history
    pub fn send_input(&self, input: &str) {
        self.mark_session_interaction();
        self.record_user_input_for_command(input.as_bytes());
        self.scroll_to_bottom();
        self.transport
            .send_input(&self.terminal_id, input.as_bytes());
    }

    /// Send pasted text to the PTY, wrapping in bracketed paste sequences if the
    /// terminal application has enabled bracketed paste mode (DECSET 2004).
    /// This prevents shells from executing each line of a multi-line paste individually.
    pub fn send_paste(&self, text: &str) {
        self.mark_session_interaction();
        self.scroll_to_bottom();

        let bracketed = self.term.lock().mode().contains(TermMode::BRACKETED_PASTE);
        if bracketed {
            // Preserve pasted newlines inside the bracketed paste block. Strip embedded
            // paste markers and escape bytes so pasted content cannot break out of the block.
            let sanitized = text
                .replace("\x1b[200~", "")
                .replace("\x1b[201~", "")
                .replace('\x1b', "");
            self.record_user_input_for_command(sanitized.as_bytes());
            let mut buf = Vec::with_capacity(sanitized.len() + 12);
            buf.extend_from_slice(b"\x1b[200~");
            buf.extend_from_slice(sanitized.as_bytes());
            buf.extend_from_slice(b"\x1b[201~");
            self.transport.send_input(&self.terminal_id, &buf);
        } else {
            // Without bracketed paste, terminals submit lines with carriage returns.
            let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
            self.record_user_input_for_command(normalized.as_bytes());
            self.transport
                .send_input(&self.terminal_id, normalized.as_bytes());
        }
    }

    /// Send raw bytes to the PTY
    /// Automatically scrolls to bottom if scrolled into history
    pub fn send_bytes(&self, data: &[u8]) {
        self.mark_session_interaction();
        self.record_user_input_for_command(data);
        self.scroll_to_bottom();
        self.transport.send_input(&self.terminal_id, data);
    }

    /// Clear the terminal screen by sending the clear sequence
    pub fn clear(&self) {
        self.mark_session_interaction();
        // Send ANSI escape sequence to clear screen and move cursor to home
        // \x1b[2J = clear entire screen
        // \x1b[H = move cursor to home position (0,0)
        self.transport
            .send_input(&self.terminal_id, b"\x1b[2J\x1b[H");
        self.scroll_to_bottom();
    }

    /// Select all visible text in the terminal
    pub fn select_all(&self) {
        let mut term = self.term.lock();
        let grid = term.grid();
        let rows = grid.screen_lines() as i32;
        let cols = grid.columns();
        let history = grid.history_size() as i32;

        // Clear any existing selection
        term.selection = None;
        drop(term);

        // Create selection from start of history to end of screen
        // Start at top-left of history
        let start_row = -history;
        let start_col = 0;

        // End at bottom-right of visible area
        let end_row = rows - 1;
        let end_col = cols.saturating_sub(1);

        // Use the existing selection infrastructure
        self.start_selection(start_col, start_row, Side::Left);
        self.update_selection(end_col, end_row, Side::Right);
        self.end_selection();
    }

    /// Scroll to bottom (display_offset = 0)
    pub fn scroll_to_bottom(&self) {
        let mut term = self.term.lock();
        let current = term.grid().display_offset();
        if current > 0 {
            term.scroll_display(Scroll::Delta(-(current as i32)));
            self.content_generation.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Resize the terminal with debounced transport resize.
    ///
    /// The terminal grid is always resized immediately (optimistic update) for
    /// smooth rendering. Transport resize signals (PTY/remote) are debounced to
    /// avoid flooding the shell or network.
    ///
    /// Debounce interval is transport-dependent: 16ms for local PTY (~60fps),
    /// 150ms for remote connections. A trailing-edge timer ensures the final
    /// resize is always sent even when resize events stop mid-debounce.
    pub fn resize(&self, new_size: TerminalSize) {
        let debounce_ms = self.transport.resize_debounce_ms();

        // Always update local size immediately (optimistic UI)
        {
            let mut rs = self.resize_state.lock();
            rs.size = new_size;
            rs.last_local_resize = std::time::Instant::now();
        }

        // Resize terminal grid immediately (independent mutex)
        let mut term = self.term.lock();
        let term_size = TermSize::new(new_size.cols as usize, new_size.rows as usize);
        term.resize(term_size);
        drop(term);

        self.content_generation.fetch_add(1, Ordering::Relaxed);

        // Debounce transport resize
        let now = std::time::Instant::now();
        let mut rs = self.resize_state.lock();
        let elapsed = now.duration_since(rs.last_pty_resize);

        if elapsed.as_millis() >= debounce_ms as u128 {
            // Enough time has passed — send resize immediately
            rs.pending_pty_resize = None;
            rs.last_pty_resize = now;
            drop(rs);
            self.transport
                .resize(&self.terminal_id, new_size.cols, new_size.rows);
        } else {
            // Store pending resize
            rs.pending_pty_resize = Some((new_size.cols, new_size.rows));

            // Schedule a trailing-edge flush timer if not already active.
            // This ensures the final resize is always sent even when events stop.
            if !rs.flush_timer_active {
                rs.flush_timer_active = true;
                let transport = self.transport.clone();
                let terminal_id = self.terminal_id.clone();
                let resize_state = self.resize_state.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(debounce_ms));
                    let mut rs = resize_state.lock();
                    rs.flush_timer_active = false;
                    if let Some((cols, rows)) = rs.pending_pty_resize.take() {
                        rs.last_pty_resize = std::time::Instant::now();
                        drop(rs);
                        transport.resize(&terminal_id, cols, rows);
                    }
                });
            }
        }
    }

    /// Resize only the local alacritty grid, without sending resize to PTY/transport.
    /// Used by remote clients to pre-resize the grid to match server dimensions before snapshot.
    ///
    /// Skips the resize if the client recently performed a local resize (within 200ms)
    /// to avoid redundant grid reflows from server echo during active resize drag.
    pub fn resize_grid_only(&self, cols: u16, rows: u16) {
        let rs = self.resize_state.lock();
        // Skip if we recently resized locally — the server is echoing back our own resize
        if rs.last_local_resize.elapsed().as_millis() < 200 {
            // Still accept if the size actually differs (e.g. server-initiated resize)
            if rs.size.cols == cols && rs.size.rows == rows {
                return;
            }
        }
        let size = TerminalSize {
            cols,
            rows,
            cell_width: rs.size.cell_width,
            cell_height: rs.size.cell_height,
        };
        drop(rs);
        self.resize_state.lock().size = size;
        let mut term = self.term.lock();
        let term_size = TermSize::new(cols as usize, rows as usize);
        term.resize(term_size);
        self.content_generation.fetch_add(1, Ordering::Relaxed);
    }

    /// Mark the local side (origin) as resize authority. Process-global:
    /// any local keyboard/mouse input in any terminal claims authority for all
    /// terminals.
    pub fn claim_resize_local(&self) {
        claim_resize_authority_local();
    }

    /// Mark the remote side as resize authority. Called on the server when a
    /// remote client sends input to any terminal.
    pub fn claim_resize_remote(&self) {
        claim_resize_authority_remote();
    }

    /// Returns true if the local (origin) side currently has resize authority.
    /// The server's UI uses this to decide whether to push resize events to
    /// the PTY.
    pub fn is_resize_owner_local(&self) -> bool {
        is_resize_authority_local()
    }

    /// Flush any pending PTY resize (call this when resize operations complete)
    pub fn flush_pending_resize(&self) {
        let mut rs = self.resize_state.lock();
        if let Some((cols, rows)) = rs.pending_pty_resize.take() {
            rs.last_pty_resize = std::time::Instant::now();
            drop(rs);
            self.transport.resize(&self.terminal_id, cols, rows);
        }
    }

    /// Access the terminal content for rendering.
    ///
    /// Drains any pending output (enqueued by remote clients) before
    /// handing the content to the callback, so the rendered frame is
    /// always up-to-date.
    pub fn with_content<R>(&self, f: impl FnOnce(&Term<ZedEventListener>) -> R) -> R {
        self.drain_pending_output();
        let term = self.term.lock();
        f(&term)
    }

    /// Scroll the terminal
    pub fn scroll(&self, delta: i32) {
        let mut term = self.term.lock();
        term.scroll_display(Scroll::Delta(delta));
        *self.scroll_offset.lock() += delta;
        self.content_generation.fetch_add(1, Ordering::Relaxed);
    }

    /// Scroll up by lines
    pub fn scroll_up(&self, lines: i32) {
        self.scroll(lines);
    }

    /// Scroll down by lines
    pub fn scroll_down(&self, lines: i32) {
        self.scroll(-lines);
    }

    /// Start selection at a point
    pub fn start_selection(&self, col: usize, row: i32, side: Side) {
        self.start_selection_with_type(col, row, SelectionType::Simple, side);
    }

    /// Start word (semantic) selection at a point
    pub fn start_word_selection(&self, col: usize, row: i32) {
        self.start_selection_with_type(col, row, SelectionType::Semantic, Side::Left);
    }

    /// Start line selection at a point
    pub fn start_line_selection(&self, col: usize, row: i32) {
        self.start_selection_with_type(col, row, SelectionType::Lines, Side::Left);
    }

    /// Start selection with a specific type
    /// Note: row is the visual row on screen (0 to screen_lines-1)
    /// We convert it to buffer coordinates by accounting for display_offset
    fn start_selection_with_type(
        &self,
        col: usize,
        row: i32,
        selection_type: SelectionType,
        side: Side,
    ) {
        let mut term = self.term.lock();

        // Convert visual row to buffer row
        // When scrolled up (display_offset > 0), visual row 0 shows history (negative buffer lines)
        let display_offset = term.grid().display_offset() as i32;
        let buffer_row = row - display_offset;

        let mut state = self.selection_state.lock();
        state.start = Some((col, buffer_row));
        state.end = Some((col, buffer_row));
        state.is_selecting = true;

        // Set selection in the terminal using buffer coordinates
        let point = Point::new(Line(buffer_row), Column(col));
        let selection = Selection::new(selection_type, point, side);
        term.selection = Some(selection);
    }

    /// Update selection to a new point
    /// Note: row is the visual row on screen (0 to screen_lines-1)
    /// We convert it to buffer coordinates by accounting for display_offset
    pub fn update_selection(&self, col: usize, row: i32, side: Side) {
        let mut state = self.selection_state.lock();
        if state.is_selecting {
            // Update terminal selection
            let mut term = self.term.lock();

            // Convert visual row to buffer row
            let display_offset = term.grid().display_offset() as i32;
            let buffer_row = row - display_offset;

            state.end = Some((col, buffer_row));

            if let Some(ref mut selection) = term.selection {
                let point = Point::new(Line(buffer_row), Column(col));
                selection.update(point, side);
            }
        }
    }

    /// End selection
    pub fn end_selection(&self) {
        let mut state = self.selection_state.lock();
        state.is_selecting = false;
    }

    /// Clear selection
    pub fn clear_selection(&self) {
        let mut state = self.selection_state.lock();
        state.start = None;
        state.end = None;
        state.is_selecting = false;

        let mut term = self.term.lock();
        term.selection = None;
    }

    /// Get selected text
    pub fn get_selected_text(&self) -> Option<String> {
        let term = self.term.lock();
        term.selection_to_string()
    }

    /// Check if there is an active selection
    pub fn has_selection(&self) -> bool {
        let term = self.term.lock();
        term.selection.is_some()
    }

    /// Get selection bounds for rendering
    /// Uses alacritty's selection which properly handles semantic (word) and line selection
    /// Returns ((start_col, start_row), (end_col, end_row)) where rows are buffer coordinates (can be negative for history)
    pub fn selection_bounds(&self) -> Option<((usize, i32), (usize, i32))> {
        let term = self.term.lock();
        if let Some(ref selection) = term.selection
            && let Some(range) = selection.to_range(&*term)
        {
            let start = (range.start.column.0, range.start.line.0);
            let end = (range.end.column.0, range.end.line.0);
            return Some((start, end));
        }
        None
    }

    /// Get cell dimensions (width, height) for coordinate conversion
    pub fn cell_dimensions(&self) -> (f32, f32) {
        let rs = self.resize_state.lock();
        (rs.size.cell_width, rs.size.cell_height)
    }

    /// Get the terminal title (from OSC sequences)
    pub fn title(&self) -> Option<String> {
        self.title.lock().clone()
    }

    /// Check if terminal has unread bell notification
    pub fn has_bell(&self) -> bool {
        *self.has_bell.lock()
    }
    pub fn clear_bell(&self) {
        *self.has_bell.lock() = false;
    }
    pub fn last_notification(&self) -> Option<TerminalNotification> {
        self.last_notification.lock().clone()
    }
    /// Take the latest OSC-announced notification exactly once, for native OS
    /// notification posting. Returns `None` until PTY output carries a new
    /// OSC 9/99/777 notification. Notifications set via [`set_notification`]
    /// (the remote `Notify` action) are deliberately not returned here — that
    /// path posts natively at the action site.
    ///
    /// [`set_notification`]: Self::set_notification
    pub fn take_unposted_notification(&self) -> Option<TerminalNotification> {
        if self.notification_unposted.swap(false, Ordering::Relaxed) {
            self.last_notification.lock().clone()
        } else {
            None
        }
    }
    pub fn clear_notification(&self) {
        *self.last_notification.lock() = None;
        *self.has_bell.lock() = false;
        // Repaint the mounted pane so the ring clears promptly (mirrors the
        // dirty flag set by real PTY output in `process_output_inner`).
        self.dirty.store(true, Ordering::Relaxed);
    }
    pub fn set_notification(&self, title: String, body: String) {
        *self.last_notification.lock() = Some(TerminalNotification {
            title,
            body,
            timestamp: std::time::Instant::now(),
        });
        *self.has_bell.lock() = true;
        // Mark dirty so the pane's dirty-check loop repaints the notification
        // ring/overlay even without further PTY output.
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Whether an agent is actively working a turn in this terminal.
    pub fn agent_working(&self) -> bool {
        *self.agent_working.lock()
    }
    /// Set the agent working state (driven by agent lifecycle hooks).
    pub fn set_agent_working(&self, working: bool) {
        *self.agent_working.lock() = working;
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Get the initial working directory for this terminal
    pub fn initial_cwd(&self) -> &str {
        &self.initial_cwd
    }

    /// Set the shell process PID (for foreground process checking)
    pub fn set_shell_pid(&self, pid: u32) {
        *self.shell_pid.lock() = Some(pid);
    }

    /// Read the cached "waiting for input" state (cheap, no subprocess).
    /// This is safe to call from render paths. Updated by `update_waiting_state()`.
    pub fn is_waiting_for_input(&self) -> bool {
        self.waiting_for_input.load(Ordering::Relaxed)
    }

    /// Human-readable idle duration string (e.g., "5s", "2m", "1h").
    /// Shows time since the unseen output arrived.
    /// Only meaningful when `is_waiting_for_input()` is true.
    pub fn idle_duration_display(&self) -> String {
        let secs = self.last_viewed_time.lock().elapsed().as_secs();
        if secs < 60 {
            format!("{}s", secs)
        } else if secs < 3600 {
            format!("{}m", secs / 60)
        } else {
            format!("{}h", secs / 3600)
        }
    }

    /// Get the shell PID (for background thread to run pgrep off the main thread)
    pub fn shell_pid(&self) -> Option<u32> {
        *self.shell_pid.lock()
    }

    /// Get the last output time (for background thread idle check)
    pub fn last_output_time(&self) -> Instant {
        *self.last_output_time.lock()
    }

    /// Whether the user has ever sent input to this terminal
    pub fn had_user_input(&self) -> bool {
        self.had_user_input.load(Ordering::Relaxed)
    }

    /// Update the cached waiting state (called from background thread only)
    pub fn set_waiting_for_input(&self, waiting: bool) {
        self.waiting_for_input.store(waiting, Ordering::Relaxed);
    }

    /// Returns true if the shell currently has a child process running.
    /// Performs a synchronous, low-overhead check (direct `/proc` read on Linux,
    /// `pgrep -P` fallback elsewhere) and is safe to call from UI event handlers.
    ///
    /// Note: `shell_pid` is expected to be the *real* shell pid, not a session
    /// proxy (dtach / tmux attach client). Session-backend resolution is done
    /// when the terminal is created (see `TerminalBackend::get_foreground_shell_pid`).
    pub fn has_running_child(&self) -> bool {
        match *self.shell_pid.lock() {
            Some(pid) => has_child_processes(pid),
            None => false,
        }
    }

    /// Reset the idle timer to now, clearing the waiting state.
    /// Called when the terminal receives focus so it won't immediately re-trigger.
    pub fn clear_waiting(&self) {
        self.waiting_for_input.store(false, Ordering::Relaxed);
        *self.last_output_time.lock() = Instant::now();
        *self.last_viewed_time.lock() = Instant::now();
    }

    /// Record that the user has seen this terminal's output (called on blur).
    /// After this, the terminal won't be flagged as waiting unless new output arrives.
    pub fn mark_as_viewed(&self) {
        *self.last_viewed_time.lock() = Instant::now();
    }

    /// Whether new output has arrived since the user last viewed this terminal.
    pub fn has_unseen_output(&self) -> bool {
        *self.last_output_time.lock() > *self.last_viewed_time.lock()
    }

    /// Search the terminal grid for occurrences of a query string
    /// Returns a list of (line, col, length) for each match
    /// Supports case-sensitive and regex search, and searches through scrollback buffer
    pub fn search_grid(
        &self,
        query: &str,
        case_sensitive: bool,
        is_regex: bool,
    ) -> Vec<(i32, usize, usize)> {
        if query.is_empty() {
            return Vec::new();
        }

        // Build regex pattern if needed
        let regex = if is_regex {
            let pattern = if case_sensitive {
                query.to_string()
            } else {
                format!("(?i){}", query)
            };
            match Regex::new(&pattern) {
                Ok(r) => Some(r),
                Err(_) => return Vec::new(), // Invalid regex, return no matches
            }
        } else {
            None
        };

        let mut matches = Vec::new();

        self.with_content(|term| {
            let grid = term.grid();
            let screen_lines = grid.screen_lines() as i32;
            let history_size = grid.history_size() as i32;
            let cols = grid.columns();
            let _display_offset = grid.display_offset() as i32;

            // Search from top of history to bottom of screen
            // Line numbers: negative = history, 0..screen_lines = visible
            // We iterate from -(history_size) to (screen_lines - 1)
            for row in (-history_size)..screen_lines {
                // Calculate the actual line index for grid access
                // The grid uses Line() which handles the offset automatically
                let line = row;

                // Build the line text
                let mut line_text = String::with_capacity(cols);
                for col in 0..cols {
                    let cell_point = Point::new(Line(line), Column(col));
                    let cell = &grid[cell_point];
                    line_text.push(cell.c);
                }

                // Build byte-to-column mapping for converting byte offsets to grid columns.
                // Each char in line_text corresponds to exactly one grid column.
                let total_chars = line_text.chars().count();

                // Convert a byte offset to a column index
                let col_at_byte = |byte_offset: usize| -> usize {
                    line_text
                        .char_indices()
                        .enumerate()
                        .find(|(_, (b, _))| *b == byte_offset)
                        .map(|(col, _)| col)
                        .unwrap_or(total_chars)
                };

                if let Some(ref regex) = regex {
                    // Regex search
                    for mat in regex.find_iter(&line_text) {
                        let col = col_at_byte(mat.start());
                        let end_col = col_at_byte(mat.end());
                        // Store absolute grid line (not display-relative)
                        matches.push((line, col, end_col - col));
                    }
                } else {
                    // Plain text search
                    let (search_text, query_text) = if case_sensitive {
                        (line_text.clone(), query.to_string())
                    } else {
                        (line_text.to_lowercase(), query.to_lowercase())
                    };

                    let query_char_len = query.chars().count();
                    let mut search_start = 0;
                    while let Some(pos) = search_text[search_start..].find(&query_text) {
                        let byte_pos = search_start + pos;
                        let col = col_at_byte(byte_pos);
                        // Store absolute grid line (not display-relative)
                        matches.push((line, col, query_char_len));
                        search_start = byte_pos + query_text.len();
                        if search_start >= search_text.len() {
                            break;
                        }
                    }
                }
            }
        });

        matches
    }

    /// Get the number of screen lines
    pub fn screen_lines(&self) -> usize {
        self.with_content(|term| term.grid().screen_lines())
    }

    /// Get scroll information for scrollbar rendering
    /// Returns (total_lines, visible_lines, scroll_offset)
    /// total_lines: Total number of lines in history + screen
    /// visible_lines: Number of lines visible on screen
    /// scroll_offset: Current scroll position (0 = bottom, positive = scrolled up)
    pub fn scroll_info(&self) -> (usize, usize, i32) {
        let term = self.term.lock();
        let grid = term.grid();
        let visible_lines = grid.screen_lines();
        let history_size = grid.history_size();
        let total_lines = visible_lines + history_size;
        let display_offset = grid.display_offset();
        (total_lines, visible_lines, display_offset as i32)
    }

    /// Get the current display offset (how many lines scrolled from bottom)
    pub fn display_offset(&self) -> usize {
        let term = self.term.lock();
        term.grid().display_offset()
    }

    /// Detect URLs and file paths in the visible terminal content (Ghostty-style).
    ///
    /// Uses a single combined regex compiled once via OnceLock. Two branches:
    /// - URL: many schemes (http, https, ftp, ssh, git, mailto, etc.)
    /// - Path: explicit prefixes only (`/`, `~/`, `./`, `../`) with optional `:line:col`
    ///
    /// Returns a list of `DetectedLink` for each match. File paths are validated
    /// for existence by the caller (UrlDetector).
    pub fn detect_urls(&self) -> Vec<DetectedLink> {
        static LINK_REGEX: OnceLock<Regex> = OnceLock::new();
        let regex = LINK_REGEX.get_or_init(|| {
            // Combined regex: URL schemes | explicit file paths with optional :line:col
            // Path prefixes: /, ~/, ./, ../, or dotfile dirs like .github/
            Regex::new(
                r#"(?:(?:https?|ftp|file|ssh|git|mailto|tel|magnet|ipfs|gemini|gopher|news)://[^\s<>"'`{}\[\]|\\^]+|(?:~?/|(?:\./|\.\./)|\.[a-zA-Z][\w.-]*/)[^\s<>"'`{}\[\]|\\^()]+(?::(\d+)(?::(\d+))?)?)"#
            ).expect("link detection regex should compile")
        });

        // Characters that can appear in a URL (for continuation detection)
        let url_char = |c: char| -> bool {
            c.is_ascii_alphanumeric()
                || matches!(
                    c,
                    '-' | '.'
                        | '_'
                        | '~'
                        | ':'
                        | '/'
                        | '?'
                        | '#'
                        | '['
                        | ']'
                        | '@'
                        | '!'
                        | '$'
                        | '&'
                        | '\''
                        | '('
                        | ')'
                        | '*'
                        | '+'
                        | ','
                        | ';'
                        | '='
                        | '%'
                )
        };

        let mut matches = Vec::new();
        let mut next_wrap_group = 0usize;

        self.with_content(|term| {
            let grid = term.grid();
            let screen_lines = grid.screen_lines() as i32;
            let cols = grid.columns();
            let last_col = Column(cols - 1);
            let display_offset = grid.display_offset() as i32;

            // Helper: read a visual row from the grid as a String.
            let read_row = |vrow: i32| -> String {
                let buf = vrow - display_offset;
                let mut s = String::with_capacity(cols);
                for c in 0..cols {
                    s.push(grid[Point::new(Line(buf), Column(c))].c);
                }
                s
            };

            // Iterate over visual rows (0..screen_lines).
            // When scrolled, visual row 0 maps to buffer line (0 - display_offset).
            let mut visual_row = 0i32;
            while visual_row < screen_lines {
                let mut combined_text = String::new();
                // (visual_row, offset_in_combined, leading_spaces_stripped)
                let mut row_offsets: Vec<(i32, usize, usize)> = Vec::new();

                // Collect wrapped lines into one logical line
                loop {
                    let row_text = read_row(visual_row);

                    // Trim trailing spaces — URLs/paths never end with spaces,
                    // and this allows the regex to match across padded line breaks.
                    let rtrimmed = row_text.trim_end_matches(' ');

                    // For continuation rows, also strip leading spaces (TUI padding)
                    let (text_to_add, leading_stripped) = if combined_text.is_empty() {
                        (rtrimmed, 0usize)
                    } else {
                        let ltrimmed = rtrimmed.trim_start_matches(' ');
                        (ltrimmed, rtrimmed.len() - ltrimmed.len())
                    };

                    row_offsets.push((visual_row, combined_text.len(), leading_stripped));
                    combined_text.push_str(text_to_add);

                    let buffer_line = visual_row - display_offset;
                    let last_cell = &grid[Point::new(Line(buffer_line), last_col)];
                    let has_wrapline_flag = last_cell.flags.contains(Flags::WRAPLINE);

                    visual_row += 1;

                    // Only merge via terminal WRAPLINE flag.  TUI-managed
                    // wrapping (no WRAPLINE) is handled in Phase 2 below.
                    if !has_wrapline_flag || visual_row >= screen_lines {
                        break;
                    }
                }

                for mat in regex.find_iter(&combined_text) {
                    let raw = mat.as_str();
                    let trimmed = trim_url_trailing(raw);
                    if trimmed.is_empty() {
                        continue;
                    }

                    // Each regex match gets a unique wrap_group.
                    // Segments of a wrapped URL (same match, multiple rows) share it.
                    let wrap_group = next_wrap_group;
                    next_wrap_group += 1;

                    let match_start = mat.start();
                    let trimmed_end = match_start + trimmed.len();

                    // Determine if this is a URL or file path
                    let is_url = trimmed.contains("://");

                    // Parse :line:col from file paths
                    let (display_text, file_line, file_col) = if !is_url {
                        parse_path_line_col(trimmed)
                    } else {
                        (trimmed.to_string(), None, None)
                    };

                    // Map back to physical rows
                    for i in 0..row_offsets.len() {
                        let (phys_row, row_start_offset, leading_stripped) = row_offsets[i];
                        let row_end_offset = if i + 1 < row_offsets.len() {
                            row_offsets[i + 1].1
                        } else {
                            combined_text.len()
                        };

                        if trimmed_end <= row_start_offset || match_start >= row_end_offset {
                            continue;
                        }

                        let seg_start = match_start.max(row_start_offset);
                        let seg_end = trimmed_end.min(row_end_offset);

                        let col_start = combined_text[row_start_offset..seg_start].chars().count()
                            + leading_stripped;
                        let len = combined_text[seg_start..seg_end].chars().count();

                        if len > 0 {
                            matches.push(DetectedLink {
                                line: phys_row,
                                col: col_start,
                                len,
                                text: display_text.clone(),
                                file_line,
                                file_col,
                                is_url,
                                wrap_group,
                            });
                        }
                    }
                }
            }

            // ── Phase 2: Extend URL matches at TUI-wrapped row boundaries ──
            //
            // Phase 1 only merges rows with the terminal WRAPLINE flag.  TUI
            // applications manage their own wrapping (no WRAPLINE), so a long
            // URL may be split across visual rows with only the first fragment
            // matched by the regex.
            //
            // Approach inspired by Kitty: for each URL that reaches the end of
            // visible content, strip leading whitespace from the next row and
            // consume URL-compatible chars.  No attempt to reverse-engineer TUI
            // decoration via common-prefix detection (too fragile).
            //
            // Guards against false positives:
            //  - URL must not start at col 0 (terminal would set WRAPLINE)
            //  - No alphabetic text before/after the URL (prose context)
            //  - Continuation must have alphanumeric chars (not just punctuation)
            //  - "Weak" continuations (no `/`) rejected if content has spaces
            //  - Continuation containing `://` means a new URL, not extension

            let phase1_len = matches.len();
            let mut idx = 0;
            while idx < phase1_len {
                let group = matches[idx].wrap_group;

                // Advance to the last segment of this wrap_group.
                let mut last_idx = idx;
                while last_idx + 1 < phase1_len && matches[last_idx + 1].wrap_group == group {
                    last_idx += 1;
                }
                let next_idx = last_idx + 1;

                // Only extend URL matches (not file paths).
                if !matches[last_idx].is_url {
                    idx = next_idx;
                    continue;
                }

                // URL must start after col 0 — if the URL occupies the full
                // line without WRAPLINE, the lines are independent (the
                // terminal would have set WRAPLINE for a genuine wrap).
                let url_start_col = matches[idx].col;
                if url_start_col == 0 {
                    idx = next_idx;
                    continue;
                }

                // Skip rows with WRAPLINE (already handled by Phase 1).
                let m_line = matches[last_idx].line;
                let m_col = matches[last_idx].col;
                let m_len = matches[last_idx].len;
                let match_buf_line = m_line - display_offset;
                let match_last_cell = &grid[Point::new(Line(match_buf_line), last_col)];
                if match_last_cell.flags.contains(Flags::WRAPLINE) {
                    idx = next_idx;
                    continue;
                }

                let match_row_text = read_row(m_line);
                let match_rtrimmed = match_row_text.trim_end();

                // URL must reach near the end of visible content.
                // TUIs may use a narrower layout than the terminal width.
                let trimmed_char_len = match_rtrimmed.chars().count();
                if m_col + m_len + 3 < trimmed_char_len {
                    idx = next_idx;
                    continue;
                }

                // No alphabetic text after the URL (prose context).
                let url_end_pos = m_col + m_len;
                let suffix_byte = match_rtrimmed
                    .char_indices()
                    .nth(url_end_pos)
                    .map_or(match_rtrimmed.len(), |(b, _)| b);
                if match_rtrimmed[suffix_byte..]
                    .chars()
                    .any(|c| c.is_alphabetic())
                {
                    idx = next_idx;
                    continue;
                }

                // ── Extension loop ──
                let mut extended_url = matches[last_idx].text.clone();
                let mut current_row = m_line;

                loop {
                    let next_row = current_row + 1;
                    if next_row >= screen_lines {
                        break;
                    }

                    let next_row_text = read_row(next_row);
                    let next_rtrimmed = next_row_text.trim_end();

                    // Strip leading whitespace (TUI indentation).
                    let content = next_rtrimmed.trim_start_matches(' ');
                    let indent = next_rtrimmed.len() - content.len();

                    if content.is_empty() {
                        break;
                    }

                    // Don't extend into a new URL scheme.
                    if content.starts_with("http://")
                        || content.starts_with("https://")
                        || content.starts_with("ftp://")
                        || content.starts_with("file://")
                        || content.starts_with("ssh://")
                        || content.starts_with("git://")
                    {
                        break;
                    }

                    // Take URL-compatible chars as extension.
                    let ext_char_len = content.chars().take_while(|c| url_char(*c)).count();
                    if ext_char_len == 0 {
                        break;
                    }
                    let ext_byte_len = content
                        .char_indices()
                        .nth(ext_char_len)
                        .map_or(content.len(), |(b, _)| b);
                    let ext_raw = &content[..ext_byte_len];

                    // Trim the FULL combined URL, not just the fragment,
                    // so balanced parens spanning the line break are
                    // handled correctly (e.g. `Rust_(pr` + `ogramming_language)`).
                    let candidate = format!("{}{}", extended_url, ext_raw);
                    let trimmed_full = trim_url_trailing(&candidate);
                    if trimmed_full.len() <= extended_url.len() {
                        break;
                    }
                    let ext_trimmed = &trimmed_full[extended_url.len()..];

                    // Must contain at least one alphanumeric character.
                    if !ext_trimmed.chars().any(|c| c.is_alphanumeric()) {
                        break;
                    }

                    // Pure alphabetic words (e.g. "remote", "next",
                    // "Press") are not URL continuations — URL path
                    // fragments always contain non-alpha chars (digits,
                    // `/`, `-`, `_`, `.`, etc.).
                    if ext_trimmed.chars().all(|c| c.is_alphabetic()) {
                        break;
                    }

                    // Remaining content has a URL scheme → new item.
                    let remaining = &content[ext_byte_len..];
                    if remaining.contains("://") {
                        break;
                    }

                    // "Weak" extension (no path separator `/`): only
                    // accept when the full content has no spaces.
                    // URLs never contain spaces; spaces mean prose.
                    // Exception: tokens with digits (UUIDs, hashes, IDs)
                    // are almost certainly URL content, not words.
                    if !ext_trimmed.contains('/')
                        && !ext_trimmed.chars().any(|c| c.is_ascii_digit())
                        && content.contains(' ')
                    {
                        break;
                    }

                    // Commit extension.
                    let ext_trimmed_len = ext_trimmed.len();
                    let ext_trimmed_chars = ext_trimmed.chars().count();
                    extended_url.push_str(ext_trimmed);

                    matches.push(DetectedLink {
                        line: next_row,
                        col: indent,
                        len: ext_trimmed_chars,
                        text: String::new(), // updated below
                        file_line: None,
                        file_col: None,
                        is_url: true,
                        wrap_group: group,
                    });

                    // If trim_url_trailing removed characters, the URL
                    // ended here (e.g. trailing `,`, `.`).
                    if ext_trimmed_len < ext_raw.len() {
                        break;
                    }

                    // Continue only if extension fills to near end of
                    // visible content on this row.
                    let next_trimmed_len = next_rtrimmed.chars().count();
                    if indent + ext_char_len + 3 < next_trimmed_len {
                        break;
                    }
                    if !remaining.is_empty() && remaining.chars().any(|c| c.is_alphanumeric()) {
                        break;
                    }

                    current_row = next_row;
                }

                // Update text for all segments (original + extensions).
                if extended_url != matches[last_idx].text {
                    for m in matches.iter_mut() {
                        if m.wrap_group == group {
                            m.text.clone_from(&extended_url);
                        }
                    }
                }

                idx = next_idx;
            }
        });

        matches
    }

    /// Render the terminal's visible content as ANSI escape sequences.
    ///
    /// Produces a byte stream that, when fed to another terminal emulator,
    /// reproduces the current screen state including colors and attributes.
    pub fn render_snapshot(&self) -> Vec<u8> {
        self.with_content(grid_to_ansi)
    }

    /// Scroll to a specific position (0 = bottom, positive = towards top)
    pub fn scroll_to(&self, offset: usize) {
        let mut term = self.term.lock();
        let current = term.grid().display_offset();
        let delta = offset as i32 - current as i32;
        if delta != 0 {
            term.scroll_display(Scroll::Delta(delta));
            self.content_generation.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Check if terminal is in mouse reporting mode (for tmux, vim, etc.)
    /// Also returns true if using tmux backend (which handles mouse with `set mouse on`)
    pub fn is_mouse_mode(&self) -> bool {
        // If using tmux backend, tmux handles mouse events directly
        if self.transport.uses_mouse_backend() {
            return true;
        }
        // Otherwise check if the terminal itself requested mouse mode
        let term = self.term.lock();
        term.mode().intersects(TermMode::MOUSE_MODE)
    }

    /// Check if terminal is in application cursor keys mode (DECCKM)
    /// When enabled, arrow keys should send SS3 sequences (\x1bOA) instead of CSI (\x1b[A)
    /// This is used by applications like less, vim, htop, etc.
    pub fn is_app_cursor_mode(&self) -> bool {
        let term = self.term.lock();
        term.mode().contains(TermMode::APP_CURSOR)
    }

    /// Check if terminal is using the alternate screen buffer.
    /// TUI apps (vim, less, htop, Claude Code CLI) use alternate screen.
    pub fn is_alt_screen(&self) -> bool {
        let term = self.term.lock();
        term.mode().contains(TermMode::ALT_SCREEN)
    }

    /// Delete the currently selected text by sending arrow keys + backspaces to the PTY.
    /// Only works for single-row selections on the cursor's row in a plain shell.
    /// Returns true if deletion was performed.
    pub fn delete_selection(&self) -> bool {
        let mut term = self.term.lock();

        let display_offset = term.grid().display_offset();
        if display_offset != 0 {
            return false;
        }

        let range = match term.selection.as_ref().and_then(|s| s.to_range(&*term)) {
            Some(r) => r,
            None => return false,
        };

        let cursor = term.grid().cursor.point;

        // Only single-row selections on the cursor's row
        if range.start.line != range.end.line || range.start.line != cursor.line {
            return false;
        }

        let sel_start = range.start.column.0;
        let sel_end = range.end.column.0; // inclusive
        let cursor_col = cursor.column.0;
        let app_cursor = term.mode().contains(TermMode::APP_CURSOR);

        // Count logical characters in selection (skip WIDE_CHAR_SPACER)
        let mut char_count = 0usize;
        for c in sel_start..=sel_end {
            let cell = &term.grid()[Point::new(cursor.line, Column(c))];
            if !cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                char_count += 1;
            }
        }

        if char_count == 0 {
            return false;
        }

        // Move cursor to end of selection + 1 (position right after last selected char)
        let target_col = sel_end + 1;
        let mut arrow_count = 0usize;
        if target_col > cursor_col {
            for c in cursor_col..target_col {
                let cell = &term.grid()[Point::new(cursor.line, Column(c))];
                if !cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    arrow_count += 1;
                }
            }
        } else if target_col < cursor_col {
            // Cursor is to the right — need left arrows (negative direction)
            for c in target_col..cursor_col {
                let cell = &term.grid()[Point::new(cursor.line, Column(c))];
                if !cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    arrow_count += 1;
                }
            }
        }

        let right_seq: &[u8] = if app_cursor { b"\x1bOC" } else { b"\x1b[C" };
        let left_seq: &[u8] = if app_cursor { b"\x1bOD" } else { b"\x1b[D" };

        let mut buf = Vec::new();

        // Send arrow keys to move cursor to end of selection + 1
        if target_col > cursor_col {
            for _ in 0..arrow_count {
                buf.extend_from_slice(right_seq);
            }
        } else if target_col < cursor_col {
            for _ in 0..arrow_count {
                buf.extend_from_slice(left_seq);
            }
        }

        // Send backspaces for each character in the selection
        buf.extend(std::iter::repeat_n(0x7f, char_count)); // DEL / Backspace

        // Clear selection
        term.selection = None;

        drop(term);
        self.send_bytes(&buf);
        true
    }

    /// True if the active app wants drag/motion events (DEC modes 1002/1003).
    /// Press/release alone (1000) is implied by `is_mouse_mode()`.
    pub fn supports_mouse_drag(&self) -> bool {
        if self.transport.uses_mouse_backend() {
            return true;
        }
        let term = self.term.lock();
        term.mode()
            .intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION)
    }

    /// Forward a mouse button press or release to the PTY.
    /// `button` is 0=left, 1=middle, 2=right. `modifiers` is the OR of 4 (shift),
    /// 8 (alt/meta), 16 (control). Coordinates are 0-based cells.
    pub fn send_mouse_button(
        &self,
        button: u8,
        pressed: bool,
        col: usize,
        row: usize,
        modifiers: u8,
    ) {
        let use_sgr = if self.transport.uses_mouse_backend() {
            true
        } else {
            let term = self.term.lock();
            term.mode().contains(TermMode::SGR_MOUSE)
        };

        let cb = (button & 0b11) | (modifiers & 0b1_1100);
        let buf: Vec<u8> = if use_sgr {
            let action = if pressed { 'M' } else { 'm' };
            format!("\x1b[<{};{};{}{}", cb, col + 1, row + 1, action).into_bytes()
        } else {
            // Legacy X10/normal format: release reports button=3, no SGR distinction.
            let legacy_cb = if pressed {
                cb
            } else {
                3 | (modifiers & 0b1_1100)
            };
            vec![
                0x1b,
                b'[',
                b'M',
                legacy_cb.saturating_add(32),
                (col as u8).saturating_add(33),
                (row as u8).saturating_add(33),
            ]
        };
        self.send_bytes(&buf);
    }

    /// Forward a drag (button-held motion) event to the PTY.
    /// Caller should gate on `supports_mouse_drag()`.
    pub fn send_mouse_drag(&self, button: u8, col: usize, row: usize, modifiers: u8) {
        let use_sgr = if self.transport.uses_mouse_backend() {
            true
        } else {
            let term = self.term.lock();
            term.mode().contains(TermMode::SGR_MOUSE)
        };

        // Motion bit = 32 (0x20) added to button code.
        let cb = (button & 0b11) | (modifiers & 0b1_1100) | 32;
        let buf: Vec<u8> = if use_sgr {
            format!("\x1b[<{};{};{}M", cb, col + 1, row + 1).into_bytes()
        } else {
            vec![
                0x1b,
                b'[',
                b'M',
                cb.saturating_add(32),
                (col as u8).saturating_add(33),
                (row as u8).saturating_add(33),
            ]
        };
        self.send_bytes(&buf);
    }

    /// Send scroll events to PTY as a single batched write.
    /// button: 64 for scroll up, 65 for scroll down
    pub fn send_mouse_scroll(&self, button: u8, col: usize, row: usize, count: usize) {
        if count == 0 {
            return;
        }

        let use_sgr = if self.transport.uses_mouse_backend() {
            true
        } else {
            let term = self.term.lock();
            term.mode().contains(TermMode::SGR_MOUSE)
        };

        let mut buf = Vec::new();
        for _ in 0..count {
            if use_sgr {
                // SGR format: \x1b[<button;col;rowM
                buf.extend_from_slice(
                    format!("\x1b[<{};{};{}M", button, col + 1, row + 1).as_bytes(),
                );
            } else {
                // Legacy format: \x1b[M + (button+32) + (col+33) + (row+33)
                buf.extend_from_slice(&[
                    0x1b,
                    b'[',
                    b'M',
                    button.saturating_add(32),
                    (col as u8).saturating_add(33),
                    (row as u8).saturating_add(33),
                ]);
            }
        }
        self.send_bytes(&buf);
    }

    /// Move cursor to clicked column by sending arrow key sequences to the PTY.
    /// `target_col` is the visual column, `visual_row` is the visual row from pixel_to_cell().
    /// Only moves if the click is on the cursor's row and not scrolled into history.
    pub fn move_cursor_to_click(&self, target_col: usize, visual_row: i32) {
        let term = self.term.lock();

        let display_offset = term.grid().display_offset();
        if display_offset != 0 {
            return;
        }

        let cursor = term.grid().cursor.point;
        let cursor_visual_row = cursor.line.0; // equals buffer line when display_offset == 0

        if visual_row != cursor_visual_row {
            return;
        }

        let cursor_col = cursor.column.0;
        let cols = term.grid().columns();
        let target_col = target_col.min(cols.saturating_sub(1));

        if cursor_col == target_col {
            return;
        }

        let app_cursor = term.mode().contains(TermMode::APP_CURSOR);

        // Count logical characters (arrow presses) between cursor and target,
        // skipping WIDE_CHAR_SPACER cells (second cell of wide chars).
        let (start, end, going_right) = if target_col > cursor_col {
            (cursor_col, target_col, true)
        } else {
            (target_col, cursor_col, false)
        };

        let mut arrow_count = 0usize;
        for c in start..end {
            let cell = &term.grid()[Point::new(cursor.line, Column(c))];
            if !cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                arrow_count += 1;
            }
        }

        if arrow_count == 0 {
            return;
        }

        let arrow_seq: &[u8] = if going_right {
            if app_cursor { b"\x1bOC" } else { b"\x1b[C" }
        } else if app_cursor {
            b"\x1bOD"
        } else {
            b"\x1b[D"
        };

        let mut buf = Vec::with_capacity(arrow_seq.len() * arrow_count);
        for _ in 0..arrow_count {
            buf.extend_from_slice(arrow_seq);
        }

        drop(term);
        self.send_bytes(&buf);
    }
}

/// Check if a process has any child processes.
///
/// On Linux, this reads `/proc/<pid>/task/*/children` directly — sub-millisecond,
/// safe to call synchronously from UI handlers (e.g. click / key-down).
/// On other Unix, falls back to `pgrep -P` (~5–20 ms fork+exec).
/// On non-Unix, always returns false.
#[cfg(target_os = "linux")]
pub fn has_child_processes(pid: u32) -> bool {
    let task_dir = format!("/proc/{}/task", pid);
    let Ok(entries) = std::fs::read_dir(&task_dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let Some(tid) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let path = format!("/proc/{}/task/{}/children", pid, tid);
        if let Ok(s) = std::fs::read_to_string(&path) {
            if !s.trim().is_empty() {
                return true;
            }
        }
    }
    false
}

#[cfg(all(unix, not(target_os = "linux")))]
pub fn has_child_processes(pid: u32) -> bool {
    std::process::Command::new("pgrep")
        .args(["-P", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(not(unix))]
pub fn has_child_processes(_pid: u32) -> bool {
    false
}

// ── ANSI snapshot serialization ────────────────────────────────────────────

/// Tracked SGR state to minimize escape sequences in snapshot output.
#[derive(Clone, Default, PartialEq)]
struct SgrState {
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    inverse: bool,
    strikeout: bool,
    fg: Option<Color>,
    bg: Option<Color>,
}

/// Serialize the visible terminal grid to ANSI escape sequences.
fn grid_to_ansi(term: &Term<ZedEventListener>) -> Vec<u8> {
    let grid = term.grid();
    let screen_lines = grid.screen_lines();
    let cols = grid.columns();
    let cursor = term.grid().cursor.point;
    let cursor_hidden = !term.mode().contains(TermMode::SHOW_CURSOR);

    // Generous pre-allocation
    let mut buf = Vec::with_capacity(screen_lines * cols * 4);

    // Clear viewport, then clear scrollback history, then home cursor.
    // `\x1b[2J` alone scrolls the old viewport into history (alacritty's
    // `clear_viewport` calls `scroll_up`), so successive snapshots would stack
    // old content into the remote client's scrollback and the user would see
    // their output duplicated when scrolling up. `\x1b[3J` (ED 3 = erase saved
    // lines) drops the history alacritty just pushed, leaving a clean grid
    // before the snapshot body renders.
    buf.extend_from_slice(b"\x1b[2J\x1b[3J\x1b[H");

    let default_fg = Color::Named(NamedColor::Foreground);
    let default_bg = Color::Named(NamedColor::Background);

    let mut current = SgrState::default();

    for row in 0..screen_lines as i32 {
        // Position cursor at start of row
        write_csi_pos(&mut buf, row + 1, 1);

        let mut col_idx = 0usize;
        while col_idx < cols {
            let cell = &grid[Point::new(Line(row), Column(col_idx))];

            // Skip wide char spacer cells
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                col_idx += 1;
                continue;
            }

            // Determine desired SGR state
            let desired = SgrState {
                bold: cell.flags.contains(Flags::BOLD),
                dim: cell.flags.contains(Flags::DIM),
                italic: cell.flags.contains(Flags::ITALIC),
                underline: cell.flags.intersects(Flags::ALL_UNDERLINES),
                inverse: cell.flags.contains(Flags::INVERSE),
                strikeout: cell.flags.contains(Flags::STRIKEOUT),
                fg: if cell.fg == default_fg {
                    None
                } else {
                    Some(cell.fg)
                },
                bg: if cell.bg == default_bg {
                    None
                } else {
                    Some(cell.bg)
                },
            };

            if desired != current {
                emit_sgr(&mut buf, &desired);
                current = desired;
            }

            // Write the character
            let c = cell.c;
            if c == '\0' || c == ' ' {
                buf.push(b' ');
            } else {
                let mut utf8_buf = [0u8; 4];
                let encoded = c.encode_utf8(&mut utf8_buf);
                buf.extend_from_slice(encoded.as_bytes());
            }

            col_idx += 1;
        }
    }

    // Reset attributes
    buf.extend_from_slice(b"\x1b[0m");

    // Position cursor
    write_csi_pos(&mut buf, cursor.line.0 + 1, cursor.column.0 as i32 + 1);

    // Hide cursor if needed
    if cursor_hidden {
        buf.extend_from_slice(b"\x1b[?25l");
    }

    buf
}

fn replay_prefix_before_alt_screen(bytes: &[u8]) -> Option<&[u8]> {
    const ALT_SCREEN_ENTERS: [&[u8]; 3] = [b"\x1b[?1049h", b"\x1b[?1047h", b"\x1b[?47h"];
    ALT_SCREEN_ENTERS
        .iter()
        .filter_map(|needle| find_bytes(bytes, needle).map(|idx| &bytes[..idx]))
        .next()
}

fn split_after_alt_screen_exit(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    const ALT_SCREEN_EXITS: [&[u8]; 3] = [b"\x1b[?1049l", b"\x1b[?1047l", b"\x1b[?47l"];
    let split = ALT_SCREEN_EXITS
        .iter()
        .filter_map(|needle| find_bytes(bytes, needle).map(|idx| idx + needle.len()))
        .filter(|split| *split < bytes.len())
        .min()?;
    Some(bytes.split_at(split))
}

fn contains_alt_screen_exit(bytes: &[u8]) -> bool {
    const ALT_SCREEN_EXITS: [&[u8]; 3] = [b"\x1b[?1049l", b"\x1b[?1047l", b"\x1b[?47l"];
    ALT_SCREEN_EXITS
        .iter()
        .any(|needle| byte_slice_contains(bytes, needle))
}

fn byte_slice_contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    find_bytes(haystack, needle).is_some()
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// Write CSI cursor position: `\x1b[{row};{col}H`
fn write_csi_pos(buf: &mut Vec<u8>, row: i32, col: i32) {
    use std::io::Write;
    let _ = write!(buf, "\x1b[{};{}H", row, col);
}

/// Emit a full SGR sequence from the desired state (always resets first).
fn emit_sgr(buf: &mut Vec<u8>, state: &SgrState) {
    use std::io::Write;

    buf.extend_from_slice(b"\x1b[0");

    if state.bold {
        buf.extend_from_slice(b";1");
    }
    if state.dim {
        buf.extend_from_slice(b";2");
    }
    if state.italic {
        buf.extend_from_slice(b";3");
    }
    if state.underline {
        buf.extend_from_slice(b";4");
    }
    if state.inverse {
        buf.extend_from_slice(b";7");
    }
    if state.strikeout {
        buf.extend_from_slice(b";9");
    }
    if let Some(ref color) = state.fg {
        push_color_sgr(buf, color, true);
    }
    if let Some(ref color) = state.bg {
        push_color_sgr(buf, color, false);
    }

    let _ = write!(buf, "m");
}

/// Append color SGR parameters (e.g. `;31` or `;38;5;123` or `;38;2;R;G;B`).
fn push_color_sgr(buf: &mut Vec<u8>, color: &Color, is_fg: bool) {
    use std::io::Write;

    match color {
        Color::Named(named) => {
            let code = named_color_sgr_code(named, is_fg);
            if let Some(code) = code {
                let _ = write!(buf, ";{}", code);
            }
        }
        Color::Indexed(idx) => {
            let base = if is_fg { 38 } else { 48 };
            let _ = write!(buf, ";{};5;{}", base, idx);
        }
        Color::Spec(rgb) => {
            let base = if is_fg { 38 } else { 48 };
            let _ = write!(buf, ";{};2;{};{};{}", base, rgb.r, rgb.g, rgb.b);
        }
    }
}

/// Map a NamedColor to its SGR code.
fn named_color_sgr_code(color: &NamedColor, is_fg: bool) -> Option<u8> {
    let code = match color {
        NamedColor::Black => 0,
        NamedColor::Red => 1,
        NamedColor::Green => 2,
        NamedColor::Yellow => 3,
        NamedColor::Blue => 4,
        NamedColor::Magenta => 5,
        NamedColor::Cyan => 6,
        NamedColor::White => 7,
        NamedColor::BrightBlack => 8,
        NamedColor::BrightRed => 9,
        NamedColor::BrightGreen => 10,
        NamedColor::BrightYellow => 11,
        NamedColor::BrightBlue => 12,
        NamedColor::BrightMagenta => 13,
        NamedColor::BrightCyan => 14,
        NamedColor::BrightWhite => 15,
        // Foreground/Background/Cursor are default colors, no SGR code
        _ => return None,
    };

    if code < 8 {
        Some(if is_fg { 30 + code } else { 40 + code })
    } else {
        // Bright colors: 90-97 / 100-107
        Some(if is_fg {
            90 + (code - 8)
        } else {
            100 + (code - 8)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct NullTransport;
    impl TerminalTransport for NullTransport {
        fn send_input(&self, _terminal_id: &str, _data: &[u8]) {}
        fn resize(&self, _terminal_id: &str, _cols: u16, _rows: u16) {}
        fn uses_mouse_backend(&self) -> bool {
            false
        }
    }

    fn terminal_line_text(terminal: &Terminal, row: i32) -> String {
        let term = terminal.term.lock();
        let grid = term.grid();
        let mut text = String::new();
        for col in 0..grid.columns() {
            text.push(grid[Point::new(Line(row), Column(col))].c);
        }
        text
    }

    #[test]
    fn take_unposted_notification_returns_osc_notification_exactly_once() {
        let transport = Arc::new(NullTransport);
        let terminal = Terminal::new(
            "test-id".to_string(),
            TerminalSize::default(),
            transport,
            "/tmp".to_string(),
        );

        // Nothing pending before any output.
        assert!(terminal.take_unposted_notification().is_none());

        // OSC 777 notify: ESC ] 777 ; notify ; title ; body BEL
        terminal.process_output(b"\x1b]777;notify;Claude Code;Turn complete\x07");

        let notif = terminal
            .take_unposted_notification()
            .expect("notification should be pending after OSC 777");
        assert_eq!(notif.title, "Claude Code");
        assert_eq!(notif.body, "Turn complete");
        // Drained: a second take returns nothing.
        assert!(terminal.take_unposted_notification().is_none());

        // OSC 9 marks a new pending notification again.
        terminal.process_output(b"\x1b]9;Build finished\x07");
        let notif = terminal.take_unposted_notification().expect("new OSC 9");
        assert_eq!(notif.body, "Build finished");
    }

    #[test]
    fn set_notification_does_not_mark_unposted() {
        let transport = Arc::new(NullTransport);
        let terminal = Terminal::new(
            "test-id".to_string(),
            TerminalSize::default(),
            transport,
            "/tmp".to_string(),
        );

        // The remote Notify action posts natively at the action site, so the
        // PTY drain must not report it again.
        terminal.set_notification("Codex".to_string(), "Turn complete".to_string());
        assert!(terminal.take_unposted_notification().is_none());
    }

    #[test]
    fn test_osc_title_set() {
        let transport = Arc::new(NullTransport);
        let terminal = Terminal::new(
            "test-id".to_string(),
            TerminalSize::default(),
            transport,
            "/tmp".to_string(),
        );

        // Feed OSC 0 (set title) sequence: ESC ] 0 ; MOJE_JMENO BEL
        let osc_data = b"\x1b]0;MOJE_JMENO\x07";
        terminal.process_output(osc_data);

        assert_eq!(terminal.title(), Some("MOJE_JMENO".to_string()));
    }

    #[test]
    fn test_osc_title_with_surrounding_data() {
        let transport = Arc::new(NullTransport);
        let terminal = Terminal::new(
            "test-id".to_string(),
            TerminalSize::default(),
            transport,
            "/tmp".to_string(),
        );

        // Simulate what dtach sends: clear screen + OSC title + some output
        let data = b"\x1b[H\x1b[J\x1b]0;MOJE_JMENO\x07DONE\r\n";
        terminal.process_output(data);

        assert_eq!(terminal.title(), Some("MOJE_JMENO".to_string()));
    }

    #[test]
    fn test_osc_title_split_across_chunks() {
        let transport = Arc::new(NullTransport);
        let terminal = Terminal::new(
            "test-id".to_string(),
            TerminalSize::default(),
            transport,
            "/tmp".to_string(),
        );

        // Split the OSC sequence across two process_output calls
        terminal.process_output(b"\x1b]0;MOJE");
        assert_eq!(terminal.title(), None); // Not complete yet

        terminal.process_output(b"_JMENO\x07");
        assert_eq!(terminal.title(), Some("MOJE_JMENO".to_string()));
    }

    #[test]
    fn test_osc_title_reset() {
        let transport = Arc::new(NullTransport);
        let terminal = Terminal::new(
            "test-id".to_string(),
            TerminalSize::default(),
            transport,
            "/tmp".to_string(),
        );

        // Set title
        terminal.process_output(b"\x1b]0;MOJE_JMENO\x07");
        assert_eq!(terminal.title(), Some("MOJE_JMENO".to_string()));

        // Reset title (OSC 0 with empty string => set_title(None) in alacritty)
        terminal.process_output(b"\x1b]0;\x07");
        // After reset, title should be cleared or set to empty
        let title = terminal.title();
        assert!(
            title.is_none() || title.as_deref() == Some(""),
            "title should be empty or None, got: {:?}",
            title
        );
    }

    // The resize authority is process-global; these tests share a mutex so
    // they don't observe each other's writes.
    static RESIZE_AUTH_TEST_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    #[test]
    fn resize_owner_defaults_to_local() {
        let _g = RESIZE_AUTH_TEST_LOCK.lock();
        reset_resize_authority();
        let transport = Arc::new(NullTransport);
        let terminal = Terminal::new(
            "t".into(),
            TerminalSize::default(),
            transport,
            String::new(),
        );
        assert!(terminal.is_resize_owner_local());
    }

    #[test]
    fn resize_owner_transitions() {
        let _g = RESIZE_AUTH_TEST_LOCK.lock();
        reset_resize_authority();
        let transport = Arc::new(NullTransport);
        let terminal = Terminal::new(
            "t".into(),
            TerminalSize::default(),
            transport,
            String::new(),
        );

        terminal.claim_resize_remote();
        assert!(!terminal.is_resize_owner_local());

        terminal.claim_resize_local();
        assert!(terminal.is_resize_owner_local());
    }

    #[test]
    fn resize_owner_is_process_global() {
        let _g = RESIZE_AUTH_TEST_LOCK.lock();
        reset_resize_authority();
        let transport = Arc::new(NullTransport);
        let term_a = Terminal::new(
            "a".into(),
            TerminalSize::default(),
            transport.clone(),
            String::new(),
        );
        let term_b = Terminal::new(
            "b".into(),
            TerminalSize::default(),
            transport,
            String::new(),
        );

        // Claiming remote on A flips authority for B as well.
        term_a.claim_resize_remote();
        assert!(!term_b.is_resize_owner_local());

        // Claiming local on B flips authority back for A.
        term_b.claim_resize_local();
        assert!(term_a.is_resize_owner_local());
    }

    #[test]
    fn resize_grid_only_does_not_call_transport() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct SpyTransport {
            resize_called: AtomicBool,
        }
        impl TerminalTransport for SpyTransport {
            fn send_input(&self, _: &str, _: &[u8]) {}
            fn resize(&self, _: &str, _: u16, _: u16) {
                self.resize_called.store(true, Ordering::Relaxed);
            }
            fn uses_mouse_backend(&self) -> bool {
                false
            }
        }

        let transport = Arc::new(SpyTransport {
            resize_called: AtomicBool::new(false),
        });
        let terminal = Terminal::new(
            "t".into(),
            TerminalSize::default(),
            transport.clone(),
            String::new(),
        );

        terminal.resize_grid_only(120, 40);
        assert!(!transport.resize_called.load(Ordering::Relaxed));
        assert_eq!(terminal.resize_state.lock().size.cols, 120);
        assert_eq!(terminal.resize_state.lock().size.rows, 40);
    }

    #[test]
    fn selection_state_preserves_negative_scrollback_rows() {
        let transport = Arc::new(NullTransport);
        let terminal = Terminal::new(
            "t".into(),
            TerminalSize {
                cols: 20,
                rows: 5,
                cell_width: 8.0,
                cell_height: 16.0,
            },
            transport,
            String::new(),
        );

        for i in 0..20 {
            terminal.process_output(format!("line {i}\r\n").as_bytes());
        }

        terminal.scroll_up(3);
        terminal.start_selection(0, 0, Side::Left);
        terminal.update_selection(4, 0, Side::Right);

        let state = terminal.selection_state.lock();
        assert_eq!(state.start, Some((0, -3)));
        assert_eq!(state.end, Some((4, -3)));
    }

    #[test]
    fn scrollback_capture_preserves_simple_spaces_as_text() {
        let transport = Arc::new(NullTransport);
        let terminal = Terminal::new(
            "t".into(),
            TerminalSize::default(),
            transport,
            String::new(),
        );

        terminal.process_output(b"assets        crates\r\n");

        let snapshot = terminal.capture_scrollback(100, None);
        let body = &snapshot[10..];

        assert!(
            body.windows(b"assets        crates".len())
                .any(|w| w == b"assets        crates"),
            "snapshot body should contain literal spacing: {:?}",
            String::from_utf8_lossy(body)
        );
        assert!(
            !body.windows(b"\x1b[8C".len()).any(|w| w == b"\x1b[8C"),
            "snapshot body should not encode simple spaces as cursor-forward escapes: {:?}",
            String::from_utf8_lossy(body)
        );
    }

    #[test]
    fn snapshot_capture_drains_pending_output_before_saving() {
        let dir = std::env::temp_dir().join(format!(
            "notmux-terminal-pending-snapshot-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let transport = Arc::new(NullTransport);
        let terminal = Terminal::new(
            "t".into(),
            TerminalSize::default(),
            transport,
            String::new(),
        );

        terminal.enqueue_output(b"pending output\r\n");
        let (snapshot, stats) =
            terminal.capture_scrollback_snapshot(&dir, "term", 100, None, SnapshotReason::AppQuit);
        let body = &snapshot[10..];

        assert_eq!(stats.pending_output_len, 0);
        assert!(
            body.windows(b"pending output".len())
                .any(|w| w == b"pending output"),
            "pending output should be included in snapshot: {:?}",
            String::from_utf8_lossy(body)
        );

        let _ = std::fs::remove_dir(dir);
    }

    #[test]
    fn alt_screen_snapshot_persists_only_launch_command() {
        let dir = std::env::temp_dir().join(format!(
            "notmux-terminal-alt-screen-snapshot-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let transport = Arc::new(NullTransport);
        let terminal = Terminal::new(
            "t".into(),
            TerminalSize::default(),
            transport,
            String::new(),
        );

        terminal.process_output(b"previous output\r\n$ nvim Cargo.toml\r\n");
        terminal.send_input("nvim Cargo.toml");
        terminal.send_bytes(b"\r");
        terminal.process_output(b"\x1b[?1049h\x1b[?1000h\x1b[?1006hTUI screen contents");

        assert!(terminal.is_alt_screen());

        let (snapshot, _stats) =
            terminal.capture_scrollback_snapshot(&dir, "term", 100, None, SnapshotReason::AppQuit);
        let body = &snapshot[10..];

        assert!(
            body.windows(b"nvim Cargo.toml".len())
                .any(|w| w == b"nvim Cargo.toml"),
            "launch command should be persisted: {:?}",
            String::from_utf8_lossy(body)
        );
        assert!(
            body.windows(b"previous output".len())
                .any(|w| w == b"previous output"),
            "normal-buffer output before alt-screen should be persisted: {:?}",
            String::from_utf8_lossy(body)
        );
        assert!(
            !body
                .windows(b"TUI screen contents".len())
                .any(|w| w == b"TUI screen contents"),
            "alt-screen output must not be persisted: {:?}",
            String::from_utf8_lossy(body)
        );
        assert!(
            !body.windows(b"\x1b[?1000h".len())
                .any(|w| w == b"\x1b[?1000h"),
            "mouse mode must not be persisted: {:?}",
            String::from_utf8_lossy(body)
        );

        let _ = std::fs::remove_dir(dir);
    }

    #[test]
    fn restored_scrollback_without_user_input_discards_spawned_prompt_on_capture() {
        let dir =
            std::env::temp_dir().join(format!("notmux-terminal-replay-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let transport = Arc::new(NullTransport);
        let terminal = Terminal::new(
            "t".into(),
            TerminalSize::default(),
            transport,
            String::new(),
        );

        terminal.restore_scrollback_with_initial_text(
            b"old output\r\n",
            b"\x1b[0m\x1b[7m *  History restored \x1b[0m\r\n\r\n",
        );
        terminal.process_output(b"fresh prompt % ");

        let (snapshot, stats) =
            terminal.capture_scrollback_snapshot(&dir, "term", 100, None, SnapshotReason::AppQuit);
        let body = &snapshot[10..];

        assert!(stats.restored_from_snapshot);
        assert!(!stats.had_user_input);
        assert!(
            body.windows(b"old output".len())
                .any(|w| w == b"old output"),
            "restored output should remain: {:?}",
            String::from_utf8_lossy(body)
        );
        assert!(
            !body.windows(b"History restored".len())
                .any(|w| w == b"History restored"),
            "restore marker should not be persisted before user input: {:?}",
            String::from_utf8_lossy(body)
        );
        assert!(
            !body.windows(b"fresh prompt".len())
                .any(|w| w == b"fresh prompt"),
            "spawned prompt should not be persisted before user input: {:?}",
            String::from_utf8_lossy(body)
        );

        let _ = std::fs::remove_dir(dir);
    }

    #[test]
    fn restored_scrollback_after_user_input_persists_visible_session_state() {
        let dir = std::env::temp_dir().join(format!(
            "notmux-terminal-replay-interaction-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let transport = Arc::new(NullTransport);
        let terminal = Terminal::new(
            "t".into(),
            TerminalSize::default(),
            transport,
            String::new(),
        );

        terminal.restore_scrollback_with_initial_text(
            b"old output\r\n",
            b"\x1b[0m\x1b[7m *  History restored \x1b[0m\r\n\r\n",
        );
        terminal.process_output(b"fresh prompt % ");
        terminal.send_input("ls\r");
        terminal.process_output(b"ls\r\n");

        let (snapshot, stats) =
            terminal.capture_scrollback_snapshot(&dir, "term", 100, None, SnapshotReason::AppQuit);
        let body = &snapshot[10..];

        assert!(!stats.restored_from_snapshot);
        assert!(stats.had_user_input);
        for expected in [
            b"old output" as &[u8],
            b"History restored",
            b"fresh prompt",
            b"ls",
        ] {
            assert!(
                body.windows(expected.len()).any(|w| w == expected),
                "session state should be serialized after user input: {:?}",
                String::from_utf8_lossy(body)
            );
        }

        let _ = std::fs::remove_dir(dir);
    }

    #[test]
    fn empty_restore_without_user_input_does_not_persist_marker() {
        let dir = std::env::temp_dir().join(format!(
            "notmux-terminal-empty-restore-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let transport = Arc::new(NullTransport);
        let terminal = Terminal::new(
            "t".into(),
            TerminalSize::default(),
            transport,
            String::new(),
        );

        terminal.restore_scrollback_with_initial_text(
            b"",
            b"\x1b[0m\x1b[7m *  History restored \x1b[0m\r\n\r\n",
        );
        terminal.process_output(b"fresh prompt % ");

        let (snapshot, stats) =
            terminal.capture_scrollback_snapshot(&dir, "term", 100, None, SnapshotReason::AppQuit);

        assert!(snapshot.is_empty());
        assert!(stats.restored_from_snapshot);
        assert!(!stats.had_user_input);

        let _ = std::fs::remove_dir(dir);
    }

    #[test]
    fn restored_scrollback_initial_text_appends_marker_after_raw_output() {
        let dir = std::env::temp_dir().join(format!(
            "notmux-terminal-replay-order-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let transport = Arc::new(NullTransport);
        let terminal = Terminal::new(
            "t".into(),
            TerminalSize::default(),
            transport,
            String::new(),
        );

        terminal.restore_scrollback_with_initial_text(
            b"history output",
            b"\r\n\x1b[0m\x1b[7m *  History restored \x1b[0m\r\n\r\n",
        );
        terminal.send_input("x");
        terminal.process_output(b"new output");

        let snapshot = terminal.capture_scrollback_merged(&dir, "term", 100, None);
        let body = &snapshot[10..];
        let history = body
            .windows(b"history output".len())
            .position(|w| w == b"history output")
            .expect("history output should be present");
        let marker = body
            .windows(b"History restored".len())
            .position(|w| w == b"History restored")
            .expect("restore marker should be present");
        let new_output = body
            .windows(b"new output".len())
            .position(|w| w == b"new output")
            .expect("new output should be present");

        assert!(history < marker);
        assert!(marker < new_output);

        let _ = std::fs::remove_dir(dir);
    }

    #[test]
    fn restored_scrollback_marker_does_not_overwrite_text_below_cursor() {
        let transport = Arc::new(NullTransport);
        let terminal = Terminal::new(
            "t".into(),
            TerminalSize::default(),
            transport,
            String::new(),
        );

        terminal.restore_scrollback_with_initial_text(
            b"top\r\nmiddle\r\nbottom\x1b[1;1H",
            b"\r\n\x1b[0m\x1b[7m *  History restored \x1b[0m\r\n\r\n",
        );

        assert!(
            terminal_line_text(&terminal, 1).contains("middle"),
            "line below cursor should survive restore marker"
        );
        assert!(
            terminal_line_text(&terminal, 2).contains("bottom"),
            "lower line should survive restore marker"
        );
        assert!(
            (3..8).any(|row| terminal_line_text(&terminal, row).contains("History restored")),
            "restore marker should be moved below restored content"
        );
    }

    #[test]
    fn alt_screen_exit_prompt_does_not_overwrite_normal_buffer_lines_below_cursor() {
        let transport = Arc::new(NullTransport);
        let terminal = Terminal::new(
            "t".into(),
            TerminalSize::default(),
            transport,
            String::new(),
        );

        terminal.process_output(b"top\r\nmiddle\r\nbottom\x1b[1;1H");
        terminal.process_output(b"\x1b[?1049hclaude screen\x1b[?1049l");
        terminal.process_output(b"prompt % ");

        assert!(
            terminal_line_text(&terminal, 1).contains("middle"),
            "line below cursor should survive app exit"
        );
        assert!(
            terminal_line_text(&terminal, 2).contains("bottom"),
            "lower line should survive app exit"
        );
        assert!(
            (3..8).any(|row| terminal_line_text(&terminal, row).contains("prompt % ")),
            "prompt should be moved below preserved normal-buffer content"
        );
    }

    #[test]
    fn alt_screen_exit_prompt_in_same_chunk_is_guarded() {
        let transport = Arc::new(NullTransport);
        let terminal = Terminal::new(
            "t".into(),
            TerminalSize::default(),
            transport,
            String::new(),
        );

        terminal.process_output(b"top\r\nmiddle\r\nbottom\x1b[1;1H");
        terminal.process_output(b"\x1b[?1049hclaude screen\x1b[?1049lprompt % ");

        assert!(
            terminal_line_text(&terminal, 1).contains("middle"),
            "line below cursor should survive app exit"
        );
        assert!(
            terminal_line_text(&terminal, 2).contains("bottom"),
            "lower line should survive app exit"
        );
        assert!(
            (3..8).any(|row| terminal_line_text(&terminal, row).contains("prompt % ")),
            "prompt should be guarded even when it arrives with the alt-screen exit"
        );
    }

    #[test]
    fn pending_output_uses_alt_screen_exit_cursor_guard_when_drained() {
        let transport = Arc::new(NullTransport);
        let terminal = Terminal::new(
            "t".into(),
            TerminalSize::default(),
            transport,
            String::new(),
        );

        terminal.process_output(b"top\r\nmiddle\r\nbottom\x1b[1;1H");
        terminal.enqueue_output(b"\x1b[?1049hclaude screen\x1b[?1049lprompt % ");
        terminal.drain_pending_output_for_snapshot();

        assert!(
            terminal_line_text(&terminal, 1).contains("middle"),
            "line below cursor should survive queued app output"
        );
        assert!(
            terminal_line_text(&terminal, 2).contains("bottom"),
            "lower line should survive queued app output"
        );
        assert!(
            (3..8).any(|row| terminal_line_text(&terminal, row).contains("prompt % ")),
            "queued prompt should be moved below preserved normal-buffer content"
        );
    }

    #[test]
    fn replay_output_suppresses_terminal_query_responses() {
        struct SpyTransport {
            writes: Mutex<Vec<Vec<u8>>>,
        }
        impl TerminalTransport for SpyTransport {
            fn send_input(&self, _: &str, data: &[u8]) {
                self.writes.lock().push(data.to_vec());
            }
            fn resize(&self, _: &str, _: u16, _: u16) {}
            fn uses_mouse_backend(&self) -> bool {
                false
            }
        }

        let transport = Arc::new(SpyTransport {
            writes: Mutex::new(Vec::new()),
        });
        let terminal = Terminal::new(
            "t".into(),
            TerminalSize::default(),
            transport.clone(),
            String::new(),
        );

        terminal.process_output(b"\x1b[6n\x1b]10;?\x1b\\");
        assert!(
            !transport.writes.lock().is_empty(),
            "live output should answer terminal queries"
        );
        transport.writes.lock().clear();

        terminal.replay_output(b"\x1b[6n\x1b]10;?\x1b\\");
        assert!(
            transport.writes.lock().is_empty(),
            "replayed history must not send query responses to the live shell"
        );
    }

    /// Helper: create a terminal and write text to it, returns detected URLs
    fn detect_urls_in(text: &str, cols: u16) -> Vec<DetectedLink> {
        let transport = Arc::new(NullTransport);
        let size = TerminalSize {
            cols,
            rows: 24,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        let terminal = Terminal::new("test".into(), size, transport, "/tmp".into());
        terminal.process_output(text.as_bytes());
        terminal.detect_urls()
    }

    #[test]
    fn detect_url_wrapped_with_padding() {
        // TUI writes a URL with a decoration prefix, URL fills nearly the
        // whole row, then continues on next line with matching indentation.
        // No WRAPLINE flag — the TUI manages wrapping itself.
        // Row 1: "- https://claude.ai/code/sess_ABC" (33 chars)
        // Row 2: "  DEF123" + padding
        // cols=36 so row 1 is nearly full (33+3 >= 36).
        let links = detect_urls_in("- https://claude.ai/code/sess_ABC\r\n  DEF123\r\n", 36);
        assert_eq!(links.len(), 2, "URL spans two rows: {:?}", links);
        assert_eq!(links[0].text, "https://claude.ai/code/sess_ABCDEF123");
        assert_eq!(links[0].col, 2);
        assert_eq!(links[1].text, "https://claude.ai/code/sess_ABCDEF123");
        assert_eq!(links[1].col, 2);
        assert_eq!(links[1].line, 1);
    }

    #[test]
    fn detect_url_wrapped_with_leading_padding() {
        // TUI adds leading spaces on the continuation line for alignment
        // Row 1: "  https://claude.ai/code/sess_ABC" (33 chars) + padding
        // Row 2: "  DEF123" + padding
        // cols=36 so row 1 is nearly full (33+3 >= 36).
        let links = detect_urls_in("  https://claude.ai/code/sess_ABC\r\n  DEF123\r\n", 36);
        assert_eq!(links.len(), 2, "URL spans two rows: {:?}", links);
        assert_eq!(links[0].text, "https://claude.ai/code/sess_ABCDEF123");
        assert_eq!(links[0].col, 2); // starts after 2 spaces
        assert_eq!(links[1].text, "https://claude.ai/code/sess_ABCDEF123");
        assert_eq!(links[1].col, 2); // continuation also at col 2
        assert_eq!(links[1].line, 1);
    }

    #[test]
    fn detect_url_not_wrapped_when_next_line_more_indented() {
        // Next line has more leading spaces than the first line —
        // the extra indentation means it's NOT a URL continuation.
        // Reproduces: "   1. zkusí https://api.postmarkapp.com\n      (oficiální API)"
        let links = detect_urls_in(
            "   1. text https://api.postmarkapp.com\r\n      (next line)\r\n",
            50,
        );
        assert_eq!(
            links.len(),
            1,
            "URL should NOT merge with next line: {:?}",
            links
        );
        assert_eq!(links[0].text, "https://api.postmarkapp.com");
    }

    #[test]
    fn detect_url_single_line_not_affected() {
        // Single-line URL should still work normally
        let links = detect_urls_in("visit https://example.com/path here\r\n", 80);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].text, "https://example.com/path");
        assert_eq!(links[0].col, 6);
        assert_eq!(links[0].line, 0);
    }

    #[test]
    fn detect_duplicate_urls_get_different_wrap_groups() {
        // Same URL on two separate lines should get different wrap_groups
        // so hovering one doesn't highlight the other.
        let links = detect_urls_in(
            "https://github.com/org/repo/pull/381\r\n\
             https://github.com/org/repo/pull/381\r\n",
            80,
        );
        assert_eq!(links.len(), 2, "Should detect two URLs: {:?}", links);
        assert_ne!(
            links[0].wrap_group, links[1].wrap_group,
            "Duplicate URLs must have different wrap_groups for independent hover"
        );
    }

    #[test]
    fn detect_duplicate_urls_separated_by_blank_line() {
        // Same URL separated by a blank line
        let links = detect_urls_in(
            "https://github.com/org/repo/pull/381\r\n\
             \r\n\
             https://github.com/org/repo/pull/381\r\n",
            80,
        );
        assert_eq!(links.len(), 2, "Should detect two URLs: {:?}", links);
        assert_ne!(
            links[0].wrap_group, links[1].wrap_group,
            "Duplicate URLs must have different wrap_groups"
        );
    }

    #[test]
    fn detect_duplicate_url_wrapped_then_whole() {
        // First URL wraps across two lines (TUI-style padding),
        // second URL appears whole on a later line.
        // This reproduces the real scenario from PR creation output.
        let url = "https://github.com/contember/webmaster/pull/381";
        let links = detect_urls_in(
            &format!(
                "Summary\r\n\
                 prefix {url}\r\n\
                 \r\n\
                 PR created:\r\n\
                 {url}\r\n"
            ),
            50,
        );
        let url_links: Vec<&DetectedLink> = links.iter().filter(|l| l.text == url).collect();
        // Wrapped URL produces 2 segments + standalone URL = 3 total
        assert!(
            url_links.len() >= 3,
            "Expected wrapped (2 segments) + standalone (1): {:?}",
            url_links
        );
        let wrapped_group = url_links[0].wrap_group;
        // All wrapped segments share the same group
        assert_eq!(
            url_links[0].wrap_group, url_links[1].wrap_group,
            "Wrapped segments should share wrap_group"
        );
        // Standalone URL has a different group
        let standalone = url_links.last().unwrap();
        assert_ne!(
            wrapped_group, standalone.wrap_group,
            "Standalone URL must have different wrap_group than wrapped one"
        );
    }

    #[test]
    fn detect_duplicate_url_after_colon_prefix() {
        // "PR created:" ends with ':' which is a url_char.
        // The next line starts with a URL. Visual wrap detection should NOT
        // merge them — or if it does, they must still get different wrap_groups.
        let url = "https://github.com/org/repo/pull/381";
        let links = detect_urls_in(
            &format!(
                "{url}\r\n\
                 \r\n\
                 PR created:\r\n\
                 {url}\r\n"
            ),
            80,
        );
        let url_links: Vec<&DetectedLink> = links.iter().filter(|l| l.text == url).collect();
        assert_eq!(
            url_links.len(),
            2,
            "Should have exactly 2 URL matches: {:?}",
            url_links
        );
        assert_ne!(
            url_links[0].wrap_group, url_links[1].wrap_group,
            "URLs must have different wrap_groups even when preceded by colon"
        );
    }

    #[test]
    fn detect_url_not_wrapped_when_next_line_starts_with_word() {
        // "Press ENTER..." is natural language text, not a URL continuation.
        // The URL ends with alphanumeric chars and "Press" starts with one,
        // but the word-followed-by-space heuristic should prevent merging.
        let links = detect_urls_in(
            "Login at:\r\nhttps://www.npmjs.com/login?next=/login/cli/d907c402-4ad4-474c-a183-16ae52157acf\r\nPress ENTER to open in the browser...\r\n",
            100,
        );
        assert_eq!(links.len(), 1, "Should detect exactly one URL: {:?}", links);
        assert_eq!(
            links[0].text,
            "https://www.npmjs.com/login?next=/login/cli/d907c402-4ad4-474c-a183-16ae52157acf"
        );
    }

    #[test]
    fn detect_url_not_wrapped_when_next_line_word_after_wrapline() {
        // URL wraps via WRAPLINE (fills terminal width), then next line
        // after the wrap tail starts with a word — should not merge.
        let url =
            "https://www.npmjs.com/login?next=/login/cli/d907c402-4ad4-474c-a183-16ae52157acf";
        let links = detect_urls_in(
            &format!("{url}\r\nPress ENTER to open in the browser...\r\n"),
            60, // force URL to wrap via WRAPLINE
        );
        let url_links: Vec<&DetectedLink> = links.iter().filter(|l| l.text == url).collect();
        assert!(!url_links.is_empty(), "Should detect the URL: {:?}", links);
        // "Press" should NOT be part of any detected link
        assert!(
            links.iter().all(|l| !l.text.contains("Press")),
            "No link should contain 'Press': {:?}",
            links
        );
    }

    #[test]
    fn detect_url_not_merged_with_remote_prefix() {
        // Git push output: URL on a line that doesn't fill the terminal width.
        // The "remote:" on the next line must NOT be merged as a continuation.
        let links = detect_urls_in(
            "remote:       https://github.com/contember/dotaz/pull/new/fixes\r\nremote:\r\n",
            80,
        );
        assert_eq!(links.len(), 1, "Should detect exactly one URL: {:?}", links);
        assert_eq!(
            links[0].text,
            "https://github.com/contember/dotaz/pull/new/fixes"
        );
    }

    #[test]
    fn detect_url_not_merged_with_label_suffix() {
        // Even when the URL line nearly fills the terminal, a continuation
        // ending with ':' (label pattern) must not be merged.
        let links = detect_urls_in(
            "https://github.com/contember/dotaz/pull/new/fixes\r\nremote:\r\n",
            52, // URL is 50 chars, nearly fills 52-col terminal
        );
        assert_eq!(
            links.len(),
            1,
            "Label-like 'remote:' must not be merged: {:?}",
            links
        );
        assert_eq!(
            links[0].text,
            "https://github.com/contember/dotaz/pull/new/fixes"
        );
    }

    #[test]
    fn detect_url_wrapped_with_trailing_text() {
        // URL wraps across two lines, continuation line has non-URL text after
        // the URL part (e.g. " — S3 bucket").  The first token of the
        // continuation contains '/' so it should still be recognised as a URL
        // continuation.
        let links = detect_urls_in(
            "    - #61 https://github.com/contember/npi-infrastru\r\n    cture/pull/61 \u{2014} S3 bucket\r\n",
            55,
        );
        let url_links: Vec<&DetectedLink> = links
            .iter()
            .filter(|l| l.text == "https://github.com/contember/npi-infrastructure/pull/61")
            .collect();
        assert!(
            !url_links.is_empty(),
            "Should detect the full wrapped URL: {:?}",
            links
        );
    }

    #[test]
    fn detect_url_wrapped_tui_narrow_layout() {
        // TUI uses a narrower layout than the terminal width.
        // URL doesn't reach the terminal edge but does reach the end
        // of the TUI's visible content.  Phase 2 should still extend.
        // Terminal is 55 cols, but TUI content only uses ~42 cols.
        let links = detect_urls_in(
            "\u{2514}  https://github.com/NPI-Cloud/npi-inf\r\n   rastructure/pull/64\r\n",
            55,
        );
        let url_links: Vec<&DetectedLink> = links
            .iter()
            .filter(|l| l.text == "https://github.com/NPI-Cloud/npi-infrastructure/pull/64")
            .collect();
        assert!(
            url_links.len() >= 2,
            "URL should span two rows even when TUI layout is narrower than terminal: {:?}",
            links
        );
    }

    #[test]
    fn detect_url_not_extended_by_list_marker() {
        // URL on its own line followed by a list item starting with "- ".
        // The "-" is a url_char but it's a list marker, not a URL
        // continuation.  Must not extend.
        let links = detect_urls_in(
            "  https://github.com/contember/dotaz/pull/2\r\n  - Format check passes\r\n",
            55,
        );
        assert_eq!(
            links.len(),
            1,
            "Should not extend into list marker: {:?}",
            links
        );
        assert_eq!(links[0].text, "https://github.com/contember/dotaz/pull/2");
    }

    #[test]
    fn detect_url_extension_stops_after_trailing_trim() {
        // URL continuation ends with ")" which gets trimmed.  The "2." on
        // the following line must NOT be absorbed as another extension.
        // Simulates prose: "...npi-inf +\nrastructure/pull/65)\n2. https://..."
        let links = detect_urls_in(
            "  https://github.com/NPI-Cloud/npi-inf\r\n  rastructure/pull/65)\r\n  2. next item\r\n",
            42,
        );
        let url_links: Vec<&DetectedLink> = links
            .iter()
            .filter(|l| l.text.starts_with("https://github.com/NPI-Cloud/npi-inf"))
            .collect();
        // Should have 2 segments (line 0 + line 1), NOT 3
        assert_eq!(
            url_links.len(),
            2,
            "Should not extend past trimmed ')' into '2.': {:?}",
            links
        );
        assert_eq!(
            url_links[0].text,
            "https://github.com/NPI-Cloud/npi-infrastructure/pull/65"
        );
    }

    #[test]
    fn detect_url_not_extended_into_numbered_list_item() {
        // Numbered list where each item has a URL.  The `2` from "2. https://..."
        // must NOT be absorbed as a continuation of the first URL.
        let links = detect_urls_in(
            "1. https://github.com/contember/dotaz/pull/2\r\n2. https://github.com/NPI-Cloud/npi-infrastr\r\n   ucture/pull/65\r\n",
            46,
        );
        // First URL should be exactly pull/2, not pull/22
        let first: Vec<&DetectedLink> = links
            .iter()
            .filter(|l| l.text == "https://github.com/contember/dotaz/pull/2")
            .collect();
        assert!(
            !first.is_empty(),
            "First URL should be pull/2, not absorb '2' from next line: {:?}",
            links
        );
        // Second URL should also be detected
        let second: Vec<&DetectedLink> = links
            .iter()
            .filter(|l| l.text.contains("npi-infrastructure/pull/65"))
            .collect();
        assert!(
            !second.is_empty(),
            "Second URL should be detected: {:?}",
            links
        );
    }

    #[test]
    fn detect_url_not_extended_by_prose_word() {
        // URL on a dash-list line, next line is also a dash-list item
        // with prose text.  "next" is a url_char word but must NOT be
        // absorbed as URL continuation.
        let links = detect_urls_in(
            "- https://github.com/contember/dotaz/pull/2\r\n- next item without URL\r\n",
            46,
        );
        assert_eq!(links.len(), 1, "Should not extend into 'next': {:?}", links);
        assert_eq!(links[0].text, "https://github.com/contember/dotaz/pull/2");
    }

    #[test]
    fn detect_url_uuid_continuation_with_trailing_prose() {
        // URL wraps mid-UUID, continuation line has prose after the UUID
        // fragment.  The UUID part (digits + hex letters + dashes) must
        // still be recognised as URL continuation despite spaces in the
        // line — digits distinguish it from a prose word.
        let links = detect_urls_in(
            "  http://localhost:19400/s/1f41d02d-6105-45fb-b3\r\n  b1-4b56ae4d869f \u{2014} take your time.\r\n",
            50,
        );
        let url_links: Vec<&DetectedLink> = links
            .iter()
            .filter(|l| l.text == "http://localhost:19400/s/1f41d02d-6105-45fb-b3b1-4b56ae4d869f")
            .collect();
        assert!(
            url_links.len() >= 2,
            "UUID continuation should be detected across wrapped lines: {:?}",
            links
        );
    }
}
