# Agent Context Module

## Purpose

`context/` defines bounded request packets, controls conversation context size,
and extracts safe references from model/user-visible text.

## Responsibilities

- Keep explicit request data and the pinned profile separate from system rules.
- Let the model select additional sources through guarded tools, not a keyword
  classifier or a separate compulsory planning call.
- Bound the complete request, including tools and non-ASCII text.
- Estimate context usage and compress older conversation safely.
- Preserve tool-call/result integrity across compression boundaries.
- Extract references/citations without executing or trusting them.
- Remove hidden thinking blocks where configured.

## Key Files

| Path | Role |
| --- | --- |
| `packet.rs` | Typed source-labelled ContextPacket and budgeted ContextBuilder |
| `budget.rs` | Configured/known-model input bounds and UTF-8-safe excerpts |
| `compressor.rs` | Token estimates, tail preservation, LLM summary |
| `references.rs` | Reference extraction/normalization |
| `think_scrub.rs` | Hidden reasoning tag removal |

## Dependencies

`runtime/context.rs` supplies owner-bound notes, explicit App/Activity context,
due reminders, and exposed source handles. The initial profile contains
`USER.md` and explicitly `[always]`-pinned entries only; it does not select
knowledge based on the question. Other notes and conversation history remain
behind model-invoked memory tools.

The model is explicitly instructed to continue with refined queries, source
pages or current authorized observations when retrieved memory is insufficient,
off-topic, stale or conflicting. Source revisions prevent mixing pages across
updates; search scores are not truth/confidence scores. Repeated unchanged
lookups without new evidence are not progress, and unresolved gaps remain
explicit when the existing turn/token/permission limits are reached.

Compression uses provider-neutral messages and a provider call, but it cannot
drop the current request or split tool pairs. Failed/empty summaries retain the
original history; the runtime reports an explicit error if the complete request
still exceeds its budget. Summaries are handoffs with goals, constraints,
decisions, completed actions and pending/unknown outcomes, never authority.
Automatic compression calls are marked agent-initiated for provider telemetry.

## Tests

```bash
cargo test -p cos agent::context:: -- --test-threads=1
cargo test -p cos agent::runtime::context:: -- --test-threads=1
```
