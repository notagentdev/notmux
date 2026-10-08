# Agent orchestration

NotMux can run one coding agent as an **orchestrator** (root) that directs
up to four **worker** agents, each in its own pane of the same project.
Any supported CLI can be root or worker; workers never spawn workers. The
protocol is one shell command, `notmux agent`, talking to the local NotMux
API; nothing is ever typed into another agent's terminal.

NotMux does not modify any agent's source or ship agent-side modes or
plugins. It starts the vendor CLIs with their own unattended flags, gives
them the orchestration skill as a system prompt (or as the first message
where no such flag exists), and talks to them through its API.

## Roles and limits

| Rule | Value | Enforced where |
|---|---|---|
| Live workers per root | 4 | store (`capacity_exceeded`) |
| Live workers per NotMux instance | 8 | store |
| Worker starts per root | `--budget` (default 8), consumed per attempt | store (`budget_exhausted`) |
| Assignments per worker | 1 running, up to 16 queued | store (`queue_full`) |
| Task text | 64 KiB | refused, never truncated |
| Result text | 256 KiB | refused, never truncated |
| Messages | 64 KiB each, 1024 unacknowledged per recipient | `mailbox_full` |
| Workers spawning, assigning, registering | never | `root_only` before any side effect |

A worker is a leaf: it reports with `finish`, asks the root by `message`,
and waits with `next`. It cannot `spawn`, `assign`, `register`, or `lead`
(all `root_only`). Results are immutable. Messages stay inside one root's
group. Every mutating request carries a request ID; the mutation and the
record that answers a retry are one store write, so a retry after a lost
answer returns the stored answer instead of starting a second worker or
sending a second message.

`next` without `--after` delivers the assignment the worker still holds
again, so a lost answer cannot leave a task stranded as `running`; with
`--after <task-id>` (the task the worker holds) it waits for messages
only. `stop`/`cancel` take the target as `<run-id>` or `--target <run-id>`;
an option a verb does not know is an error, never silently a group stop.
IDs are validated before anything is sent.

A run whose process is gone (exit, restart) may still read its group's
history (`list`, `get`, `task`, `tasks`, `inbox`, `wait`) by its run ID;
mutations need a live identity.

## Harness matrix

| Harness | Start | Trust written before launch | Delivery of follow-up work |
|---|---|---|---|
| notagent | `--yolo --approve --session-id <run> --append-system-prompt <file> <pointer prompt>` | none (`--approve` trusts project files) | cooperative `next` |
| Claude Code | `--session-id <run> --dangerously-skip-permissions --append-system-prompt-file <file> --settings <per-run json> <pointer prompt>` | `.claude.json`: `projects.<cwd>.hasTrustDialogAccepted`, `hasCompletedOnboarding`; `settings.json`: `skipDangerousModePermissionPrompt` | Stop hook + UserPromptSubmit hook from the per-run settings |
| Codex | `-C <cwd> --dangerously-bypass-approvals-and-sandbox <pointer prompt>` (codex 0.155 refuses `-a` beside the bypass flag) | `config.toml`: `[projects."<cwd>"] trust_level = "trusted"` (also immediate child git repos), `check_for_update_on_startup = false`, `[notice.model_migrations]` for a pinned `model` | cooperative `next` |
| generic | the given argv | none | none; the exit code is the result |

The bootstrap every agent receives is `resources/skills/notmux-orchestration/SKILL.md`
plus a role banner with the run facts. notagent and Claude get it as the
appended system prompt; Codex has no system-prompt flag, so it is prepended
to the task document. The pointer prompt names the task file; the agent
reads it.

Vendor files NotMux modifies, and how to undo them:

- `~/.claude.json` (or `$CLAUDE_CONFIG_DIR/.claude.json`): remove the
  project's `hasTrustDialogAccepted`; `~/.claude/settings.json`: remove
  `skipDangerousModePermissionPrompt`.
- `~/.codex/config.toml` (or `$CODEX_HOME/config.toml`): remove the
  `[projects."…"]` table, `check_for_update_on_startup`, and the
  `[notice.model_migrations]` entry.
