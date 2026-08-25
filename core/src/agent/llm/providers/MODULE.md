# LLM Providers Module

## Purpose

`providers/` authenticates to each backend and translates between
provider-neutral agent types and provider-specific HTTP/event protocols.

## Responsibilities

- Resolve credentials and provider endpoints.
- Serialize messages, images, tools, reasoning, and provider options.
- Parse non-streaming responses and bounded streaming events.
- Preserve tool IDs, opaque state, usage, and retry/fallback semantics.
- Keep provider-specific behavior out of runtime orchestration.

## Key Files

| Path | Role |
| --- | --- |
| `anthropic.rs` | Anthropic Messages |
| `gemini.rs` | Gemini generateContent |
| `bedrock.rs` | AWS Bedrock |
| `openai_compat.rs` | OpenAI-compatible Chat Completions provider |
| `openai_compat/tests.rs` | Auth, routing, wire, streaming, pool regressions |
| `openai_chat.rs` | Chat Completions request/response/SSE adapter |
| `openai_responses.rs` | OpenAI Responses request/response/SSE adapter |
| `copilot_auth.rs` | GitHub OAuth, Copilot token exchange/model routing |
| `llama_local.rs` | Local llama runtime |
| `mock.rs` | Deterministic test provider |

## Dependencies

Providers implement `llm::Provider` and use `llm::types`. They may share
bounded parsers and credential infrastructure, but provider-specific headers or
state do not leak into unrelated wire formats. Setup/model discovery changes
stay consistent with runtime routing.

## Tests

```bash
cargo test -p cos agent::llm::providers::<provider>::tests -- --test-threads=1
cargo test -p cos agent::llm::accumulate::tests -- --test-threads=1
cargo test -p cos agent::setup::tests -- --test-threads=1
```
