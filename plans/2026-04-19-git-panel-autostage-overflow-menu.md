# Git Panel: Auto-Stage Commit + Overflow Menu (Stash / Discard)

## Context

Today the **Commit** button in the git panel is only enabled when files are explicitly
staged. That forces the user through a two-step "tick the checkbox → click Commit"
flow even for the trivial case of "commit everything I changed".

We want the following behaviour:

1. The commit button auto-stages tracked changes when nothing is selected, and the
   label switches between **"Commit Tracked"** / **"Commit"** / **"Amend"** based
   on state.
2. A **three-dots overflow menu** in the git-panel header that exposes batch
   operations: *Stage All*, *Unstage All*, *Stash All*, *Stash Pop*, *Show Stash*,
   *Discard All Tracked*.

After the change `cargo clippy --all-targets -- -D warnings` must be 0/0.

---

## Architecture Touch-Points

| Layer | File | Change |
|-------|------|--------|
| Pure git ops | `crates/vryn-git/src/repository.rs` | Add `stash_all_including_untracked`, `stash_list`, `stash_apply`, `stash_drop`, `stash_show_patch`, `discard_all_tracked`. Existing `stash_changes` / `stash_pop` stay untouched. |
| Public API | `crates/vryn-git/src/lib.rs` | Add `pub struct StashEntry { index, hash, subject, branch, timestamp_unix }` + re-exports. |
| Provider trait | `crates/vryn-views-git/src/diff_viewer/provider.rs` | Extend `GitProvider` trait with `stash_all`, `stash_pop`, `stash_list`, `stash_apply`, `stash_drop`, `stash_show_patch`, `discard_all_tracked`. Implement on `LocalGitProvider`; for `RemoteGitProvider` (line 283+) return `Err("not supported")` for the new ops (matches existing remote-stub pattern) so we don't widen the remote API surface in this PR. |
| UI request | `crates/vryn-workspace/src/requests.rs` | Add two new variants to `OverlayRequest`: `GitOverflowMenu { project_id, position, has_staged, has_unstaged, has_tracked, has_stash }` and `GitStashList { project_id, position }`. |
| Header view | `crates/vryn-views-git/src/git_header.rs` | (1) Add three-dots button to `render_panel_header`. (2) Rewrite `render_commit_footer` button label/enable logic. (3) Rewrite `handle_commit` to auto-stage tracked first when no files are staged. (4) Add handlers `handle_stash_all`, `handle_stash_pop`, `handle_discard_all_tracked` (called from root via the existing `gh.update(...)` lookup). |
| Overflow overlay | `src/views/overlays/git_overflow_menu.rs` (NEW) | Small popover at the three-dots position. Reuses `vryn_ui::menu::{context_menu_panel, menu_item, menu_item_conditional, menu_separator}`. Has internal `confirming_discard: bool` state for the inline two-step Discard All Tracked confirmation (no separate dialog overlay needed). Emits `GitOverflowMenuEvent`. |
| Stash list overlay | `src/views/overlays/git_stash_list.rs` (NEW) | Scrollable popover showing `Vec<StashEntry>`. Per-row buttons: Apply, Pop, Drop, Show Diff. "Show Diff" toggles an inline `<pre>`-style block fed by `stash_show_patch`. |
| Overlay wiring | `src/views/overlay_manager.rs` | New `OverlaySlot<GitOverflowMenu>` + `OverlaySlot<GitStashList>`, `show_*` / `hide_*` / `render_*` methods, and event re-emission as `OverlayManagerEvent::GitStageAll/UnstageAll/StashAll/StashPop/StashApply/StashDrop/DiscardAllTracked`. |
| Root handler | `src/views/root/handlers.rs` | Two new arms in `process_pending_requests` (mirroring lines 605-626) and new arms in the `OverlayManagerEvent` match (mirroring lines 319-356). All git-side execution goes through the existing pattern: look up the `ProjectColumn`, get its `git_header()`, and call a handler method on it. |

