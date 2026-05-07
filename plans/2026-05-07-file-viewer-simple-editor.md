# File Viewer -> Simple Editor (Stages 1-3)

## Context

The current file viewer can load and render project files in the main content
area, including syntax highlighting, scrolling, text selection, copy, tabs, and
Markdown preview/source switching.

We want to turn this into a simple editor, not a full IDE. Scope is stages 1-3:

1. Minimal editing
2. Usable editor behavior
3. Good editor basics

Out of scope for this plan: LSP, completions, diagnostics, semantic tokens,
multi-cursor editing, collaborative editing, and advanced code intelligence.

---

## Product Shape

The embedded file view should become the normal central editor surface:

- Clicking a file in the right-side file explorer opens it in the middle.
- The file can be edited directly.
- Dirty state is visible in the header/tab.
- `Cmd/Ctrl+S` saves.
- Markdown files keep the existing Raw/Source mode controls.
- The old modal file browser can keep using the same editor core, but it does
  not need to become the primary workflow.

The first version should feel small and predictable rather than feature-rich.

---

## Architecture Direction

Do not mutate rendered `HighlightedLine` state directly. Add a small editable
text model underneath the viewer and let rendering derive from it.

Recommended split:

| Layer | File / Module | Responsibility |
|-------|---------------|----------------|
| Text model | `crates/vryn-files/src/file_viewer/buffer.rs` (new) | Own text, line indexing, cursor positions, edits, dirty flag, modified time. |
| Input/edit ops | `crates/vryn-files/src/file_viewer/editing.rs` (new) | Translate key/paste/delete/indent commands into buffer edits. |
| Existing viewer state | `crates/vryn-files/src/file_viewer/mod.rs` | Replace raw `content` mutation with `EditorBuffer` on each tab. |
| Rendering | `crates/vryn-files/src/file_viewer/render.rs` | Draw cursor, selection, dirty marker, editable lines. |
| Loading/saving | `crates/vryn-files/src/file_viewer/loading.rs` | Load into buffer; save buffer through `ProjectFs`. |
| FS abstraction | `crates/vryn-files/src/project_fs.rs` | Add write support for local first; remote can return unsupported initially if needed. |
| Root wiring | `src/views/root/*` | Keep current embedded FileViewer routing; no new top-level surface needed. |

### Text Model

Add an `EditorBuffer` with:

```rust
pub struct EditorBuffer {
    text: String,
    lines: Vec<LineRange>,
    dirty: bool,
    saved_text_hash: u64,
    modified_at: Option<SystemTime>,
}

pub struct Cursor {
    line: usize,
    column: usize,
}
```

Important: column handling should be char-index based initially, not byte-index
based at the UI boundary. Internally we can convert to byte offsets when editing.
This avoids panics on non-ASCII content. Grapheme-perfect movement can wait.

---

## Stage 1: Minimal Editing

Goal: make a loaded text file directly editable and saveable.

### Features

- Single cursor.
- Mouse click positions cursor.
- Printable key input inserts text.
- `Enter` inserts newline.
- `Backspace` deletes previous char or joins with previous line.
- `Delete` deletes next char or joins next line.
- Arrow keys move cursor left/right/up/down.
- `Home` / `End` move within current line.
- `Cmd/Ctrl+S` saves.
- Header shows dirty marker.
- Read-only fallback for files too large, binary files, or load errors.

### Implementation Tasks

1. Add `EditorBuffer`.
2. Load file content into `EditorBuffer`.
3. Derive syntax highlighting from buffer text after edits.
4. Add cursor state to `FileViewerTab`.
5. Handle key input in source/raw mode.
6. Render cursor in source/raw mode.
7. Add `ProjectFs::write_file(relative_path, content)`.
8. Implement local save.
9. On save success, clear dirty flag and update modification timestamp.

### Acceptance

- Open a `.rs`, `.txt`, or `.md` file from the side panel.
- Type, delete, add new lines, move cursor.
- Save with `Cmd/Ctrl+S`.
- Reopen file and see saved content.
- No editing allowed in Markdown preview mode.

---

## Stage 2: Usable Editor Behavior

Goal: editing should be comfortable for real small file changes.

