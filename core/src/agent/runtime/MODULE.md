# Agent Runtime Module

## Purpose

`runtime/` orchestrates a complete agent request from persisted user input
through model turns, tools, hooks, progress, and final records.

## Responsibilities

- Own one bounded multi-turn lifecycle for buffered and streaming asks.
- Restore or freeze one versioned canonical system prompt per persisted session.
- Keep due reminders and application context request-local.
- Execute one provider/tool-result turn.
- Dispatch parallel-safe and serial tools deterministically.
- Run lifecycle hooks and progress/heartbeat reporting.
- Record conversation, prompt injection, usage, and error state.

## Key Files

| Path | Role |
| --- | --- |
| `loop_.rs` | Shared request lifecycle, public ask adapters, and turn repetition |
| `deps.rs` | Explicit runtime hooks, clock, semantic indexer, and path snapshot |
| `turn.rs` | Shared request/response/tool state transitions and provider delivery adapters |
| `hooks.rs` | Pre/post tool and turn hooks |
| `progress.rs` | Tool progress and heartbeat contract |
| `background.rs` | Background agent task handling |
| `evidence.rs` | Evidence capture and presentation metadata |

## Dependencies

Runtime depends on provider-neutral LLM types, the guarded tool registry,
prompt/memory services, and an explicit `RuntimeDeps`. Production composition
resolves hooks, audit/notes/nudge/Skill paths, clock, and semantic indexing
before entering the lifecycle. Delegated children inherit the parent's runtime
hooks, clock, and paths while retaining their own provider and narrowed tool
registry. It never executes a model-emitted tool call
outside `turn.rs` dispatch. Message order and opaque provider state must survive
every turn.

## Lifecycle ownership

`loop_::ask_inner` is the only request lifecycle owner. Buffered and streaming
entry points select a `LifecycleOutput`; the streaming adapter additionally
applies the user-visible stream/progress projection. Neither adapter records
messages, runs hooks, compresses context, verifies evidence, generates titles,
curates memory, or chooses terminal states.

```text
Prepare
  -> record user + injected context
  -> restore/freeze system prompt
  -> register interrupt + hooks
TurnReady
  -> cancellation check -> pre-turn hook -> scrub/compress
  -> turn::run_turn_inner
       -> build request -> Buffered(retry) | Streaming(sink)
       -> append assistant -> dispatch tools -> append tool results
  -> post-turn hook -> cancellation check
  -> observe evidence -> persist appended messages
  -> ContinueWithTools -------------------------------> TurnReady
  -> Final -> verify evidence -> title -> curate -> Success
  -> provider/hook/progress error --------------------> Error
  -> cancellation at any cancellation-aware boundary -> Interrupted
  -> final-turn provider failure/empty answer -> persisted fallback -> Success
```

`turn::run_turn_inner` similarly owns request construction, run logging,
assistant/tool-result message transitions, tool hooks, deterministic dispatch,
and finish-reason handling. `ProviderDelivery` varies only how the complete
provider response is obtained: buffered calls may retry before yielding a
response; streaming calls accumulate and forward events without retrying after
output may have begun.

## Change together

- Add request lifecycle side effects only in `ask_inner`, then extend the
  buffered/streaming differential tests in `test/unit/agent/runtime/loop_.rs`.
- Change provider request or post-response/tool transitions only in
  `turn::run_turn_inner`; keep `ProviderDelivery` limited to response delivery.
- Keep public buffered/streaming entry points as argument and presentation
  adapters. They must not acquire independent persistence or terminal logic.

## Tests

```bash
cargo test -p cos agent::runtime::turn::tests -- --test-threads=1
cargo test -p cos agent::runtime::loop_::tests -- --test-threads=1
```
