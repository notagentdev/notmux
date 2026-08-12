# Changelog

All notable changes to NotMux are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-08-12

First public release.

NotMux is a native terminal multiplexer written in Rust on top of GPUI. It
keeps the things a multiplexer is good at — splits, tabs, persistent sessions —
and adds the parts that a terminal alone leaves to you: multiple projects side
by side, git and worktree state where you are already looking, and a CLI that
drives the running app so everything here is scriptable.

### Layout & windows

- Horizontal and vertical splits with drag-to-resize dividers, tabbed
  containers with reordering, and directional focus navigation.
- Detach any terminal into a floating window and reattach it later.
- Fullscreen focus mode, minimize-to-header, and per-terminal zoom (0.5x–3.0x).
- Multiple projects as resizable columns, with a collapsible sidebar that shows
  each project's terminals, current git branch, and listening ports.
- Full layout, terminal state, and settings are saved to disk and restored on
  the next start.

### Terminal

- Complete ANSI emulation via `alacritty_terminal`.
- Inline search with regex, case sensitivity, and a match count.
- Clickable URLs and file paths, including `file:line:col`, opening in VS Code,
  Cursor, Zed, Sublime, vim, and others.
- Bracketed paste with escape-sequence injection protection, Shift+Enter for
  literal newlines, configurable scrollback from 100 to 100,000 lines.
- Per-terminal shell selection across bash, zsh, fish, cmd, PowerShell, and WSL.

### Session persistence

- Terminals survive an app restart through dtach, tmux, or screen on Unix, with
  automatic backend detection (dtach preferred) and WSL support on Windows.
- Named workspace sessions can be saved, loaded, renamed, and deleted, and
  workspaces export to and import from JSON.

### Git & worktrees

- Create and manage git worktrees as first-class projects, with path templates
  using `{repo}` and `{branch}`.
- New worktrees are discovered automatically every 30 seconds; stale ones are
  cleaned up when their paths disappear.
- Merge, stash, fetch, push, or delete the branch when closing a worktree.
- Branch display including detached HEAD, plus cached diff stats.
- Unified and side-by-side diff views with syntax highlighting.

### Automation & extensibility

- Lifecycle hooks on project open/close and worktree create/close, globally or
  per project, with `pre_merge`, `post_merge`, `before_worktree_remove`,
  `worktree_removed`, `on_rebase_conflict`, and `on_dirty_worktree_close`.
- Hooks prefixed with `terminal:` spawn a visible PTY pane instead of running
  blind, and the hook monitor records status and duration for every run.
- Project services and Docker Compose integration defined in `notmux.yaml`,
  with auto-start, auto-restart, and a status panel.
- Custom per-project commands in `notmux.yaml` surface at the top of the
  command palette.

### Notifications

- Panes get an attention ring and the sidebar shows the latest message when a
  long-running program or agent needs you.
- OSC 9, OSC 99, and OSC 777 escape sequences are picked up from any program.
- Native OS notifications on macOS, Linux (libnotify), and Windows while NotMux
  is in the background.
- `notmux hooks setup` wires Claude Code and Codex to notify on turn
  completion; `notmux notify` does the same from any script.

### Command-line interface

The `notmux` binary doubles as a client for the running app, authenticating
automatically on first use:

```bash
notmux projects                      # list projects
notmux run "cargo test" -t <id>      # run a command in a terminal
notmux send "hello" --enter          # type text
notmux split down                    # split the current pane
notmux read -t <id>                  # print a terminal's visible content
notmux state                         # full workspace state as JSON
notmux events --follow               # tail the event log
```

Inside a NotMux terminal, commands target that terminal automatically. Output
is tab-separated for grep and awk, or JSON with `--json`. Every mutating
action, notification, and terminal exit is appended to
`~/.config/notmux/events.jsonl` so external tools can follow along.

### Remote control & companion apps

- Local HTTP/WebSocket server for controlling terminals remotely, with
  HMAC-SHA256 token auth and rate-limited pairing codes.
- Browser-based web client served by the app itself.
- Flutter + Rust FFI companion app for Android and iOS.

### Appearance

- 11 built-in themes from the notagent theme engine: dark, light, one-dark,
  one-light, tokyo-night, dracula, alucard, anysphere, nord-midnight,
  poimandres-dark, and poimandres-light.
- Custom themes in notagent JSON format from `~/.notagent/agent/themes/`.
- Live theme switching without a restart, plus configurable font family, size,
  line height, and a separate UI font size.
- Every keybinding is overridable through `~/.config/notmux/keybindings.json`.

### Also included

- Command palette, file search, syntax-highlighted file viewer, settings panel,
  and a searchable keybinding reference.
- Background update checks against GitHub Releases with SHA256 verification;
  self-update is skipped for Homebrew installs.
- Opt-in Claude Code and Codex status and usage tracking.
- Status bar with CPU, memory, and clock.

### Platform support

- **macOS** 11 or newer, Apple Silicon and Intel — native traffic lights and an
  extended PATH for Homebrew shells.
- **Linux** x86_64 — Wayland maximize workaround, auto-detected shells.
- **Windows** x86_64 — custom titlebar, cmd/PowerShell/WSL with distro
  detection.

### Known limitations

- The macOS build is ad-hoc signed rather than notarized. Gatekeeper will
  refuse the first launch; clear the quarantine flag with
  `xattr -dr com.apple.quarantine /Applications/NotMux.app` or open it once via
  right-click → Open.
- The mobile app has no text selection, pinch-to-zoom, or two-finger scrollback
  yet, and renders with the dark palette regardless of the desktop theme.
- Git status for remote (SSH) projects is not read yet, so status coloring in
  the sidebar and file explorer stays empty for them.
