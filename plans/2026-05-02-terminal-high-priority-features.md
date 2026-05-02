# Terminal High-Priority Feature Plan

## Scope

Implement the high-priority terminal features identified during the reference review:

1. Terminal environment variables.
2. Configurable working-directory strategy for new terminals.
3. macOS Option-as-Meta input handling.

The implementation must be functional end to end. No placeholder settings, no inactive UI, and no stubbed behavior.

## Non-Goals

- Python virtualenv auto-activation.
- Copy-on-select and keep-selection-after-copy.
- Terminal-as-editor-tab / center-pane integration.
- Task rerun integration.

These are useful, but they are separate work items with different blast radius.

## Implementation Plan

### 1. Settings Model

Add persistent settings to `AppSettings`:

- `terminal_env: HashMap<String, String>`
- `terminal_working_directory: TerminalWorkingDirectory`
- `option_as_meta: bool`

`TerminalWorkingDirectory` should support:

- `CurrentProjectDirectory`
- `FirstProjectDirectory`
- `AlwaysHome`
- `Always { directory: String }`

`CurrentFileDirectory` is intentionally deferred until Vryn has a reliable active-file concept across file viewer, diff viewer, and future editor panes.

### 2. Terminal Spawn Behavior

Thread the resolved settings into terminal creation:

- Resolve the working directory before creating or reconnecting a PTY.
- Expand `~` and environment variables only for the fixed `Always` directory.
- Fall back to the project directory, then home, if a configured directory is invalid.
- Merge `terminal_env` into the spawned command environment.
- Preserve existing required terminal variables (`TERM`, `COLORTERM`, `VRYN_TERMINAL_ID`, `PATH`, `LANG`).

### 3. Option-as-Meta

When enabled on macOS:

- Treat Option/Alt-modified printable input as Meta.
- Send ESC-prefixed bytes to the terminal, e.g. Option+x -> `\x1bx`.
- Keep existing function-key filtering and paste behavior.
- Leave non-macOS behavior unchanged.

### 4. Tests

Add tests for:

- `TerminalWorkingDirectory` deserialization defaults and explicit variants.
- Fixed-directory expansion and fallback behavior.
- Terminal env merging without losing required variables.
- Option-as-Meta byte conversion for printable text.

Run targeted `cargo test` commands for the affected crates. Do not run `cargo fmt` or `rustfmt` without explicit confirmation.

## Acceptance Criteria

- New terminals honor configured environment variables.
- New terminals start in the configured working directory.
- Invalid fixed working directories fall back predictably.
- Option-as-Meta sends ESC-prefixed input on macOS when enabled.
- All added tests pass.
