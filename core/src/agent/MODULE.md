# Agent Module

## Purpose

`core/src/agent/` owns conversational agent setup, prompt construction,
provider calls, multi-turn tool execution, memory, safety, usage, and user/API
surfaces.

## Responsibilities

- Configure providers and credentials without storing secret values in config.
- Build traced system prompts and persist all model-visible context.
- Run model turns, dispatch authorized tools, and preserve provider state.
- Maintain memory, sessions, checkpoints, audit views, and usage records.
- Attach built-in, app, and MCP tools to one guarded registry.
- Expose CLI, daemon worker, and authenticated local web surfaces.

## Key Files

| Path | Role |
| --- | --- |
| `mod.rs` | `cos agent` command family and user-facing probes |
| `setup.rs` | Provider/modality setup, OAuth, model discovery, verification |
| `runtime/loop_.rs` | Multi-turn orchestration and persistence |
| `runtime/turn.rs` | One provider turn, hooks, tool ordering, results |
| `llm/types.rs` | Provider-neutral request, response, content, and stream types |
| `llm/registry.rs` | Provider construction |
| `llm/providers/` | Provider-specific authentication and wire adapters |
| `llm/accumulate.rs` | Streaming events to complete response/history |
| `tools/registry.rs` | Tool exposure and dispatch lookup |
| `tools/mcp/` | Outbound/inbound MCP and lifecycle integration |
| `memory/sqlite_fts.rs` | Durable message/session memory and FTS |
| `prompt/` | System prompt composition, tracing, caching |
| `safety/` | Redaction, file/tool safety, and external-data controls |
| `web/` | Authenticated local agent API and UI |

## Dependencies

The normal call direction is:

```text
CLI/web/clawd worker
  -> runtime loop
  -> prompt + memory
  -> Provider
  -> stream accumulator
  -> guarded tool registry
  -> tool result history
```

`runtime/turn.rs` is the contract seam between providers and tools. Provider
changes must preserve equivalent streaming/non-streaming text, tools, opaque
reasoning state, usage, and error behavior. Tool calls only execute through the
registry, guardrails, and hooks.

## Tests

Most tests live inline with their owning module:

```bash
cargo test -p cos agent::setup::tests -- --test-threads=1
cargo test -p cos agent::runtime::turn::tests -- --test-threads=1
cargo test -p cos agent::llm::providers::<provider>::tests -- --test-threads=1
cargo test -p cos agent::llm::accumulate::tests -- --test-threads=1
```

For an LLM-provider change, test setup/discovery, auth, request serialization,
non-streaming parsing, SSE conversion, tool/reasoning round-trips, and
pool/fallback classification.

