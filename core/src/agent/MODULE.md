# Agent Module

## Purpose

`core/src/agent/` owns conversational agent setup, prompt construction,
provider calls, multi-turn tool execution, memory, safety, usage, and user/API
surfaces.

## Responsibilities

- Configure providers and credentials without storing secret values in config.
- Freeze versioned, content-addressed system prompts per session and trace
  request-local model context separately.
- Run model turns, dispatch authorized tools, and preserve provider state.
- Maintain memory, sessions, checkpoints, audit views, and usage records.
- Attach built-in, app, and MCP tools to one guarded registry.
- Delegate dynamic App and MCP execution to the task-owned extension host when
  running inside `claw-agentd`.
- Expose CLI, task-queue, and authenticated local web surfaces. The queue's
  execution side runs in `claw-agentd`, never in the `clawd` broker — see
  [`../agentd/MODULE.md`](../agentd/MODULE.md).

## Key Files

| Path | Role |
| --- | --- |
| `mod.rs` | `cos agent` command family and user-facing probes |
| `setup.rs` | Provider/modality setup, OAuth, model discovery, verification |
| `setup/copilot.rs` | Copilot OAuth device flow and live model discovery |
| `setup/media.rs` | TTS/STT/image/embedding specs, wizards, status, and probes |
| `../../test/unit/agent/setup.rs` | Setup, status, apply, OAuth, and config regression tests |
| `runtime/loop_.rs` | Multi-turn orchestration, prompt restore/freeze, compression, and persistence |
| `runtime/turn.rs` | One provider turn, hooks, tool ordering, results |
| `service.rs`, `../../test/unit/agent/service.rs` | Task queue, ownership/lease records, and `execute_job` — the runtime entry the `agentd` worker calls |
| `llm/types.rs` | Provider-neutral request, response, content, and stream types |
| `llm/registry.rs` | Provider construction |
| `llm/providers/` | Provider-specific authentication and wire adapters |
| `llm/accumulate.rs` | Streaming events to complete response/history |
| `tools/registry.rs`, `tools/exposure.rs` | Immutable tool catalogue plus session-scoped exposure and dispatch lookup |
| `skills/loader.rs`, `skills/disclosure.rs` | Layered Skill discovery and progressive model disclosure |
| `tools/mcp/` | Outbound/inbound MCP and lifecycle integration |
| `memory/sqlite_fts.rs` | Durable messages, content-addressed session prompts, and FTS |
| `memory/recovery.rs` | Memory health, serialized repair, FTS rebuild, and evidence-preserving quarantine |
| `prompt/` | System prompt composition, tracing, caching |
| `safety/` | Redaction, file/tool safety, and external-data controls |
| `web/` | Authenticated local agent API and UI |

## Dependencies

The normal call direction is:

```text
CLI/web -> clawd task queue -> claw-agentd worker
  -> runtime loop
  -> prompt + memory
  -> Provider
  -> stream accumulator
  -> guarded tool registry
  -> tool result history
```

`runtime/turn.rs` is the contract seam between providers and tools. Provider
changes must preserve equivalent streaming/non-streaming text, tools, opaque
reasoning state, usage, and error behavior. Tool schemas and calls use the same
trusted per-request exposure context, then execute through registry
reauthorization, guardrails, approvals, and hooks.

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
