<p align="center">
  <img src=".github/assets/notmux-icon.png" width="256" alt="NotMux">
</p>

# NotMux

A fast, native terminal multiplexer built in Rust with GPUI.
Tabs, splits, detachable windows, command palette, and automatic workspace restore.

## Installation

### macOS

```bash
curl -fsSL https://raw.githubusercontent.com/contember/vryn-ws/main/install.sh | bash
```

### Linux

```bash
curl -fsSL https://raw.githubusercontent.com/contember/vryn-ws/main/install.sh | bash
```

### Windows

```powershell
irm https://raw.githubusercontent.com/contember/vryn-ws/main/install.ps1 | iex
```

The install script includes built-in auto-update support. On macOS and Linux, NotMux is installed to `~/.local/bin/notmux`. On Windows, it installs to `%LOCALAPPDATA%\Programs\NotMux` with a Start Menu shortcut.

## Features

### Layout & Window Management
- **Split panes** - Horizontal and vertical splits with drag-to-resize dividers
- **Tabs** - Organize terminals in tabbed containers with reordering support
- **Detachable windows** - Pop out any terminal into a separate floating window and reattach later
- **Fullscreen mode** - Focus on a single terminal with next/previous cycling
- **Minimize/restore** - Collapse terminals to their header to save space
- **Per-terminal zoom** - Adjust zoom level (0.5x to 3.0x) independently per terminal
- **Directional focus navigation** - Move focus between panes using arrow-key shortcuts

### Multi-Project Workspace
- **Project columns** - Manage multiple projects side-by-side with resizable columns
- **Sidebar** - Collapsible project list with tree view of terminals, drag-and-drop reordering, and auto-hide mode
- **Project metadata** - Rows show the current git branch and the listening ports of running services
- **Folder colors** - Color-code projects (red, orange, yellow, green, blue, purple, pink)
- **Project switcher** - Quick searchable project navigation overlay
- **Workspace persistence** - Auto-saves full layout, terminal state, and settings to disk

### Terminal
- **Full terminal emulation** - Powered by alacritty_terminal with complete ANSI support
- **Search** - Inline text search with regex support, case sensitivity toggle, and match count
- **Link detection** - Clickable URLs and file paths (supports `file:line:col` syntax)
- **File opener integration** - Open detected files in your editor (VS Code, Cursor, Zed, Sublime, vim, etc.)
- **Bracketed paste mode** - Proper multi-line paste handling with escape sequence injection protection
- **Shift+Enter** - Send literal newline for multi-line input (useful for Claude Code, Python, etc.)
- **Configurable scrollback** - 100 to 100,000 lines
- **Cursor blink** - Toggleable cursor blinking
- **Bell notification** - Visual indicator when a terminal rings the bell
- **Per-terminal shell selection** - Choose a different shell for each terminal
- **Context menu** - Right-click for copy, paste, select all, and link actions

### Session Persistence
- **Session backends** - Keep terminals alive across app restarts using dtach, tmux, or screen (Unix)
- **dtach support** - Lightweight session persistence (preferred backend)
- **Auto-detection** - Automatically selects the best available backend (dtach > tmux > screen)
- **WSL session support** - Session backends work with WSL terminals on Windows
- **Session manager** - Save, load, rename, and delete named workspace sessions
- **Export/import** - Export workspaces to JSON and import them back

### Git Integration
- **Git worktree support** - Create and manage git worktrees as projects directly from the UI
- **Worktree sync watcher** - Auto-discovers new git worktrees every 30 seconds
- **Worktree auto-cleanup** - Removes stale worktree projects when paths no longer exist
- **Worktree path templates** - Configure worktree paths with `{repo}` and `{branch}` variables
- **Merge/stash on close** - Options to merge, stash, fetch, push, or delete branch when closing a worktree
- **Branch detection** - Displays current branch, handles detached HEAD
- **Diff stats** - Tracks lines added/removed with cached git status

