# LLM Providers Module

## Purpose

`providers/` authenticates already-resolved requests and translates between
provider-neutral agent types and provider-specific HTTP/event protocols.

## Responsibilities

- Serialize messages, images, tools, reasoning, and provider options.
- Parse non-streaming responses and bounded streaming events.
- Apply provider-specific authentication headers and error classification.
- Preserve tool IDs, opaque state, usage, and retry/fallback semantics.
- Consume resolved credentials and the shared transport supplied by `llm/`.
- Surface provider-construction and shared-state failures through
  `ProviderInfrastructureError`; never continue through poisoned credential,
  Copilot cache, or mock-provider state.

## Key Files

| Path | Role |
| --- | --- |
| `anthropic.rs` | Anthropic Messages |
| `gemini.rs` | Gemini generateContent |
| `bedrock.rs` | AWS Bedrock |
| `openai_compat.rs` | OpenAI-compatible Chat Completions provider |
| `../../../../test/unit/agent/llm/providers/openai_compat.rs` | Auth, routing, wire, streaming, pool regressions |
| `openai_chat.rs` | Chat Completions request/response/SSE adapter |
| `openai_responses.rs` | OpenAI Responses request/response/SSE adapter |
| `copilot_auth.rs` | GitHub OAuth, Copilot token exchange/model routing |
| `llama_local.rs` | Local llama runtime |
| `mock.rs` | Deterministic test provider |

## Dependencies

Providers implement `llm::Provider` and use `llm::types`. Context-aware
constructors never discover configuration, credential stores, process
environment, audit paths, or HTTP client policy. Legacy public constructors
remain as explicit compatibility boundaries, while `llm::registry` supplies provider-specific settings,
`llm::construction` supplies resolved credential ownership and a shared
transport, and provider-specific headers or state do not leak into unrelated
wire formats. Setup/model discovery changes stay consistent with runtime
routing.

Provider constructors used by production composition are fallible. Legacy
infallible constructors remain source-compatible and store deferred typed
initialization failures; `is_configured` returns false and requests return that
failure without creating a fallback HTTP client or inventing credentials.
`CredentialSource` and `resolve_aws_value` retain their original public
signatures, while `TypedCredentialSource` and `try_resolve_aws_value` drive
source-preserving production composition. A pool whose keys are cooling maps
to `RateLimited` so provider-chain fallback/retry semantics remain unchanged;
only poisoned/unavailable pool state maps to infrastructure failure.

## Tests

```bash
cargo test -p cos agent::llm::providers::<provider>::tests -- --test-threads=1
cargo test -p cos agent::llm::accumulate::tests -- --test-threads=1
cargo test -p cos agent::setup::tests -- --test-threads=1
```
