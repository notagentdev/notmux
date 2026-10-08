---
name: notmux-orchestration
description: Direct up to four worker agents in NotMux panes, or work as one of them. Use when a task is large enough to split across independent workers, or when NOTMUX_RUN_ID is set in your environment.
---

# NotMux agent orchestration

NotMux runs one **root** (the orchestrator) and up to four **workers** in
terminal panes of the same project. Every participant talks to NotMux with
one command, `notmux agent`, over the local NotMux API. Nothing is typed
into another agent's terminal; work and messages travel through NotMux.

Your identity comes from the environment NotMux set for your process:

- `NOTMUX_RUN_ID` — your run ID. `notmux agent` reads it automatically.
- `NOTMUX_AGENT_ROLE` — `root` or `worker`.
- `NOTMUX_PARENT_RUN_ID` — the root's run ID (workers only).
- `NOTMUX_TASK_FILE` — the file with your current assignment (workers only).

Rules NotMux enforces, so do not try to work around them:

- Only the root spawns, assigns, and registers. **Workers never start
  workers.** A worker that needs help sends a message to the root.
- A root has at most **4 live workers** and a fixed **start budget**;
  `notmux agent list` shows both.
- A worker runs **one assignment at a time**; further assignments queue in
  order.
- Every result is reported once and never changes.
- Messages stay inside one root's group.

## Commands

`notmux agent …` are **shell commands**: run them with your shell (bash)
tool exactly as written. They are not tools of your own, and there is no
tool named `notmux`, `finish` or `next`.

All commands print JSON on stdout unless noted. Errors go to stderr with a
non-zero exit code and a stable `code` such as `root_only`,
`capacity_exceeded`, `budget_exhausted`, `already_finished`.

| Command | Who | What |
|---|---|---|
| `notmux agent register --harness <notagent\|claude\|codex> --budget <n>` | a pane that wants to become root | Bind this pane as root. Idempotent. |
| `notmux agent lead --harness <h> [--budget <n>] [--project <id>] [--cwd <dir>]` | any pane that is not a worker | Open a new orchestrator pane. A worker is refused (`root_only`). |
| `notmux agent spawn --name <name> --harness <h> --task <text> \| --task-file <file> [--cwd <dir>]` | root | Start a worker with its first assignment. Prints the run and the task. |
| `notmux agent spawn --name <n> --harness generic --task <t> -- <program> [args…]` | root | Start any executable; its exit code is the result. |
| `notmux agent assign --worker <run-id> --name <name> --task <text> \| --task-file <file>` | root | Queue another assignment on a running worker. |
| `notmux agent list` | any | Every run of your group with process state, availability, and the root's budget. |
| `notmux agent get <run-id>` | any | One run. |
| `notmux agent task <task-id>` | any | One assignment with its result. |
| `notmux agent tasks --worker <run-id> [--after <seq>]` | any | A worker's assignments in order. |
| `notmux agent message --to <run-id> --body <text>` | any | Send a message inside the group. |
| `notmux agent inbox [--unread] [--after <seq>]` | any | Read your messages. `--drain` also acknowledges them. |
| `notmux agent ack <message-id>` | any | Acknowledge a message. |
| `notmux agent wait [--after <seq>] [--timeout <ms>]` | root | Block until something happened in the group (events with sequence numbers). |
| `notmux agent next [--after <task-id>] [--timeout <ms>]` | worker | Block for the next assignment or pending messages. Without `--after`, an assignment you still hold is delivered again (so a lost answer cannot strand it); pass `--after <task-id>` while you hold it to wait for messages only. |
| `notmux agent finish --task <task-id> --outcome succeeded\|failed --result <text> \| --result-file <file>` | worker (or root for any task) | Report the result of an assignment. |
| `notmux agent finish --outcome succeeded\|failed --result <text>` | root | Close the orchestration; every worker is stopped. |
| `notmux agent stop [<run-id> \| --target <run-id>]` | root: one worker, or the whole group when no run is named; worker: itself | Stop and cancel open assignments. Unknown options are errors. |
| `notmux agent cancel [<run-id> \| --target <run-id>]` | same as stop | Alias kept for scripts. |

**Messages stay unread until you acknowledge them.** However a message
reaches you (from `next`, `inbox`, or pasted into your prompt by a hook),
run `notmux agent ack <message-id>` once you have read it. NotMux cannot
tell whether a delivery reached you, so an unacknowledged message is
delivered again and again.

Task and result texts are limited (64 KiB task, 256 KiB result); a limit is
an error, never a truncation. Prefer `--task-file` and `--result-file` for
long texts.

## If you are the root

1. Decide the split. Independent, well-bounded pieces only; a worker cannot
   ask you questions while it works except by message, so put every
   constraint, acceptance criterion, and file boundary into the task text.
2. `notmux agent spawn` one worker per piece (at most four live at once).
   Give each worker its own working directory or clearly disjoint files;
   workers do not coordinate with each other unless you tell them to.
3. Loop: `notmux agent wait --after <last sequence>` and react to events:
   `assignment_finished` → `notmux agent task <task-id>` to read the result;
   `message_sent` addressed to you → `notmux agent inbox --unread` and
   answer with `notmux agent message`.
4. Queue follow-up work with `notmux agent assign`. A worker that reported
   its assignment blocks in `next` and picks the queued one up immediately.
5. Integrate the results yourself. Workers never merge, push, or touch each
   other's files.
6. When everything is integrated, `notmux agent finish --outcome … --result …`
   with a summary. That stops the workers.

A worker's result is its report; verify it (run the tests, read the diff)
before you build on it.

## If you are a worker

1. Read `NOTMUX_TASK_FILE` completely. It is your only assignment.
2. Do exactly that assignment in your working directory. Do not start other
   agents, do not create worktrees or branches unless the task says so, do
   not push.
3. If the task is impossible or ambiguous, do the possible part and say what
   is missing in your result; for a blocking question send
   `notmux agent message --to $NOTMUX_PARENT_RUN_ID --body …` and wait for
   the answer with `notmux agent next --after <your task id>` (repeat it
   until messages arrive; `--after` keeps your own task from being handed
   to you again).
4. Report with `notmux agent finish --task <task-id> --outcome succeeded|failed --result …`.
   The task ID is in the assignment file header and in `notmux agent list`.
   Include: what you changed (files), how you verified it, what you did not
   do.
5. Then `notmux agent next`. It blocks until the root assigns more work,
   returns pending messages, or tells you that you are stopped. When it
   returns a task, work on it and report the same way. When it says
   `stopped`, you are done.

Check `notmux agent inbox --unread` when a step takes long; the root may
have sent constraints or a stop request.
