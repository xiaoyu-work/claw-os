# Claw TUI Backend Adapter

## Responsibility

This module implements the remote app-server protocol consumed by the **actual
upstream Codex TUI**, pinned at
[`a592c38c16cdd7623dacc9168926ebccedfb67d3`](https://github.com/openai/codex/tree/a592c38c16cdd7623dacc9168926ebccedfb67d3).
It does not run Codex's model, tools, sandbox, credential store, or execution
backend. This is a compatibility adapter, **not complete feature parity**.

The process-owning launcher supplies a securely bound Unix listener and a
shutdown receiver. Transport is WebSocket-over-Unix, not newline-delimited JSON.
The wire envelopes follow upstream's intentional omission of `jsonrpc`; the
standard `jsonrpc: "2.0"` marker is also accepted.

## Entry Points and Dependency Direction

- `Options { use_memory, max_turns, session_id }`.
- `serve(UnixListener, Arc<CosConfig>, Options, watch::Receiver<bool>)`.
- `initial_thread_id(&str)` checks owner-scoped canonical session visibility
  before producing the UUID accepted by upstream's `resume` command.

`initial_thread_id` reads the canonical `presentation_id`; it never invents an
alias. The launcher selects `resume <uuid>` explicitly. Supplying `session_id`
to `serve` validates that startup selection but does not alter `thread/start`:
that method always creates a new conversation with empty history.

```text
upstream TUI
  -> transport / protocol
  -> presentation consumers
  -> Backend definition
  -> BrokerBackend provider
  -> clawd task / conversation / approval services
  -> ordinary claw-agentd execution
```

See the [Agent module](../MODULE.md) and
[system architecture](../../../../ARCHITECTURE.md). Persistence and permission
decisions remain at those existing boundaries.

## Key Files

| File | Responsibility |
| --- | --- |
| `backend.rs` | Narrow injected service definition and execution-independent status |
| `broker.rs` | Typed broker calls, authenticated Skill catalogue, installed polkit helper |
| `protocol.rs`, `transport.rs` | Strict envelopes, bounded framing/queues, concurrent control and request handling |
| `bootstrap.rs`, `input.rs`, `models.rs` | Configured-provider catalogue, truthful configuration projection and explicit unsupported-option errors |
| `server.rs`, `threads.rs` | Request lifecycle, subscriptions, canonical conversation/task mappings |
| `history.rs`, `pagination.rs` | Separate execution-evidence projection, fail-closed history admission and identity-bound page cursors |
| `events.rs` | Text, summaries, tool identity/status, usage and terminal-job presentation |
| `approvals.rs` | Exact pending-root-request correlation; user answers are not grants |

## Feature Parity Ledger

| Upstream surface | Adapter behavior |
| --- | --- |
| `initialize`, `initialized` | Client shape/capability validation, opt-out notifications, Claw version/platform metadata |
| `account/read` | No invented Codex/ChatGPT account; Claw provider readiness is additional metadata. OpenAI authentication is not required by this adapter |
| `model/list` | Configured model plus Claw's matching-provider catalogue; Copilot uses the existing authenticated catalogue service, never a model-generation probe |
| `config/read`, `configRequirements/read` | Read-only allowlisted presentation of Claw ownership/defaults; never serializes the whole configuration or credentials |
| `modelProvider/capabilities/read` | Disables Codex-native namespace tools, image generation, and web search. Claw tools remain separate guarded tools |
| `thread/start` | Creates a real canonical conversation without submitting a synthetic task |
| `thread/read`, `thread/resume` | Owner-scoped metadata, verified retained task-history hydration and live-task reattachment using root-resolved UUIDs; legacy or incomplete bindings fail closed |
| `thread/list`, `thread/loaded/list`, `thread/unsubscribe` | Bounded owner/runtime views and subscription lifecycle; cursor/query mismatch is an error |
| `thread/name/set`, archive/unarchive/delete | Canonical title and soft-state updates; active-task restrictions remain enforced by the broker |
| `thread/fork`, `thread/revert` | Verified retained histories support whole-task boundaries, inherited source journals and revision-checked mutation. Active, clipped, legacy or partial histories fail before mutation. No file rollback is implied |
| `thread/turns/list`, `thread/items/list` | Paginated views hydrate actual retained task journals only after canonical binding and completeness checks |
| `turn/start` | Durable Claw task submission with memory/max-turn defaults and catalogued per-task `model`; one active task per loaded conversation |
| `thread/settings/update` | Model selection for subsequent tasks in the loaded thread; does not alter an active task, provider, credentials or global configuration |
| `turn/interrupt` | Exact current task cancellation; acknowledgement is not early `turn/completed` |
| Text/reasoning/tool/status events | Genuine Claw text, provider summaries, tool IDs/names/results status, approval wait/resume and token usage |
| Approvals | Structured `item/tool/requestUserInput` review, exact once/deny choices, installed polkit helper and subsequent root-state confirmation |
| `skills/list` | Existing authenticated Claw catalogue, including disabled/quarantine diagnostics; no new discovery roots |
| Input queuing | The TUI retains queued text and submits after completion. Busy submissions/`turn/steer` are explicit errors; no input is silently discarded |

### Explicitly unavailable

- Per-turn arbitrary provider/config/reasoning/service-tier/personality changes,
  custom developer/base instructions, output schemas, collaboration/plan mode,
  and dynamic client tools. Matching advertised defaults do not reconfigure the
  worker.
- Attachments, audio, image/Skill/mention input, rich text-element metadata,
  live steering, durable server-side queue editing and active-turn settings changes.
- Codex-local command/process/PTY/filesystem APIs, apply-patch/diff UI, direct
  tool execution, native tool result bodies, and speculative plan/diff events.
- MCP connection/resource/tool/OAuth control: worker-owned connections cannot
  honestly be reported or controlled from this process.
- Manual compaction, goal/subagent orchestration, memory settings/reset,
  configuration writes, project/section metadata, plugins/marketplaces/apps,
  hooks, remote control, realtime voice, cloud account/rate-limit/billing,
  feedback upload and upstream update operations.

These calls fail with JSON-RPC errors. They do not return fabricated success or
empty catalogues as a substitute for a missing backend.

### Upstream frontend limits

The pinned frontend also has limits that an adapter cannot remove:
explicit remote mode rejects `--worktree`, and realtime voice requires the
separate native voice runtime as well as a Claw protocol implementation.
Account-gated screens must not be enabled by inventing ChatGPT entitlements.

The unpatched frontend independently fetches announcement/cloud/auth data during
startup. The frontend build's reviewed explicit-remote startup patch addresses
those automatic paths; on-demand pet/editor/browser actions are separate.
Disabling updates, analytics and feedback does not itself make the frontend
offline. Patch attribution and verification, launcher credential isolation,
environment sanitization, and any frontend network boundary belong to the
[upstream launcher contract](../../../../terminal/README.md).

## Canonical Backend Limits

Canonical memory now records explicit message-to-task membership, original
source session/message identities, outer-user boundaries, retained fork lineage
and revision-checked revert state. The broker verifies those bindings against
owner-scoped jobs before setting `task_bindings_complete`; only then does the
adapter hydrate stored turns or translate a fork/revert selection. Legacy,
partial, foreign, pruned or clipped evidence remains unavailable rather than
being guessed from text, timestamps, roles or counts.

1. Add arbitrary older-history pagination beyond the broker's bounded complete
   view. Existing row/job counts and truncation flags are honored; the adapter
   refuses clipped views rather than pretending they are complete.
2. Add real broker/runtime contracts for reasoning/settings, working directories,
   workspace constraints, attachments, steering/queues, compaction, and
   worker-owned MCP/tool state before enabling their TUI controls.

Model selections are constrained to the configured provider's catalogue snapshot.
Native providers reuse Claw's static metadata; custom endpoints without discovery
advertise only their configured model. Copilot discovery resolves the configured
credential source/pool through Claw and filters the existing authenticated
catalogue's selectable chat models. No credentials or entitlement details cross
the TUI socket.

Selected models are sent as `task.submit.model`, checked against the broker's
`requested_model` acknowledgement, and executed through the signed worker
contract. The last submitted `requested_model` restores the choice on resume;
an unsubmitted loaded-thread choice is runtime state, not a new persistent
configuration store. The model-only settings route does not emit an invented
collaboration-mode settings notification. Reasoning effort and provider/backend
overrides remain explicitly unavailable.

Thread UUIDs come exclusively from the canonical conversation service's
`presentation_id` metadata. `agent.conversation.get` accepts either that UUID or
the opaque canonical session ID and performs the owner-scoped reverse lookup.
The adapter neither derives UUIDs nor scans a recent list to resolve them.
`thread.sessionId` continues to carry the real Claw session ID. Loaded UI state
can retain the metadata for its notifications, but it is not an alias registry
or a second transcript. Fork lineage resolves the parent's presentation
identity through its canonical metadata, including when the parent is
soft-deleted.

Upstream's `ThreadId` parser accepts these UUIDv8 identities without a version
check, while its own generator uses UUIDv7 and notes that some consumers rely
on v7. Parsing/schema acceptance is not evidence for every such consumer.
Existing canonical presentation identities must not be silently changed to
another version; full remote lifecycle behavior needs separate validation.

## Safety and Lifecycle

- Refuses root, like `task.submit`; accepts same-user Unix peers only from the
  owning process or its directly launched TUI child. Same-uid workers belonging
  to `clawd` cannot turn the adapter into a fresh ambient-task proxy.
- Limits HTTP upgrade bytes to 16 KiB, WebSocket messages/frames to 1 MiB,
  connected clients to four, ordinary in-flight requests to sixteen per
  client, and control work to eight per client.
- Uses bounded 64-message event queues, eight task subscriptions, and sixty-four
  loaded conversations. Slow writers time out rather than accumulate state.
- Text input is limited to 64 KiB; history responses are bounded separately
  from live current-turn item state.
- A provider `Done` closes a provider response and publishes usage. Only a
  terminal, identity-checked Claw job completes the outer TUI turn.
- Tool arguments, successful result bodies, encrypted reasoning, thought
  signatures and raw provider response items are never emitted.
- Disconnect/shutdown closes subscriptions and outstanding helper processes;
  it does not silently cancel durable tasks.
- Watch shutdown also interrupts startup/session lookup and stops an idle
  listener before the first connection. Dropping the owner watch has the same
  effect; no durable task cancellation is inferred.
- Codex sandbox settings cannot broaden Claw authority. Thread responses
  identify an external sandbox; read-only/workspace-write policies that the
  current Claw task contract cannot implement are rejected.

## Validation

From the repository root, in Linux/WSL:

```bash
cargo test -p cos --lib agent::tui_backend:: -- --test-threads=1
```

Unit bodies live under `core/test/unit/agent/tui_backend/`. The mock implements
the same narrow backend definition; tests make no model requests. Tests cover
the real WebSocket upgrade, initialization, out-of-order replies, cancellation
during streaming, bounded inputs/queues, terminal lifecycle, opaque-state
exclusion, owner IDs/history, unsupported operations, and root review flow.

An optional conformance check uses the pinned upstream checkout's generated
JSON schemas and the existing Python `jsonschema` package, without contacting
any server or model:

```bash
python3 core/test/unit/agent/tui_backend/validate_upstream.py --upstream ../codex
```
