# Agent Context Module

## Purpose

`context/` controls conversation context size and extracts safe references from
model/user-visible text.

## Responsibilities

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
| `compressor.rs` | Token estimates, deterministic pruning, protected-boundary planning, and LLM summary execution |
| `references.rs` | Reference extraction/normalization |
| `think_scrub.rs` | Hidden reasoning tag removal |

## Dependencies

Compression uses provider-neutral messages and a provider call, but it cannot
drop unresolved tool state or turn summaries into authority. Automatic context
calls are marked agent-initiated for provider telemetry.

## Tests

```bash
cargo test -p cos agent::context:: -- --test-threads=1
```