### Themes & Appearance
- **Built-in themes** - 11 themes from the notagent theme engine: dark, light, one-dark, one-light, tokyo-night, dracula, alucard, anysphere, nord-midnight, poimandres-dark, poimandres-light
- **Custom themes** - Drop notagent-format JSON themes into the custom themes directory (shared with notagent)
- **Live switching** - Theme changes apply instantly across all panes, no restart
- **Configurable fonts** - Font family, size (8-48pt), line height (1.0-3.0), and separate UI font size

### Command Palette & Overlays
- **Command palette** - Searchable list of all actions with keybinding hints
- **Custom commands** - Project-specific actions from `notmux.yaml` (`commands:` with name, command, optional cwd) appear at the top of the palette and run in a new terminal in the project
- **File search** - Fast file lookup within a project (respects .gitignore-style filtering)
- **Settings panel** - GUI for all preferences (theme, font, terminal, hooks, per-project settings)
- **Theme selector** - Live-preview theme picker
- **Keybindings help** - Categorized shortcut reference with search
- **File viewer** - Syntax-highlighted file preview with line numbers and search
- **Diff viewer** - Unified and side-by-side diff views with syntax highlighting

### Customization
- **Custom keybindings** - Override any shortcut via `keybindings.json`
- **Lifecycle hooks** - Run commands on project open/close and worktree create/close (global or per-project)
- **Per-project settings** - Override global settings per project
- **Shell configuration** - Set default shell or pick per terminal (bash, zsh, fish, cmd, PowerShell, WSL)

### Hook Terminals
- **Hook terminals** - Commands prefixed with `terminal:` in hooks spawn visible PTY terminals (e.g., `terminal: claude -p "fix rebase conflict"`)
- **Hook monitor** - Tracks execution history, status (Running/Succeeded/Failed), and duration for all hooks
- **Git hooks** - `pre_merge`, `post_merge`, `before_worktree_remove`, `worktree_removed`, `on_rebase_conflict`, `on_dirty_worktree_close`
- **Environment variables** - Hooks receive `NOTMUX_PROJECT_ID`, `NOTMUX_PROJECT_NAME`, `NOTMUX_PROJECT_PATH`, `NOTMUX_BRANCH`, `NOTMUX_TARGET_BRANCH`, etc.

### Services
- **Project services** - Define services in `notmux.yaml` with name, command, cwd, env vars
- **Docker Compose integration** - Auto-detects and manages Docker Compose services
- **Auto-start & restart** - Services can auto-start on project open and auto-restart on crash
- **Service panel** - Monitor service status (Stopped, Starting, Running, Crashed) and ports

### Notifications
- **Notification rings & labels** - Panes get a ring and the sidebar shows the latest message when an agent needs attention
- **Terminal escape sequences** - Picks up OSC 9, OSC 99, and OSC 777 notifications from any program
- **Native OS notifications** - Forwards agent/OSC notifications to macOS Notification Center, libnotify (Linux), or Windows toasts while NotMux is in the background; toggle under Settings → General
- **Agent hooks** - `notmux hooks setup` wires Claude Code and Codex to notify on turn completion (`notmux notify` from any script works too)

### AI Tool Integration
- **Claude Code status** - Real-time service status from status.claude.com
- **Claude Code usage** - OAuth-based usage tracking (5-hour, 7-day rate limits, credits)
- **Codex status & usage** - OpenAI Codex status monitoring with OAuth token refresh
- Both integrations are opt-in via settings toggles

### Command-Line Interface
The `notmux` binary doubles as a CLI for scripting the running app (authentication is automatic on first use):

```bash
notmux projects                      # list projects (* = focused)
notmux terminals                     # list terminals with names and projects
notmux run "cargo test" -t <id>      # run a command in a terminal
notmux send "hello" --enter          # type text (defaults to the current terminal)
notmux key ctrl-c                    # send a special key
notmux split down                    # split the current pane
notmux focus <terminal-id>           # focus a terminal
notmux new-terminal [project]        # create a terminal
notmux read -t <id>                  # print a terminal's visible content
notmux add-project <path>            # add a project to the workspace
notmux notify --title "Done"         # send a notification (ring + badge + OS toast)
notmux state                         # full workspace state as JSON
notmux action '<json>'               # any raw ActionRequest
notmux events --follow               # tail the event log
```

