# NotMux

Cross-platform terminal multiplexer built with Rust and GPUI (from Zed editor).

## Git Rules

- **Never revert or discard changes you didn't make.** If you see unexpected modifications in the working tree (e.g. from worktrees, other branches, or manual edits), leave them alone. Only stage and commit your own work.

## Build Commands

```bash
cargo build
cargo run
cargo test
```

On Windows, build from **x64 Native Tools Command Prompt for VS 2022** to avoid link.exe PATH conflicts with Git for Windows.

## Project Structure

```
src/                        # Desktop app — main binary, GPUI views, app coordinator
crates/                     # Library crates (20 crates, see below)
mobile/                     # Mobile app (Flutter + Rust FFI)
web/                        # Web client (React + TypeScript + xterm.js)
assets/                     # Fonts, icons (assets/icons/*.svg referenced as icons/*.svg)
scripts/                    # Build & utility scripts
```

### Crate layout

Most logic lives in `crates/`. The `src/` modules are thin re-exports (`pub use notmux_*::*`) so existing `use crate::` imports keep working.

| Crate | Purpose |
|-------|---------|
| `notmux-state` | Pure data types: `WorkspaceData`, `ProjectData`, `FolderData`, `HooksConfig`, `Toast`. No GPUI. |
| `notmux-layout` | `LayoutNode` recursive tree + algorithms (split/normalize/merge_visual_state) |
| `notmux-hooks` | Lifecycle hook execution (`HookRunner`, `HookMonitor`). Decoupled from `notmux-workspace`. |
| `notmux-workspace` | `Workspace` GPUI entity, persistence, settings, sessions, action methods |
| `notmux-terminal` | PTY management, shell config, session backends |
| `notmux-git` | Git status, diff parsing, worktree operations |
| `notagent-theme` | notagent theme engine: built-in JSON themes, custom themes dir, color resolution (verbatim port from notagent) |
| `notmux-theme` | GPUI theme adapter (`GpuiTheme`) + bridges mapping it to `ThemeColors` |
| `notmux-ui` | Design tokens, shared UI utilities |
| `notmux-files` | File search, file viewer, syntax highlighting |
| `notmux-markdown` | Markdown parsing and rendering |
| `notmux-views-terminal` | Terminal pane, layout container, split/tabs views |
| `notmux-views-sidebar` | Sidebar, project list, folder list, drag-and-drop |
| `notmux-views-git` | Diff viewer, worktree dialog, git status UI |
| `notmux-views-remote` | Remote connection dialogs |
| `notmux-views-services` | Service panel views |
| `notmux-remote-client` | Remote client connection manager |
| `notmux-services` | Docker Compose, port detection |
| `notmux-extensions` | Extension system |
| `notmux-ext-claude` | Claude AI extension |
| `notmux-ext-codex` | Codex extension |
| `notmux-ext-updater` | Self-update system |
| `notmux-core` | Shared types, API client, key handling |

## Module-Specific Context

Read these when working in the corresponding areas:

- `src/CLAUDE.md` — Desktop app architecture, event flow, GPUI entity model, testing rules
- `src/app/CLAUDE.md` — Main app entity, PTY event loop, remote bridge
- `src/remote/CLAUDE.md` — Remote control server (HTTP/WS API)
- `src/keybindings/CLAUDE.md` — Keyboard actions, bindings config
- `crates/notmux-workspace/CLAUDE.md` — State management, LayoutNode tree, persistence
- `crates/notmux-terminal/CLAUDE.md` — PTY threading model, shell detection
- `crates/notmux-git/CLAUDE.md` — Diff parsing, worktree operations
- `mobile/CLAUDE.md` — Flutter + Rust FFI mobile app
- `web/CLAUDE.md` — React web client
