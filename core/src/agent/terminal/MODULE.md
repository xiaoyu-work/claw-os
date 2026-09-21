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
| `state.rs` | Bounded transcript, composer, queue, approval and session state |
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
- Keep durable tasks running when the UI disconnects unless the user explicitly
  cancels. Task browsing, cancellation and retry must use the owner-scoped
  broker contract rather than terminal-local task records.
- Display only shared redacted presentation data. Never expose tool arguments,
  successful result bodies, opaque reasoning state or credentials.
- Approval keys call the installed OS helper for one exact pending request;
  no UI object or key press is permission.
- Approval browsing combines only owner-scoped pending and recent records.
  Historical records are read-only; pending decisions still pass through the
  same protected helper and post-decision status verification.
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
