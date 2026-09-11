# Agent Context Module

## Purpose

`context/` controls conversation context size and extracts safe references from
model/user-visible text.

## Responsibilities

- Bound initial profile/surface data without keyword-based task routing.
- Preserve trust labels and the policy/prelude/instruction channel split.
- Estimate context usage and prepare versioned, durable-capable compression.
- Deterministically prune oversized old tool results before spending a model
  call.
- Preserve tool-call/result integrity across compression boundaries.
- Keep at least one real user message in the protected verbatim tail.
- Extract references/citations without executing or trusting them.
- Remove hidden thinking blocks where configured.

## Key Files

| Path | Role |
| --- | --- |
| `packet.rs` | Label-preserving ContextPacket and ContextBuilder |
| `budget.rs` | Whole-request accounting and UTF-8-safe bounded excerpts |
| `compressor.rs` | Token estimates, deterministic pruning, protected-boundary planning, and LLM summary execution |
| `references.rs` | Reference extraction/normalization |
| `think_scrub.rs` | Hidden reasoning tag removal |

## Dependencies

Compression uses provider-neutral messages and a provider call, but it cannot
drop unresolved tool state or turn summaries into authority. Automatic context
calls are marked agent-initiated for provider telemetry.

`runtime/context.rs` admits pinned notes only under the trusted request's memory
read exposure. The packet contributes fenced prelude segments, not a string
appended to the owner's instruction. The main model chooses additional reads
and must refine insufficient or unreliable retrieval. Truncation retains the
source label, while required data that exceeds a fence/input limit fails
explicitly. Durable compaction and its source/origin tracking remain the
runtime's persistence authority.

## Tests

```bash
cargo test -p cos agent::context:: -- --test-threads=1
```
