# Claw Agent terminal

`cos agent chat` provides a Claw-owned full-screen terminal UI. Its renderer,
state machine, key handling, broker client and stream projection live under
`core/src/agent/terminal/` and compile into `cos`. There is no downloaded or
separately packaged TUI executable, compatibility app server, or second Agent
backend.

## Launch

Run as the ordinary account that configured the Claw Agent:

```bash
cos agent chat
cos agent chat --session <session-id>
cos agent chat --no-memory --max-turns 8
```

The full-screen UI is selected only when stdin, stdout and stderr are terminals
and `TERM` is not `dumb`. `--tui` requires that environment explicitly. Root
cannot submit Agent work on behalf of another account.

The compatible line interface remains available:

```bash
cos agent chat --plain
printf 'Explain the current task\n/quit\n' | cos agent chat
```

`--no-stream` and `--show-tools` retain their line-interface behavior. They
cannot be combined with `--tui`. The Claw UI does not accept another
frontend's flags after `--`.

## Presentation

The UI has four stable regions:

1. A Claw header with canonical conversation title, model, session and task
   state.
2. A scrollable transcript with separate user, assistant, reasoning, tool,
   approval, system and error entries.
3. A dynamically sized multiline composer with Unicode-safe editing, paste
   handling and prompt history.
4. A status line with controls, queued-input count, scroll position and token
   usage.

Working tasks show an animated status and elapsed time. Tool rows retain their
identity, outcome and reported duration without exposing arguments or successful
result bodies. Markdown headings, lists, quotes, fenced code, bold text and
inline code receive terminal-native styling.

Model text is control-character sanitized and passed through the shared secret
redactor before rendering. Tool inputs, successful result bodies, encrypted
reasoning, thought signatures and raw provider payloads never enter the
terminal. Tool identity and success/failure remain visible.

Controls:

- `Enter` submits the composer.
- `Shift-Enter`, `Alt-Enter` or `Ctrl-J` inserts a newline.
- `Esc` cancels the exact current task.
- `PageUp` / `PageDown` scroll the transcript.
- `Up` / `Down` navigate multiline input, prompt history, command suggestions
  or an active picker according to focus.
- `Ctrl-K` or typing `/` opens the supported Claw command palette; `Tab`
  completes the selected match.
- `Ctrl-C` cancels active work, or exits while idle.
- `Ctrl-D` exits while idle.
- During an approval, `a` requests one exact authorization and `d` requests
  denial through the installed OS helper. The frontend response itself grants
  nothing.
- In a durable task detail, `c` cancels a non-terminal task, `r` retries a
  terminal task as a new durable task, and `Up` / `Down` scroll its bounded
  redacted result.
- In Approval Center, `a` requests one-time approval and `d` requests denial
  only while the selected request is still pending. Recent decisions are
  read-only.
- In Notification Inbox, `m` marks an unread record read, `a` acknowledges it
  and `d` dismisses it. Opening a detail does not mutate durable state.
- In notification settings, `w`, `e`, `n` toggle Web, desktop and ntfy
  delivery; `q` toggles all-day DND. Exact UTC windows use `/dnd`.

Text entered while a task is active is queued locally and submitted after that
task reaches a terminal state. Exiting does not silently cancel durable work.

## Commands

The command set names Claw concepts only:

```text
/help
/new
/sessions
/resume [session-id]
/rename <title>
/archive
/unarchive
/fork
/rewind <user-turn-count>
/models
/model [model-id]
/skills
/tasks
/task [task-id]
/approvals
/approval [approval-id]
/inbox [all]
/notification [notification-id]
/notify-settings
/notify-channel <web|desktop|ntfy> <on|off>
/notify-severity <web|desktop|ntfy> <info|warning|error|critical>
/dnd <off|HH:MM-HH:MM>
/session
/clear
/cancel
/quit
```

`/rewind` changes retained conversation replay only. It does not roll back
files, processes or other admitted effects. `/clear` clears the current
terminal view without deleting canonical history. The terminal deliberately
does not present a permanent-delete action unless the backend can provide that
exact contract.

