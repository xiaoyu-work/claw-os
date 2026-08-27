# Agent Runtime Module

## Purpose

`runtime/` orchestrates a complete agent request from persisted user input
through model turns, tools, hooks, progress, and final records.

## Responsibilities

- Build and run bounded multi-turn loops.
- Restore or freeze one versioned canonical system prompt per persisted session.
- Keep due reminders and application context request-local.
- Execute one provider/tool-result turn.
- Dispatch parallel-safe and serial tools deterministically.
- Run lifecycle hooks and progress/heartbeat reporting.
- Record conversation, prompt injection, usage, and error state.

## Key Files

| Path | Role |
| --- | --- |
| `loop_.rs` | Request-level orchestration and turn repetition |
| `turn.rs` | Provider call, tool extraction, dispatch, results |
| `hooks.rs` | Pre/post tool and turn hooks |
| `progress.rs` | Tool progress and heartbeat contract |
| `background.rs` | Background agent task handling |
| `evidence.rs` | Evidence capture and presentation metadata |

## Dependencies

Runtime depends on provider-neutral LLM types, the guarded tool registry,
prompt/memory services, and hooks. It never executes a model-emitted tool call
outside `turn.rs` dispatch. Message order and opaque provider state must survive
every turn.

## Tests

```bash
cargo test -p cos agent::runtime::turn::tests -- --test-threads=1
cargo test -p cos agent::runtime::loop_::tests -- --test-threads=1
```
