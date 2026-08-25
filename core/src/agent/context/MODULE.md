# Agent Context Module

## Purpose

`context/` controls conversation context size and extracts safe references from
model/user-visible text.

## Responsibilities

- Estimate context usage and compress older conversation safely.
- Preserve tool-call/result integrity across compression boundaries.
- Extract references/citations without executing or trusting them.
- Remove hidden thinking blocks where configured.

## Key Files

| Path | Role |
| --- | --- |
| `compressor.rs` | Token estimates, tail preservation, LLM summary |
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