Every mutating action, notification, and terminal exit is appended to
`~/.config/notmux/events.jsonl` (NDJSON, rotates at 10 MB) so external tools
can observe app activity.

Inside a NotMux terminal, commands target that terminal automatically (`NOTMUX_TERMINAL_ID`). Output is tab-separated for grep/awk; pass `--json` for structured output. The same API is reachable over HTTP/WebSocket for remote instances.

### Remote Control & Companion Apps
- **Remote API** - Local HTTP/WebSocket server for remote terminal control (see `docs/remote.md`)
- **Mobile app** - Flutter + Rust FFI companion app for Android/iOS (see `docs/mobile-status.md`)
- **Web client** - Browser-based terminal access via built-in web UI
- **Secure pairing** - HMAC-SHA256 token auth with rate-limited pairing codes

### Auto-Update
- **Built-in updater** - Background update checks via GitHub Releases
- **SHA256 verification** - Downloaded updates are cryptographically verified
- **Homebrew-aware** - Skips self-update when installed via Homebrew

### Platform Support
- **macOS** - Native traffic light buttons, extended PATH for homebrew shells
- **Linux** - Wayland maximize workaround, auto-detected shells
- **Windows** - Custom titlebar, cmd/PowerShell/WSL support with distro detection

### Status Bar
- CPU usage, memory usage, and current time displayed at the bottom

## Building

Requires Rust toolchain (edition 2021).

```bash
cargo build --release
```

## Running

```bash
cargo run
```

## Keyboard Shortcuts

| Action | macOS | Linux/Windows |
|--------|-------|---------------|
| New terminal | Cmd+T | Ctrl+T |
| Close terminal | Cmd+W | Ctrl+W |
| Split horizontal | Cmd+D | Ctrl+D |
| Split vertical | Cmd+Shift+D | Ctrl+Shift+D |
| Navigate panes | Cmd+Alt+Arrow | Ctrl+Alt+Arrow |
| Next/prev terminal | Cmd+Shift+]/[ | Ctrl+Tab / Ctrl+Shift+Tab |
| Fullscreen terminal | Shift+Escape | Shift+Escape |
| Command palette | Cmd+Shift+P | Ctrl+Shift+P |
| File search | Cmd+P | Ctrl+P |
| Find | Cmd+F | Ctrl+F |
| Copy | Cmd+C | Ctrl+C |
| Paste | Cmd+V | Ctrl+V |
| Zoom in/out | Cmd++/- | Ctrl++/- |
| Reset zoom | Cmd+0 | Ctrl+0 |
| Toggle sidebar | Cmd+B | Ctrl+B |
| Settings | Cmd+, | Ctrl+, |

All shortcuts are customizable via `~/.config/notmux/keybindings.json`.

## Configuration

Settings are stored in `~/.config/notmux/`:

| File | Purpose |
|------|---------|
| `settings.json` | Theme, font, shell, scrollback, hooks, and other preferences |
| `workspace.json` | Projects, layouts, and terminal state |
| `keybindings.json` | Custom keyboard shortcuts |
| `notmux.yaml` (project root) | Project services and Docker Compose configuration |

Custom themes (notagent JSON format) live in `~/.notagent/agent/themes/*.json` and appear in the theme picker automatically.

## Documentation

| Guide | Description |
|-------|-------------|
| [Configuration](docs/configuration.md) | Settings, keybindings, custom themes, per-project overrides |
| [Lifecycle Hooks](docs/hooks.md) | Hook terminals, git hooks, environment variables |
| [Project Services](docs/services.md) | notmux.yaml, Docker Compose integration, auto-restart |
| [Git Worktrees](docs/worktrees.md) | Worktree management, sync watcher, path templates |
| [Remote Control API](docs/remote.md) | HTTP/WebSocket API, pairing, authentication |
| [Mobile Client](docs/mobile-status.md) | Flutter + Rust FFI mobile companion app |

## Dependencies

- **GPUI** + **gpui-component** - UI framework
- **alacritty_terminal** - Terminal emulation
- **portable-pty** - PTY management
- **smol** - Async runtime
- **tokio** + **axum** - Remote control server
- **syntect** - Syntax highlighting
- **serde_yaml** - Service config parsing

## License

MIT
