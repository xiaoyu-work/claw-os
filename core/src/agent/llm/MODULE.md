# LLM Module

## Purpose

`llm/` provides the provider-neutral request/response contract, provider
construction, streaming accumulation, fallback, credentials, and usage.

## Responsibilities

- Define messages, content blocks, tools, finish reasons, usage, and events.
- Build providers from configuration.
- Normalize provider streams into complete history.
- Rotate credentials and classify failures.
- Run fallback/auxiliary provider chains.
- Record model usage without leaking credentials or prompt bodies.

## Key Files

| Path | Role |
| --- | --- |
| `types.rs` | Provider-neutral wire-independent types |
| `registry.rs` | Provider construction |
| `providers/` | Provider authentication and wire formats |
| `accumulate.rs` | StreamEvent to ChatResponse/history |
| `provider_chain.rs` | Ordered provider fallback |
| `credential_pool.rs` | Key selection, health, cooldown |
| `error_classifier.rs` | Retry/fallback error classes |
| `sse.rs` | Bounded SSE parser |
| `usage.rs`, `run_log.rs` | Usage and call records |

## Dependencies

Provider modules translate only at the wire boundary. Runtime and tools consume
the neutral types. Streaming and non-streaming paths preserve equivalent
content, tools, opaque reasoning/tool state, usage, and errors.

## Tests

```bash
cargo test -p cos agent::llm:: -- --test-threads=1
```

Provider work should usually target its provider tests, accumulator tests,
setup tests, and provider-chain/pool tests.