### Features

- Selection replace: typing replaces selected text.
- `Cmd/Ctrl+A` selects all.
- Paste inserts clipboard text, preserving newlines.
- Copy/cut selection.
- Undo/redo.
- Dirty state in header and tab.
- Save error shown inline.
- External file change detection:
  - if clean, reload automatically;
  - if dirty, show warning state and do not overwrite silently.
- Tab key inserts indentation.
- Shift+Tab unindents selected/current line.
- Auto-indent on Enter based on previous line leading whitespace.

### Implementation Tasks

1. Replace current selection model usage with edit-aware selection.
2. Add edit transaction type:

```rust
pub struct Edit {
    range: TextRange,
    replacement: String,
}
```

3. Add undo/redo stacks of inverse edits.
4. Add paste/cut commands.
5. Add save error state to `FileViewerTab`.
6. Add external-change state:
   - `Clean`
   - `Dirty`
   - `ConflictOnDisk`
7. Update header rendering for dirty/conflict.
8. Add focused tests for buffer edit operations.

### Acceptance

- Replacing selected text works.
- Undo/redo works across typing, paste, delete, and newline edits.
- Dirty marker appears and disappears correctly.
- External disk changes do not overwrite dirty editor content silently.

---

## Stage 3: Good Editor Basics

Goal: enough polish that this can be used as the default file editing surface.

### Features

- Multi-line selection editing.
- Line operations:
  - indent/unindent range;
  - delete selected range;
  - select line on triple click if feasible.
- Find integration uses live buffer content.
- Markdown Raw/Source modes stay synchronized after edits.
- Large file behavior:
  - files over configured limit remain read-only;
  - warning explains why.
- Better cursor visibility:
  - keep cursor in viewport after keyboard movement;
  - keep selection endpoints visible.
- Basic bracket helpers:
  - insert matching `()`, `[]`, `{}`, quotes only when low-risk;
  - skip closing char when cursor is before it.
- Save all open dirty tabs in one viewer instance.

### Implementation Tasks

1. Generalize edit ranges across lines.
2. Make source rendering consume buffer snapshots consistently.
3. Re-run syntax highlighting incrementally enough for small files; full
   re-highlight is acceptable initially under `MAX_LINES`.
4. Ensure Markdown document parse refreshes after source edits.
5. Add viewport-follow helpers around cursor movement.
6. Add tests for multi-line edit ranges, indentation, and Markdown refresh.

### Acceptance

- Editing a Markdown file, switching Raw/Source, and saving keeps content
  consistent.
- Multi-line paste, indent, unindent, and undo behave predictably.
- Cursor does not disappear offscreen during normal keyboard navigation.
- Existing FileViewer tests still pass.

---

## Risks

| Risk | Mitigation |
|------|------------|
| Byte/char offset bugs on non-ASCII text | Keep UI positions as char indices and centralize conversion in `EditorBuffer`. |
| Syntax highlighting gets expensive | Keep current file size/line limits; full re-highlight is acceptable for stage 1-2. |
| Dirty content overwritten by external refresh | Add explicit dirty/conflict state before enabling external reload behavior. |
| Viewer becomes too complex | Move buffer/editing into separate modules and keep rendering mostly declarative. |
| Remote save support unclear | Add trait method now; implement local first; remote can return a clear unsupported error until remote API is widened. |

---

## Suggested Delivery Order

1. `EditorBuffer` with unit tests: insert/delete/newline/range conversion.
2. FileViewer loads from buffer and still renders read-only.
3. Cursor rendering + mouse click positioning.
4. Keyboard insert/delete/navigation.
5. Local save + dirty state.
6. Selection replace + paste/cut/copy.
7. Undo/redo.
8. External change conflict handling.
9. Multi-line edit polish and Markdown synchronization.

Each step should keep `cargo test -p vryn-files` green before moving on.

---

## Verification Checklist

- `cargo check`
- `cargo test -p vryn-files`
- Manual local-file smoke test:
  - open file from side panel;
  - type;
  - save;
  - reopen;
  - undo/redo;
  - Markdown Raw/Source switch;
  - external disk edit while clean and while dirty.