`/archive` and `/rewind` open a Claw-owned confirmation panel that states the
durability and external-effect consequences before any broker mutation.

`/model` without an ID opens a searchable configured-provider picker. `/resume`
without an ID opens a searchable bounded conversation picker. These lists are
presentation only; selecting an item still uses the normal broker operation and
canonical identity checks.

`/tasks` (or `/task` without an ID) opens the newest 100 owner-scoped durable
tasks. Selecting one fetches its current canonical detail, including status,
session, Activity association, timing, model, result or error. Cancellation
acts on that exact task. Retry is available only for terminal tasks and creates
the new pending task through `task.retry`; the TUI does not manufacture or
rewrite task state.

`/approvals` (or `/approval` without an ID) combines a bounded owner-scoped
view of pending and recent approval records. Details show the exact capability,
scope, risk, requester, session, reason and decision metadata. A historical
approved, consumed or denied record cannot be decided again. Pending decisions
use the installed privileged helper, verify pending state before authorization,
and verify the resulting root-owned state afterward.

`/inbox` opens the newest 100 retained, non-dismissed owner notifications;
`/inbox all` also includes dismissed records. Details show durable state,
source, severity, links to tasks/sessions, occurrence count, display-only
actions and per-channel delivery status. The TUI never executes a notification
action URI as authority.

`/notify-settings` shows the complete persisted delivery policy. Channel
toggles and `/notify-severity` preserve every unrelated preference.
`/dnd HH:MM-HH:MM` sets the exact UTC quiet window and `/dnd off` removes it.
The full preference document is validated and stored by the existing
Notification Service; the TUI does not own a parallel settings file.

## Backend ownership

```text
Claw ratatui renderer
  -> canonical agent.conversation.* broker routes
  -> durable task submit/stream/list/get/cancel/retry routes
  -> protected pending/recent approval reads and root-owned decisions
  -> owner-scoped notification state and delivery preferences
  -> claw-agentd and the shared guarded Agent runtime
```

The renderer owns only transient display state, the current composer, local
prompt history and queued text. `clawd` owns conversation identity and
mutation; `claw-agentd` owns model/tool execution; the existing capability,
approval, audit and Activity boundaries remain authoritative.

Conversation creation and resume use canonical `ses_*` IDs directly. History
is the bounded owner-scoped view returned by the conversation service. Fork
and rewind reuse its verified task/message bindings and fail closed for active,
legacy, partial or clipped history.

Per-task model selection is constrained to the configured Claw provider's
catalogue. It does not rewrite provider choice, credentials, fallback policy,
capabilities or another task's configuration.

## Validation

From the repository root in Linux/WSL:

```bash
cargo test -p cos --lib agent::terminal::tests -- --test-threads=1

cargo build -p cos --bin cos
original_namespace="$(readlink /proc/self/ns/mnt)"
for scenario in complete cancel commands confirmations task-center approval-center notification-inbox multiline resume plain; do
  unshare --user --map-current-user --keep-caps --mount --net \
    python3 -B core/tests/agent_tui_pty.py \
    --cos target/debug/cos \
    --case "$scenario" \
    --original-mount-namespace "$original_namespace"
done
```

Unit tests cover input parsing/editing, redaction, stream projection, approval
state and ratatui rendering. The PTY fixture drives the real `cos` binary,
requires an actual canonical task submission, observes streamed output,
cancels the matching task with `Esc`, resumes by presentation ID through the
canonical service, verifies rename/fork command routing, preserves bracketed
multiline paste, requires archive/rewind confirmation and confirms plain mode
never contacts the broker. The task-center scenario browses owner-scoped tasks,
cancels one exact running task and retries one exact terminal task. The
approval-center scenario browses pending and recent owner-scoped decisions and
keeps historical records read-only. The notification scenario performs exact
read, acknowledge and dismiss mutations and updates durable delivery/DND
preferences without a desktop dependency.
