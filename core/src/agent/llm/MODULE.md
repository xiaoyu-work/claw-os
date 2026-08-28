# LLM Module

## Purpose

`llm/` provides the provider-neutral request/response contract, provider
construction, streaming accumulation, fallback, credentials, and usage.

## Responsibilities

- Define messages, content blocks, tools, finish reasons, usage, and events.
- Resolve provider settings and infrastructure at the registry boundary.
- Normalize provider streams into complete history.
- Rotate credentials and classify failures.
- Run fallback/auxiliary provider chains through injected attempt observation.
- Record model usage without leaking credentials or prompt bodies.

## Key Files

| Path | Role |
| --- | --- |
| `types.rs` | Provider-neutral wire-independent types |
| `construction.rs` | Credential source/resolution and shared HTTP transport |
| `registry.rs` | Provider settings and typed construction |
| `attempt_observer.rs` | Request metadata and fallback observation |
| `providers/` | Provider authentication and wire formats over injected infrastructure |
| `accumulate.rs` | StreamEvent to ChatResponse/history |
| `provider_chain.rs` | Ordered provider fallback |
| `credential_pool.rs` | Key selection, health, cooldown |
| `error_classifier.rs` | Retry/fallback error classes |
| `sse.rs` | Bounded SSE parser |
| `usage.rs`, `run_log.rs` | Usage and call records |

## Dependencies

Provider modules translate only at the wire boundary. `registry` receives an
immutable `AgentConfig` snapshot plus `ProviderBuildContext`; credential-store
and environment precedence lives once in `construction`, and providers share
its HTTP connection pool. `ProviderChain` owns fallback state but receives a
`ProviderAttemptObserver`, so audit path and session metadata discovery remain
at the composition boundary. Streaming and non-streaming paths preserve
equivalent content, tools, opaque reasoning/tool state, usage, and errors.

## Tests

```bash
cargo test -p cos agent::llm:: -- --test-threads=1
```

Provider work should usually target its provider tests, accumulator tests,
setup tests, and provider-chain/pool tests.