- `~/.notagent/agent/hooks.json`: one `UserPromptSubmit` entry gated on
  `NOTMUX_RUN_ID` that drains the run's inbox into the prompt's context
  (notagent already delivers a prompt hook's stdout as context). It is
  written beside the existing status hooks by `notmux hooks setup notagent`
  and removed by `notmux hooks uninstall notagent`. Nothing else is written
  into the notagent directory.

Every trust write is recorded on the run (`trust_writes` in
`notmux agent get <run>`).

## Environment of a managed process

| Variable | Meaning |
|---|---|
| `NOTMUX_RUN_ID` | the run; `notmux agent` reads it as identity |
| `NOTMUX_PARENT_RUN_ID` | the root (workers) |
| `NOTMUX_AGENT_ROLE` | `root` or `worker` |
| `NOTMUX_AGENT_HARNESS` | `notagent`, `claude`, `codex`, `generic` |
| `NOTMUX_TASK_FILE` | the assignment document (workers) |

Ordinary shells never inherit these variables, so a shell opened from a
worker pane cannot impersonate it.

## Delivery modes

- **Cooperative** (notagent, Codex, and Claude when its hook hands over):
  the worker runs `notmux agent next`, which blocks until an assignment or
  messages arrive, or the timeout elapses (default 100 s, below the 120 s
  default of Claude Code's Bash tool).
- **Hook** (Claude Code only): at the end of a turn the per-run Stop hook
  runs `notmux agent hook --event stop`. If the current assignment is
  unreported, the first such stop answers at once with the instruction to
  report; a later one (the payload's `stop_hook_active`) waits for messages
  instead, so a worker waiting for the root is not nudged in a loop.
  Otherwise the hook waits up to 540 s inside Claude's 600 s hook timeout
  for the next assignment or messages and continues the conversation with
  them (`decision: block`). When nothing arrives it hands over to
  cooperative `next`, so a worker never idles at the prompt.
  `UserPromptSubmit` drains unread messages into the prompt's context.

Messages are repeated in every hook delivery and every `next` until
acknowledged. Hooks never acknowledge: a hook's stdout does not prove the
harness passed it to the model, so only the agent marks a message read,
with `notmux agent ack <id>` (every delivered message carries its id and
this instruction). The explicit `inbox --drain` command acknowledges, and
only after its output was written and flushed; a failed write acknowledges
nothing. notagent caps hook output at 4000 UTF-16 code units; the
prompt-hook drain counts in those units, delivers only messages that fit
whole, and names at most ten of the rest as still unread.

Measured shell-tool limits: Claude Code's Bash tool defaults to 120 s
(`BASH_DEFAULT_TIMEOUT_MS`), hence the 100 s `next`; notagent's bash tool
and Codex's shell showed no cap below that in the versions tested.

## Lifecycle and exit codes

Process state (`starting`, `running`, `exited`, `lost`), assignment state
(`queued`, `running`, `succeeded`, `failed`, `cancelled`, `interrupted`)
and availability (`starting`, `busy`, `waiting`, `idle`, `stopped`) are
kept apart. A generic worker's exit code decides its assignment (0 =
succeeded). A reported worker whose process exits before `finish` leaves
its assignment `interrupted`. A root exit interrupts its group. Managed
processes are killed when NotMux quits (menu quit and window close); on
the next start every run that was live is `lost`, its open assignments
`interrupted`, its root `interrupted`. The managed panes stay in the layout
as inert panes whose header names the interrupted run (`agent · run <id> ·
interrupted · process lost`); they never start a shell or reattach, and
closing one forgets it. `notmux agent list --run <id>` (or `tasks`,
`inbox`) reads the run's group history from any pane.

`notmux agent` exits 0 on success, 1 on an API error (stderr carries
`<code>: <message>`), 2 on a usage error.

## Storage

`<config dir>/orchestration/store.json` (version 3), written atomically by
one writer; a mutation that changes nothing costs no write. The previous
version-2 prototype store is migrated and kept as `store.v2.json`. A
document that cannot be read (invalid JSON, wrong schema, unknown or newer
version) disables orchestration with a `storage_failed` error naming the
file; it is never replaced by an empty store. Per-run files live in
`orchestration/runs/<run id>/` (`task.md`, `system-prompt.md`,
`claude-settings.json`).

A launch is planned, prepared (trust files, executable) off the UI thread
under the 30 s launch deadline, and committed. If the run cannot be
recorded as started after its process is up, the process is killed and
the pane removed again, and the run is `lost` with the error.

No store write happens on the UI thread. The UI thread only plans a
launch (store reads), starts the process and inserts its pane, removes a
pane whose start could not be recorded, and looks up the project of a
terminal for `register`. Recording the launch intent, recording the start,
every other command, and recording process exits run on background
threads.

The request-id check runs inside the single store writer, in the same
write as the operation, so concurrent retries of one request apply it
once and a different payload under the same id is a conflict. A launch
reserves its request id when the intent is recorded; a failed start
releases it.

The intent also records the terminal id the process will get, so a
process that exits before its start is recorded is still found: its exit
is recorded (a generic worker's exit code decides its task, a reported
task is interrupted), and the start never sets the finished run back to
`running`.

A confirmed stop wins over a start in progress. Right before the process
is started the run is checked (not stopped, root active, deadline not
passed); a stop confirmed earlier means the program never runs. Right
after the start it is checked again: a stop that committed in between
either finds the new terminal registered (its kill works) or is seen by
this check, which kills the process before its pane is shown. As a last
line, recording the start is refused for a stopped run.

The launch deadline covers preparation and the wait before the process
start. Preparation is cancelled at the deadline: adapters check the
cancel flag before every write, and a trust file locked by another editor
is waited for only until then. A write already in progress when the
deadline passes finishes (one atomic, idempotent file replacement);
nothing starts after it. The process start itself is a local PTY spawn
and is not separately time-bounded. Waiters (`next`,
`wait`) sleep until a command actually changed the store; an answer that
changed nothing notifies nobody, so a waiting worker does not wake itself.

## Non-goals

No cascades (workers never spawn), no dashboard, no remote (paired-token)
access to `/v1/agents`, no automatic merging of worker results (the root
integrates), and no changes to any agent's own source, modes or hook
contract.