No changes needed to `crates/vryn-core/src/api.rs` or to the dispatcher/`ActionRequest`
machinery — these new ops only ever originate from local UI actions, so they follow
the same direct-provider-call pattern that `handle_stage_all` / `handle_unstage_all`
already use today (`git_header.rs:1579-1609`).

---

## Detailed Changes

### 1. `crates/vryn-git/src/repository.rs` — new git operations

All commands shell out via the existing `command("git").args([...])` helper used
throughout the file. Each function returns `Result<(), String>` with stderr on
failure (same pattern as `stash_changes` at line 471 and `discard_file` at 1252).

```rust
pub fn stash_all_including_untracked(repo_path: &Path) -> Result<(), String>
// git -C <path> stash push --include-untracked

pub fn discard_all_tracked(repo_path: &Path) -> Result<(), String>
// Two-step: `git -C <path> reset HEAD --` (unstage everything) then
// `git -C <path> checkout -- .` (revert tracked files in worktree).
// Untracked files are NOT touched. If the reset fails we still return Err
// without running checkout.

pub fn stash_apply(repo_path: &Path, index: usize) -> Result<(), String>
// git -C <path> stash apply --index stash@{N}

pub fn stash_drop(repo_path: &Path, index: usize) -> Result<(), String>
// git -C <path> stash drop stash@{N}

pub fn stash_show_patch(repo_path: &Path, index: usize) -> Result<String, String>
// git -C <path> stash show -p stash@{N}  → returns stdout

pub fn stash_list(repo_path: &Path) -> Result<Vec<StashEntry>, String>
// git -C <path> stash list --format=%gd%x09%H%x09%gs%x09%ct
// Parse tab-separated lines into StashEntry. Skip malformed lines.
// `%gd` is "stash@{N}" → extract N. `%gs` is the reflog subject which
// contains the branch like "WIP on main: <hash> <subject>" → split on
// ": " for branch, rest for subject.
```

