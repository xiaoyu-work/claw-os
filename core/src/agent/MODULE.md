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
- Publish deterministic task and approval lifecycle notifications after durable
  state transitions.
- Attach built-in, app, and MCP tools to one guarded registry.
- Expose CLI, task-queue, and authenticated local web surfaces. Web Chat and
  Tasks use the same durable `clawd` queue, while Inbox projects the
  Notification Service and keeps raw context events in a separate diagnostic
  view. The queue's execution side runs in `claw-agentd`, never in the `clawd`
  broker — see [`../agentd/MODULE.md`](../agentd/MODULE.md).
- Keep CLI parsing and presentation grouped by command responsibility while
  `mod.rs` remains the composition and top-level routing boundary.

## Key Files

| Path | Role |
| --- | --- |
| `mod.rs` | Agent module composition and top-level `cos agent` routing |
| `command_catalog.rs` | Recursive-discovery metadata for the internal `agent dev` namespace |
| `conversation_commands.rs` | `ask`/`chat`, streaming terminal presentation, and interactive session UI |
| `session_commands.rs` | Conversation recall, session listing, titles, counts, purge, and statistics |
| `memory_commands.rs` | App memory, notes, semantic memory, and memory-learning commands |
| `skills_commands.rs` | Installed Skill inspection, provenance guards, usage, and hub operations |
| `provider_commands.rs`, `model_commands.rs` | Provider status/probes and model/auxiliary/retry/compression diagnostics |
| `media_commands.rs`, `vision_commands.rs` | Generated media/playback and image routing/analysis commands |
| `mcp_commands.rs` | MCP server status, probing, calls, and stdio serving |
| `safety_commands.rs` | Tool/approval/guardrail inspection, redaction, file safety, and OSV commands |
| `curator_commands.rs` | Skill-draft proposal, review, authoring, and session scanning |
| `app_ai_commands.rs`, `task_commands.rs` | App AI budgets/overrides and todo/nudge commands |
| `diagnostic_commands.rs`, `developer_commands.rs`, `text_commands.rs` | Public usage and observability, context/hooks, and text/prompt developer commands |
| `setup.rs` | Provider/modality setup, OAuth, model discovery, verification |
| `setup/copilot.rs` | Copilot OAuth device flow and live model discovery |
| `setup/media.rs` | TTS/STT/image/embedding specs, wizards, status, and probes |
| `../../test/unit/agent/setup.rs` | Setup, status, apply, OAuth, and config regression tests |
| `runtime/loop_.rs` | Multi-turn orchestration, prompt restore/freeze, compression, and persistence |
| `runtime/deps.rs` | Explicit hooks, clock, semantic indexer, and runtime path context |
| `runtime/turn.rs` | One provider turn, hooks, tool ordering, results |
| `service.rs`, `../../test/unit/agent/service.rs` | Task queue, approval-wait state, ownership/lease records, and `execute_job` — the runtime entry the `agentd` worker calls |
| `llm/types.rs` | Provider-neutral request, response, content, and stream types |
| `llm/registry.rs` | Provider construction |
| `llm/providers/` | Provider-specific authentication and wire adapters |
| `llm/accumulate.rs` | Streaming events to complete response/history |
| `tools/registry.rs` | Tool exposure, dispatch lookup, and explicit registry dependencies |
| `tools/progressive.rs` | Stable search/describe/call projection for deferred App/MCP tools |
| `skills/loader.rs`, `skills/disclosure.rs` | Layered Skill discovery and progressive model disclosure |
| `tools/mcp/` | Outbound/inbound MCP and lifecycle integration |
| `memory/sqlite_fts.rs` | Durable messages, content-addressed session prompts, and FTS |
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

A denied capability moves a task to the durable `waiting_approval` state after
the worker exits. The `clawd` supervisor requeues the same task and session
when every linked request is approved, or finishes it with an error when a
request is denied, disappears, or remains undecided for eight hours. The Web
surface reads owner-owned task memory directly, reads approval state through
`clawd`, and requires the installed `pkexec` helper for decisions.

`runtime/turn.rs` is the contract seam between providers and tools. Provider
changes must preserve equivalent streaming/non-streaming text, tools, opaque
reasoning state, usage, and error behavior. Tool calls only execute through the
registry, guardrails, and hooks.

CLI, web, and worker composition roots snapshot `Arc<CosConfig>`, resolve
runtime/registry paths, and open optional stores before calling
`runtime::loop_::run_with_deps`. Lower runtime code receives these dependencies
through typed contexts rather than rediscovering process state.

## Tests

Unit test bodies mirror their production modules under
`../../test/unit/agent/`. Command tests use the corresponding
`*_commands.rs` file:

```bash
cargo test -p cos agent::setup::tests -- --test-threads=1
cargo test -p cos agent::runtime::turn::tests -- --test-threads=1
cargo test -p cos agent::llm::providers::<provider>::tests -- --test-threads=1
cargo test -p cos agent::llm::accumulate::tests -- --test-threads=1
```

For an LLM-provider change, test setup/discovery, auth, request serialization,
non-streaming parsing, SSE conversion, tool/reasoning round-trips, and
pool/fallback classification.

## Change Together

- `mod.rs` command inventory and the owning `*_commands.rs` dispatcher when a
  top-level command or `dev` subcommand is added, removed, or renamed.
- `command_catalog.rs`, `cli_catalog.rs`, and `tools/cos_help.rs` when recursive
  command discovery changes.
- A production command module and its matching
  `../../test/unit/agent/<module>.rs` test body.
- `provider_commands.rs` with `setup.rs` and `doctor_cli.rs` when the shared
  provider probe contract changes.
- `conversation_commands.rs` with `runtime/loop_.rs`,
  `runtime/presentation.rs`, and `memory/` when chat streaming or continuation
  semantics change.
- `mcp_commands.rs` with `tools/mcp/` and worker launch policy when MCP process
  handling changes.
