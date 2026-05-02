use std::time::{Duration, SystemTime};

pub const OSC_PREFIX: &str = "777;vryn;";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandTrackingEvent {
    PromptStart { cwd: Option<String> },
    CommandStart { command: String },
    CommandExecuted,
    CommandFinished { exit_code: Option<i32> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandStatus {
    Running,
    Success,
    Error,
    Unknown,
}

#[derive(Clone, Debug)]
pub struct TrackedCommand {
    pub id: u64,
    pub command: String,
    pub status: CommandStatus,
    pub prompt_row: i32,
    pub command_row: i32,
    pub output_start_row: Option<i32>,
    pub output_end_row: Option<i32>,
    pub cwd: Option<String>,
    pub exit_code: Option<i32>,
    pub started_at: SystemTime,
    pub finished_at: Option<SystemTime>,
    pub duration: Option<Duration>,
}

#[derive(Debug, Default)]
pub struct CommandSequenceParser {
    pending: Vec<u8>,
}

impl CommandSequenceParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, data: &[u8]) -> Vec<CommandTrackingEvent> {
        self.pending.extend_from_slice(data);
        let mut events = Vec::new();

        loop {
            let Some(start) = find_bytes(&self.pending, b"\x1b]") else {
                if self.pending.len() > 4096 {
                    self.pending.clear();
                }
                break;
            };

            if start > 0 {
                self.pending.drain(..start);
            }

            let Some((end, terminator_len)) = find_osc_end(&self.pending[2..]) else {
                if self.pending.len() > 8192 {
                    self.pending.clear();
                }
                break;
            };

            let payload_end = 2 + end;
            let payload = String::from_utf8_lossy(&self.pending[2..payload_end]).to_string();
            self.pending.drain(..payload_end + terminator_len);

            if let Some(event) = parse_payload(&payload) {
                events.push(event);
            }
        }

        events
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn find_osc_end(data: &[u8]) -> Option<(usize, usize)> {
    let bel = data.iter().position(|b| *b == b'\x07');
    let st = find_bytes(data, b"\x1b\\");

    match (bel, st) {
        (Some(bel), Some(st)) if bel < st => Some((bel, 1)),
        (Some(_), Some(st)) => Some((st, 2)),
        (Some(bel), None) => Some((bel, 1)),
        (None, Some(st)) => Some((st, 2)),
        (None, None) => None,
    }
}

fn parse_payload(payload: &str) -> Option<CommandTrackingEvent> {
    let rest = payload.strip_prefix(OSC_PREFIX)?;
    let (kind, value) = rest.split_once(';').unwrap_or((rest, ""));
    match kind {
        "A" => Some(CommandTrackingEvent::PromptStart {
            cwd: non_empty(value),
        }),
        "B" => Some(CommandTrackingEvent::CommandStart {
            command: value.trim().to_string(),
        }),
        "C" => Some(CommandTrackingEvent::CommandExecuted),
        "D" => Some(CommandTrackingEvent::CommandFinished {
            exit_code: value.trim().parse::<i32>().ok(),
        }),
        _ => None,
    }
}

fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

#[derive(Debug)]
pub struct CommandTracker {
    next_id: u64,
    current: Option<TrackedCommand>,
    commands: Vec<TrackedCommand>,
    cwd: Option<String>,
    prompt_row: Option<i32>,
    prompt_active: bool,
}

impl Default for CommandTracker {
    fn default() -> Self {
        Self {
            next_id: 1,
            current: None,
            commands: Vec::new(),
            cwd: None,
            prompt_row: None,
            prompt_active: false,
        }
    }
}

impl CommandTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn handle_event(&mut self, event: CommandTrackingEvent, row: i32, now: SystemTime) {
        match event {
            CommandTrackingEvent::PromptStart { cwd } => {
                if let Some(cwd) = cwd {
                    self.cwd = Some(cwd);
                }
                self.prompt_row = Some(row);
                self.prompt_active = true;
            }
            CommandTrackingEvent::CommandStart { command } => {
                if command.trim().is_empty() {
                    return;
                }
                let prompt_row = self.prompt_row.unwrap_or(row);
                self.prompt_active = false;
                let command = TrackedCommand {
                    id: self.next_id,
                    command,
                    status: CommandStatus::Running,
                    prompt_row,
                    command_row: prompt_row,
                    output_start_row: None,
                    output_end_row: None,
                    cwd: self.cwd.clone(),
                    exit_code: None,
                    started_at: now,
                    finished_at: None,
                    duration: None,
                };
                self.next_id += 1;
                self.current = Some(command);
            }
            CommandTrackingEvent::CommandExecuted => {
                if let Some(command) = &mut self.current {
                    command.output_start_row = Some(row);
                }
            }
            CommandTrackingEvent::CommandFinished { exit_code } => {
                let Some(mut command) = self.current.take() else {
                    return;
                };
                command.exit_code = exit_code;
                command.status = match exit_code {
                    Some(0) => CommandStatus::Success,
                    Some(_) => CommandStatus::Error,
                    None => CommandStatus::Unknown,
                };
                command.output_end_row = Some(row);
                command.finished_at = Some(now);
                command.duration = now.duration_since(command.started_at).ok();
                self.commands.push(command);
                self.prompt_active = false;
                if self.commands.len() > 1000 {
                    self.commands.drain(..self.commands.len() - 1000);
                }
            }
        }
    }

    pub fn commands(&self) -> Vec<TrackedCommand> {
        let mut commands = self.commands.clone();
        if let Some(current) = &self.current {
            commands.push(current.clone());
        } else if self.prompt_active
            && let Some(prompt_row) = self.prompt_row
        {
            commands.push(TrackedCommand {
                id: 0,
                command: String::new(),
                status: CommandStatus::Unknown,
                prompt_row,
                command_row: prompt_row,
                output_start_row: None,
                output_end_row: None,
                cwd: self.cwd.clone(),
                exit_code: None,
                started_at: SystemTime::UNIX_EPOCH,
                finished_at: None,
                duration: None,
            });
        }
        commands
    }

    pub fn clear(&mut self) {
        self.current = None;
        self.commands.clear();
        self.prompt_row = None;
        self.prompt_active = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_handles_complete_sequences() {
        let mut parser = CommandSequenceParser::new();
        let events = parser.push(b"\x1b]777;vryn;B;cargo test\x07");
        assert_eq!(
            events,
            vec![CommandTrackingEvent::CommandStart {
                command: "cargo test".to_string()
            }]
        );
    }

    #[test]
    fn parser_handles_split_sequences() {
        let mut parser = CommandSequenceParser::new();
        assert!(parser.push(b"\x1b]777;vryn;D").is_empty());
        let events = parser.push(b";42\x1b\\");
        assert_eq!(
            events,
            vec![CommandTrackingEvent::CommandFinished {
                exit_code: Some(42)
            }]
        );
    }

    #[test]
    fn parser_ignores_malformed_sequences() {
        let mut parser = CommandSequenceParser::new();
        assert!(parser.push(b"\x1b]777;other;B;nope\x07").is_empty());
        assert!(parser.push(b"\x1b]777;vryn;X;nope\x07").is_empty());
    }

    #[test]
    fn lifecycle_tracks_success_and_duration() {
        let mut tracker = CommandTracker::new();
        let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
        let end = SystemTime::UNIX_EPOCH + Duration::from_secs(4);

        tracker.handle_event(
            CommandTrackingEvent::PromptStart {
                cwd: Some("/tmp".to_string()),
            },
            1,
            start,
        );
        tracker.handle_event(
            CommandTrackingEvent::CommandStart {
                command: "true".to_string(),
            },
            2,
            start,
        );
        tracker.handle_event(CommandTrackingEvent::CommandExecuted, 3, start);
        tracker.handle_event(
            CommandTrackingEvent::CommandFinished { exit_code: Some(0) },
            4,
            end,
        );

        let commands = tracker.commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].status, CommandStatus::Success);
        assert_eq!(commands[0].prompt_row, 1);
        assert_eq!(commands[0].command_row, 1);
        assert_eq!(commands[0].cwd.as_deref(), Some("/tmp"));
        assert_eq!(commands[0].duration, Some(Duration::from_secs(3)));
    }

    #[test]
    fn prompt_start_creates_placeholder_decoration() {
        let mut tracker = CommandTracker::new();
        tracker.handle_event(
            CommandTrackingEvent::PromptStart {
                cwd: Some("/tmp".to_string()),
            },
            7,
            SystemTime::UNIX_EPOCH,
        );

        let commands = tracker.commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].id, 0);
        assert_eq!(commands[0].status, CommandStatus::Unknown);
        assert_eq!(commands[0].command_row, 7);
        assert_eq!(commands[0].cwd.as_deref(), Some("/tmp"));
    }

    #[test]
    fn lifecycle_tracks_error() {
        let mut tracker = CommandTracker::new();
        tracker.handle_event(
            CommandTrackingEvent::CommandStart {
                command: "false".to_string(),
            },
            1,
            SystemTime::UNIX_EPOCH,
        );
        tracker.handle_event(
            CommandTrackingEvent::CommandFinished { exit_code: Some(2) },
            2,
            SystemTime::UNIX_EPOCH,
        );

        let commands = tracker.commands();
        assert_eq!(commands[0].status, CommandStatus::Error);
        assert_eq!(commands[0].exit_code, Some(2));
    }

    #[test]
    fn empty_command_is_ignored() {
        let mut tracker = CommandTracker::new();
        tracker.handle_event(
            CommandTrackingEvent::CommandStart {
                command: "   ".to_string(),
            },
            1,
            SystemTime::UNIX_EPOCH,
        );
        tracker.handle_event(
            CommandTrackingEvent::CommandFinished { exit_code: Some(0) },
            2,
            SystemTime::UNIX_EPOCH,
        );
        assert!(tracker.commands().is_empty());
    }
}
