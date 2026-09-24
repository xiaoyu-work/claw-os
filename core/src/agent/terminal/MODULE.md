# Claw Agent terminal module

## Responsibility

Own the in-process terminal presentation for the shared Claw Agent backend.
This module renders UI and translates human input into existing canonical
conversation, durable task, model, Skill and approval operations. It does not
own execution, persistence, provider credentials, capabilities, consent or
audit.

## Key files

| Path | Role |
| --- | --- |
| `mod.rs` | CLI options, terminal lifecycle, async event loop, commands and task streaming |
| `state.rs` | Bounded transcript, composer, durable-task handles, approval and session state |
| `commands.rs` | Closed Claw slash catalogue and canonical conversation/model/Skill actions |
| `stream.rs` | Durable task submission, identity/model acknowledgement and stream polling |
| `ui.rs` | ratatui layout, styling, Markdown-oriented transcript projection and cursor |
| `presentation.rs` | Redacted model/tool/reasoning/progress projection |
| `backend.rs` | Narrow typed consumer of canonical `clawd` routes and protected approval helper |
| `models.rs` | Configured-provider model catalogue without provider mutation |
| `../../../test/unit/agent/terminal/mod.rs` | State, rendering, privacy and option regression tests |
| `../../../tests/agent_tui_pty.py` | Real-binary PTY completion/cancellation/plain scenarios |

## Boundaries

- Use canonical `ses_*` conversation IDs and ordinary broker routes directly.
- Provider setup is not a prerequisite for opening local sessions, tasks,
  approvals, notifications, or Activities. Show readiness errors explicitly
  and require setup only before model work. Live catalogue failure retains only
  validated configured models and remains visibly degraded, never silent.
- Keep durable tasks running when the UI disconnects unless the user explicitly
  cancels. Task browsing, cancellation and retry must use the owner-scoped
  broker contract rather than terminal-local task records.
- A transient broker disconnect keeps the exact task and stream cursor,
  displays a reconnecting state, and resumes the durable stream without
  resubmitting work.
- Startup retries replay-safe reads and only conversation creation failures
  proven not to have been dispatched. An ambiguous create is never replayed.
- Opening a conversation reattaches its retained non-terminal task and exact
  sequenced successors without duplicating the already recorded user prompt.
  Its latest requested model is restored only when it remains in the current
  configured-provider catalogue.
- `Ctrl-T` edits only supported future-task defaults: configured model,
  reasoning effort, memory use and max turns. It never mutates provider
  credentials or global config. Explicit effort is persisted on the Job and
  fails unless Copilot selects a Responses-capable model.
- Display only shared redacted presentation data. Never expose tool arguments,
  successful result bodies, opaque reasoning state or credentials.
- Approval keys call the installed OS helper for one exact pending request;
  no UI object or key press is permission.
- Approval browsing combines only owner-scoped pending and recent records.
  Historical records are read-only; pending decisions still pass through the
  same protected helper and post-decision status verification.
- Notification Inbox consumes only owner-scoped durable notification routes.
  Read, acknowledge, dismiss, channel and DND changes are explicit broker
  mutations; opening a notification is presentation only.
- Activity views consume the shared owner-scoped Activity backend. Execution
  results never complete a goal; completion and cancellation use explicit
  confirmation and never imply task cancellation or effect rollback.
- Activity controls display canonical finite limits, configured monetary
  accounting, pending priority and capability constraints. Mutations carry an
  explicit fetched revision (or `new`) and never retry a stale CAS.
- Activity Evidence aggregates authenticated object declarations, immutable
  reported receipts and object-state annotations without resolving App data.
  Operation previews stay metadata-only and file plans stay App-owned.
- Review projects only attached Files change-plan references and their reported
  receipt diffs from Activity Evidence. It neither scans the workspace nor
  resolves, applies, or authorizes a plan.
- Keep slash completion limited to high-level entry points and common
  conversation actions. Object lifecycle, policy and evidence actions belong
  in the panel opened by that entry point; compatible direct forms may remain
  parseable without appearing in completion.
- While idle, double `Esc` opens retained user turns. Selecting one creates a
  backend-checked prefix fork and restores the selected prompt for editing;
  it never rewinds effects or forks active work.
- Workspace selection resolves through `task.workspace.resolve`; the TUI keeps
  only the returned canonical path and binds it to each submitted/queued task.
  A cwd is context, never filesystem authority.
- Follow-up text is published immediately as a durable pending Job whose exact
  predecessor is broker-validated. Local state retains only bounded handles
  for stream attachment; it is not the queue authority.
- `Ctrl-O` prepares one explicit image path under the verified owner home.
  The terminal reads bounded bytes and submits the existing attachment wire
  object; no path or filesystem capability reaches the task.
- `Ctrl-F` lists at most 512 regular-file names under the selected workspace
  and inserts an `@relative/path` mention. Completion reads no file contents,
  follows no symlink directories, and grants no capability.
- `/copy` emits only the latest redacted Assistant display through bounded
  OSC 52. `/export PATH` creates one new `0600` Markdown file under the
  verified owner home from visible user/assistant entries; it never overwrites.
- `/raw` temporarily leaves the alternate screen, publishes the bounded
  redacted transcript to the main terminal scrollback, then restores raw mode
  and the full-screen UI.
- `/appearance` changes only local chrome: bounded accent theme, optional
  conversation terminal title, and compact/full status line. It mutates no
  backend or persisted configuration.
- Appearance also switches between the default Emacs-style bindings and a
  bounded Vim composer with explicit NORMAL/INSERT modes. Active-task `Esc`
  always retains its cancellation meaning.
- Keep prompt, transcript, queue, event and catalogue sizes bounded.
- Do not launch, embed, fetch or package another product's TUI or compatibility
  protocol. External interfaces may inform interaction design only.

## Validation

```bash
cargo test -p cos --lib agent::terminal::tests -- --test-threads=1
cargo test -p cos --lib conversations:: -- --test-threads=1
```

The real PTY commands are documented in
[`../../../../docs/agent-terminal.md`](../../../../docs/agent-terminal.md).
