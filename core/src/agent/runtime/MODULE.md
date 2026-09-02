# Agent Runtime Module

## Purpose

`runtime/` orchestrates a complete agent request from persisted user input
through model turns, tools, hooks, progress, and final records.

## Responsibilities

- Build and run bounded multi-turn loops.
- Restore or freeze one versioned canonical system prompt per persisted session.
- Keep due reminders and application context request-local.
- Load the latest verified durable compaction plus its uncompacted tail for
  every continuation surface.
- Track raw row provenance through each turn so a prepared compaction can be
  committed before its summary becomes model-visible.
- Adopt a concurrent compaction winner's verified summary/tail when a
  pre-lock plan becomes stale or already covered, then recheck and boundedly
  replan until the request is compliant or fails explicitly.
- Merge unpersisted live messages by neighboring durable row anchors during
  winner adoption, preserving tool evidence and rejecting any ephemeral that
  the winner collapsed without including in its durable inputs.
- Execute one provider/tool-result turn.
- Dispatch parallel-safe and serial tools deterministically.
- Build every provider tool schema and dispatch lookup from the same
  session-scoped exposure context.
- Keep provider tool schemas deterministic while a session's exposure and
  attachment generation are unchanged; reproject after explicit invalidation.
- Treat `dangerous_tools` as a legacy name filter only; capability-aware
  proxies reach exact execution-time consent instead.
- Install a fresh task-local approval identity for every invocation and retire
  its consent state on completion or cancellation.
- Run lifecycle hooks and progress/heartbeat reporting.
- Fan out least-privilege Agent extension observations asynchronously; their
  output never mutates prompts, turns, tool calls, or authorization decisions.
- Record conversation, prompt injection, usage, and error state.

## Key Files

| Path | Role |
| --- | --- |
| `loop_.rs` | Request-level orchestration, durable projection/compaction, and turn repetition |
| `turn.rs` | Provider call, tool extraction, dispatch, results |
| `hooks.rs` | Pre/post tool and turn hooks |
| `progress.rs` | Tool progress and heartbeat contract |
| `background.rs` | Background agent task handling |
| `evidence.rs` | Evidence capture and presentation metadata |

## Dependencies

Runtime depends on provider-neutral LLM types, the guarded tool registry,
trusted tool-exposure context, prompt/memory services, and hooks. It never
executes a model-emitted tool call outside `turn.rs` dispatch. The dispatch
path repeats exposure checks before tool execution; exact capability checks
remain inside tools/providers after argument validation. Message order and
opaque provider state must survive every turn.

Progressive bridge envelopes are removed before pre-tool hooks, progress,
approval, audit, and execution, so those layers observe the underlying tool
identity rather than `cos_tool_call`.

`auto_deny_tools` remains a hard pre-dispatch block. `dangerous_tools` and
`auto_approve_tools` cannot grant or widen a capability; core primitive proxies
declare a capability-aware boundary and defer consent to `caps::require`.

## Tests

```bash
cargo test -p cos agent::runtime::turn::tests -- --test-threads=1
cargo test -p cos agent::runtime::loop_::tests -- --test-threads=1
```
