# Terminal Command Tracking And Decorations Plan

## Scope

Implement command-aware terminal UI for:

1. Command tracking.
2. Command status decorations.
3. Hover metadata for tracked commands.

The implementation must be functional end to end. No placeholder UI, no inactive settings, no stub command records, and no heuristic-only command tracking as the primary mechanism.

## Goals

- Track command lifecycle in local terminals:
  - prompt start
  - command start
  - command text
  - output start
  - command finish
  - exit code
  - start time, end time, duration
- Render a compact left-gutter command decoration on the command line:
  - running / unknown
  - success
  - error
- Show hover metadata for command decorations:
  - command text
  - exit code
  - start time
  - duration

## Non-Goals

- Rerun/copy/context-menu actions.
- Overview ruler markers.
- Chat attachment integration.
- Persisting command history across app restarts.
- Remote terminal command tracking.
- Prompt detection via fragile parsing as the primary implementation.

## Architecture

### 1. Shell Integration Protocol

Add a small terminal shell integration protocol based on OSC escape sequences emitted by shell startup scripts.

Required events:

- `PromptStart`
- `CommandStart`
- `CommandExecuted`
- `CommandFinished { exit_code }`
- `CwdChanged { cwd }`

The exact sequence IDs should be app-owned constants in `vryn-terminal`; do not copy external naming into code.

Implementation notes:

- Extend `Terminal::process_output_inner` path to observe OSC sequences before/while feeding bytes into the emulator.
- Keep parsing isolated in a pure module such as `crates/vryn-terminal/src/command_tracking.rs`.
- Reject malformed or partial sequences safely.
- Support split chunks because PTY output can break escape sequences across reads.

### 2. Shell Startup Scripts

Provide real shell integration for supported shells:

- bash
- zsh
- fish

PowerShell can be a later follow-up unless the implementation can be completed and tested in this pass.

Implementation notes:

- Store scripts as Rust string assets or static files included by `vryn-terminal`.
- Inject through the existing shell-wrapper/on-create mechanism or by augmenting shell launch args in `ShellType`.
- Do not break existing user shell selection.
- Add a setting to disable command tracking if needed, but only if it is wired fully.

### 3. Command Tracking Model

Add a command tracking state owned by each `Terminal`.

Tracked command fields:

- unique id
- command text
- status: `Running`, `Success`, `Error`, `Unknown`
- prompt row / command row
- output start row
- output end row
- cwd at execution
- exit code
- started_at
- finished_at
- duration

Implementation notes:

- Rows must account for scrollback movement.
- Store rows in terminal-grid coordinates that remain usable for render/hit-testing.
- If the buffer is cleared, invalidate affected command decorations.
- Empty prompt submissions should not create completed commands unless there is meaningful command text.

### 4. Terminal Rendering

Add a left gutter lane for command decorations.

Implementation notes:

- Keep gutter width stable so text does not shift unpredictably.
- Render decorations only for visible rows.
- Use existing theme colors and simple vector/icon primitives already available in the app.
- Status mapping:
  - running/unknown: neutral hollow circle
  - success: success-colored filled circle
  - error: error-colored small error mark or filled error circle
- Decorations must not interfere with text selection, URL hover, or TUI mouse reporting.

### 5. Hover Metadata

Add hit-testing for the gutter decorations.

Hover should show:

- command text
- exit code or "running"
- duration
- timestamp
- cwd, if available

Implementation notes:

- Reuse existing overlay/popover primitives.
- Hover must be anchored to the decoration, not the terminal cell text.
- Hover should disappear when the command scrolls out or terminal content changes.

## Tests

### Unit Tests

Add tests for:

- OSC parser handles complete sequences.
- OSC parser handles split sequences across chunks.
- OSC parser ignores malformed sequences.
- command lifecycle creates `Running`, then `Success` for exit code `0`.
- command lifecycle creates `Error` for non-zero exit code.
- empty prompt submission does not create a command record.
- command duration is calculated from start/finish times.

### Integration-Level Tests

Add tests around `Terminal` processing:

- feeding shell integration sequences updates command tracking state.
- output between command executed and command finished is associated with the command.
- buffer clear invalidates decorations or makes them non-renderable.

### UI Logic Tests

Add pure tests where possible for:

- visible command decorations generated from command rows and viewport.
- hover hit-testing maps gutter coordinates to the correct command.
- no decoration hit-test occurs inside the terminal text grid.

## Rollout Steps

1. Add parser and command tracking model with unit tests.
2. Connect parser to terminal output processing.
3. Add shell integration injection for one shell and verify manually.
4. Extend shell integration to the remaining supported shells.
5. Render visible command decorations in the terminal gutter.
6. Add hover hit-testing and metadata popover.
7. Run targeted terminal tests.
8. Run full `cargo check`.

## Acceptance Criteria

- Running a command creates a tracked command record.
- Successful commands show a success decoration.
- Failed commands show an error decoration with the correct exit code.
- Hover over a decoration shows command metadata.
- The terminal remains usable with selection, paste, URL click, scrolling, and TUI mouse mode.
- All new tests pass.
- `cargo check` passes.
- No `cargo fmt`, `rustfmt`, `git reset`, or `git checkout` is run without explicit confirmation.