Add unit tests after the existing `stash_*_returns_err_for_invalid_path` tests
(lines 1370-1378): one for each new function in the same `is_err()` style on a
non-existent path. Real success paths are not tested (no temp-repo harness in
this crate today and the existing stash tests don't have one either).

### 2. `crates/vryn-git/src/lib.rs`

```rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct StashEntry {
    pub index: usize,
    pub hash: String,
    pub subject: String,
    pub branch: Option<String>,
    pub timestamp_unix: i64,
}
```

Add re-exports next to existing `stash_changes, stash_pop` (line 20-21):
`stash_all_including_untracked, stash_list, stash_apply, stash_drop, stash_show_patch, discard_all_tracked, StashEntry`.

### 3. `crates/vryn-views-git/src/diff_viewer/provider.rs`

Add to the trait (after `discard_file` at line 42):

```rust
fn stash_all(&self) -> Result<(), String>;
fn stash_pop(&self) -> Result<(), String>;
fn stash_list(&self) -> Result<Vec<vryn_git::StashEntry>, String>;
fn stash_apply(&self, index: usize) -> Result<(), String>;
fn stash_drop(&self, index: usize) -> Result<(), String>;
fn stash_show_patch(&self, index: usize) -> Result<String, String>;
fn discard_all_tracked(&self) -> Result<(), String>;
```

`LocalGitProvider` impl: each method delegates to `vryn_git::*`, mirroring the
existing `discard_file` (line 119-121).

`RemoteGitProvider` impl (around line 283): each method returns
`Err("Stash operations not supported on remote repositories yet".into())` —
matching how the remote impl already stubs out unsupported ops.

### 4. `crates/vryn-views-git/src/git_header.rs`

#### 4a. Header — three-dots button (`render_panel_header`, line 740)

After the History button (line 765-772) add a spacer (`div().flex_1()`) and then
a new icon button using `icons/more-horizontal.svg` (verify it exists — if not
use `icons/more-vertical.svg`; both ship in `assets/icons/`). On click it
captures its bounds and pushes:

```rust
OverlayRequest::GitOverflowMenu {
    project_id: self.project_id.clone(),
    position: <bottom-left of the button>,
    has_staged: status.staged_count() > 0,
    has_unstaged: !status.tracked.is_empty() && !status.all_staged(),
    has_tracked: !status.tracked.is_empty(),
    has_stash: <see below>,
}
```

For `has_stash` we cache a `bool stash_present` on `GitHeader`, refreshed inside
`refresh_working_tree_status` by an extra `provider.stash_list()` call (cheap —
one git invocation alongside the existing status read). Initial value `false`.

#### 4b. Commit footer — label + enable + handler (lines 1420-1511, 1611-1642)

New label/enable logic in `render_commit_footer`:

```rust
let has_staged = status.staged_count() > 0;
let has_tracked = !status.tracked.is_empty();
let amend = self.commit_options_amend;
let message_empty = self.commit_message_input.read(cx).value().is_empty();

let can_commit =
    !message_empty
    && !self.committing
    && (has_staged || has_tracked || amend);

let button_label = if self.committing {
    "Committing..."
} else if amend && has_tracked && !has_staged {
    "Amend Tracked"
} else if amend {
    "Amend"
} else if has_staged {
    "Commit"
} else if has_tracked {
    "Commit Tracked"
} else {
    "Commit"
};
```

Rewrite `handle_commit` (line 1611) so the auto-stage step runs *inside* the
spawned task, before commit, when `!has_staged && has_tracked`:

```rust
fn handle_commit(&mut self, cx: &mut Context<Self>) {
    let message = self.commit_message_input.read(cx).value().to_string();
    if message.is_empty() { return; }
    let provider = self.git_provider.clone();
    let amend = self.commit_options_amend;
    let signoff = self.commit_options_signoff;

    let needs_auto_stage = self
        .working_tree_status
        .as_ref()
        .map(|s| s.staged_count() == 0 && !s.tracked.is_empty())
        .unwrap_or(false);
    let tracked_paths: Vec<String> = if needs_auto_stage {
        self.working_tree_status
            .as_ref()
            .map(|s| s.tracked.iter().map(|f| f.path.clone()).collect())
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    self.committing = true;
    cx.notify();

    cx.spawn(async move |this, cx| {
        let result = smol::unblock(move || {
            if needs_auto_stage {
                for p in &tracked_paths {
                    provider.stage_file(p)?;
                }
            }
            provider.commit(&message, amend, signoff)
        }).await;
        // ... existing post-commit refresh logic unchanged
    }).detach();
}
```

Per-file `stage_file` (vs. `stage_all`) is intentional — it stages exactly the
non-untracked tracked set we observed at click time,
without adding an `vryn_git::stage_paths(&[...])` helper. Untracked files are
left alone.

#### 4c. New handlers

```rust
fn handle_stash_all(&mut self, cx: &mut Context<Self>)
fn handle_stash_pop(&mut self, cx: &mut Context<Self>)
fn handle_stash_apply(&mut self, index: usize, cx: &mut Context<Self>)
fn handle_stash_drop(&mut self, index: usize, cx: &mut Context<Self>)
fn handle_discard_all_tracked(&mut self, cx: &mut Context<Self>)
fn fetch_stash_list(&self, cx: &mut Context<Self>) -> Task<Result<Vec<StashEntry>, String>>
fn fetch_stash_patch(&self, index: usize, cx: &mut Context<Self>) -> Task<Result<String, String>>
```

All follow the existing `handle_stage_all` template (line 1579): clone provider,
spawn, surface error to `last_error`, call `refresh_working_tree_status`.

### 5. `src/views/overlays/git_overflow_menu.rs` (NEW)

Modelled on `git_file_context_menu.rs` (the file we read in full above).

```rust
pub enum GitOverflowMenuEvent {
    Close,
    StageAll      { project_id: String },
    UnstageAll    { project_id: String },
    StashAll      { project_id: String },
    StashPop      { project_id: String },
    ShowStash     { project_id: String, position: Point<Pixels> },
    DiscardAllTracked { project_id: String },
}

pub struct GitOverflowMenu {
    project_id: String,
    position: Point<Pixels>,
    has_staged: bool,
    has_unstaged: bool,
    has_tracked: bool,
    has_stash: bool,
    confirming_discard: bool,   // inline 2-step confirmation
    focus_handle: FocusHandle,
}
```

Render (matching the structure of `git_file_context_menu.rs:68-216`):
- Backdrop closes on left/right click.
- `deferred(anchored().position(self.position).snap_to_window().child(panel))`.
- Panel uses `context_menu_panel("git-overflow-menu", &t)`.
- Items use `menu_item_conditional(...)` with `enabled = ...` so disabled
  greying matches the existing helper at `crates/vryn-ui/src/menu.rs:91`.
  - "Stage All"   — enabled when `has_unstaged`
  - "Unstage All" — enabled when `has_staged`
  - separator
  - "Stash All"   — enabled when `has_tracked || /* untracked > 0 — pass via flag if needed */`
  - "Stash Pop"   — enabled when `has_stash`
  - "Show Stash"  — enabled when `has_stash`
  - separator
  - When `!confirming_discard`: red "Discard All Tracked" (`menu_item_with_color`,
    color `t.error`), enabled when `has_tracked`. Click sets
    `self.confirming_discard = true; cx.notify()`.
  - When `confirming_discard`: shows two stacked items:
    "Confirm Discard All Tracked" (red) → emits `DiscardAllTracked` then closes,
    and "Cancel" → emits `Close`.

`Cancel` action (`crate::keybindings::Cancel`) closes the menu, same as the
file context menu.

### 6. `src/views/overlays/git_stash_list.rs` (NEW)

Standalone overlay (focusable, has its own `OverlaySlot`). Receives
`Vec<StashEntry>` lazily — the overlay manager loads it via the project's
`GitProvider` when constructing the entity, so `GitStashList::new` is async-fed
through `cx.spawn`.

Layout: a `v_flex` panel anchored top-center (or at the position pushed by the
menu — we already have it). For each `StashEntry`:

```
┌─────────────────────────────────────────────────────────┐
│ stash@{0}  WIP on main: <subject>            5h ago    │
│   [Apply] [Pop] [Drop] [Show Diff]                     │
│   ┌─────────────────────── (collapsed by default) ──┐ │
│   │ <patch text from stash_show_patch>              │ │
│   └────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────┘
```

State: `expanded: HashSet<usize>` of stash indices whose patch is shown.
Patches are loaded on-demand and cached in a `HashMap<usize, String>`.

Events:
```rust
pub enum GitStashListEvent {
    Close,
    Apply { project_id: String, index: usize },
    Pop   { project_id: String, index: usize },
    Drop  { project_id: String, index: usize },
}
```

After Pop/Drop the list is reloaded in-place (the overlay reads the provider
again). After Apply, the overlay closes (applying restores changes
into the working tree, the panel content matters more than the stash list).

### 7. `src/views/overlay_manager.rs`

Add fields next to `git_file_context_menu` (line 196):

```rust
git_overflow_menu: OverlaySlot<GitOverflowMenu>,
git_stash_list:   OverlaySlot<GitStashList>,
```

Add `show_git_overflow_menu(...)`, `hide_git_overflow_menu`, `render_git_overflow_menu`,
plus the same trio for the stash list, mirroring lines 957-1059.

Subscriptions translate menu events into new `OverlayManagerEvent` variants:
`GitStageAll { project_id }`, `GitUnstageAll`, `GitStashAll`, `GitStashPop`,
`GitDiscardAllTracked`, `GitShowStash { project_id, position }`,
`GitStashApply { project_id, index }`, `GitStashDrop { project_id, index }`.

`GitShowStash` from the overflow menu is handled inside the overlay manager
directly: close the overflow menu, then `show_git_stash_list(...)`.

### 8. `src/views/root/handlers.rs`

Add to the `process_pending_requests` match (after line 626):

```rust
OverlayRequest::GitOverflowMenu { project_id, position, has_staged, has_unstaged, has_tracked, has_stash } => {
    self.overlay_manager.update(cx, |om, cx| {
        om.show_git_overflow_menu(project_id, position, has_staged, has_unstaged, has_tracked, has_stash, cx);
    });
}
OverlayRequest::GitStashList { project_id, position } => {
    self.overlay_manager.update(cx, |om, cx| {
        om.show_git_stash_list(project_id, position, cx);
    });
}
```

Add to the `OverlayManagerEvent` match (after line 356):

```rust
OverlayManagerEvent::GitStageAll  { project_id }     => self.with_git_header(project_id, cx, |gh, cx| gh.handle_stage_all(cx)),
OverlayManagerEvent::GitUnstageAll{ project_id }     => self.with_git_header(project_id, cx, |gh, cx| gh.handle_unstage_all(cx)),
OverlayManagerEvent::GitStashAll  { project_id }     => self.with_git_header(project_id, cx, |gh, cx| gh.handle_stash_all(cx)),
OverlayManagerEvent::GitStashPop  { project_id }     => self.with_git_header(project_id, cx, |gh, cx| gh.handle_stash_pop(cx)),
OverlayManagerEvent::GitStashApply{ project_id, index } => self.with_git_header(project_id, cx, |gh, cx| gh.handle_stash_apply(*index, cx)),
OverlayManagerEvent::GitStashDrop { project_id, index } => self.with_git_header(project_id, cx, |gh, cx| gh.handle_stash_drop(*index, cx)),
OverlayManagerEvent::GitDiscardAllTracked { project_id } => self.with_git_header(project_id, cx, |gh, cx| gh.handle_discard_all_tracked(cx)),
```

Where `with_git_header` is a tiny private helper that mirrors `refresh_git_panel`
(line 361-366) but takes a closure. Could also be inlined four times — fine
either way.

---

## Tests

Per `src/CLAUDE.md`'s "what to test" rules, the new logic that warrants tests:

- **`stash_list` parser** in `repository.rs` — non-trivial: tab-separated input,
  branch extraction from reflog subject, malformed-line skip. Add a small
  pure-function helper (`fn parse_stash_list_lines(out: &str) -> Vec<StashEntry>`)
  and unit-test it directly with three fixture strings (normal entry, missing
  branch like "On main: …", garbage line).
- **Commit-button label logic** in `git_header.rs` — extract into a free function
  `fn commit_button_label(committing: bool, amend: bool, has_staged: bool, has_tracked: bool) -> &'static str`
  and table-test all 8 boolean combos × `committing`. Cheap and locks the spec.
- **Auto-stage decision** in `handle_commit` — the predicate
  `staged_count() == 0 && !tracked.is_empty()` is two field reads, not worth
  isolating.
- **New per-op handlers, overlay wiring, menu rendering** — UI/wiring code, no
  branching logic; covered by the existing "no test" exception.
- **`*_returns_err_for_invalid_path`** smoke tests for each new git function
  (matching the existing pattern at lines 1370-1378).

---

## Verification

```bash
cargo build
cargo test -p vryn-git -p vryn-views-git
cargo clippy --all-targets -- -D warnings    # MUST be 0 warnings
cargo run
```

Manual end-to-end (in a scratch git repo opened as an vryn project):

1. **Auto-stage commit**
   - Modify two tracked files, leave checkboxes unticked.
   - Open commit panel → button reads **"Commit Tracked"**.
   - Type a message, click. Both files commit, untracked file is untouched.
   - Tick one checkbox → button label flips to **"Commit"**, only that file commits.
2. **Amend** — toggle Amend with no message, button reads **"Amend"**; with
   tracked changes present and nothing staged → **"Amend Tracked"**.
3. **Three-dots → Stage All** then **Unstage All** — checkbox column reflects
   each transition, disabled states gate correctly when nothing applies.
4. **Stash All** — working tree clears (tracked + untracked gone). **Stash Pop**
   restores. **Show Stash** lists entries; Apply/Drop work; "Show Diff"
   expands the patch text inline.
5. **Discard All Tracked** — first click swaps the menu item to red
   "Confirm Discard All Tracked" + "Cancel"; confirm reverts only tracked
   changes, untracked files survive.
6. Re-open the panel and confirm the stash badge / `has_stash` state is
   refreshed after each stash op.
