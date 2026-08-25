# AI Gate Module

## Purpose

`ai/` is the policy, consent, budget, and provider-facing gate used by apps and
system callers that request AI work outside the interactive agent loop.

## Responsibilities

- Authorize AI verbs/scopes and app identity.
- Apply consent and budget decisions.
- Build provider-neutral requests through the configured agent provider.
- Record usage and deny direct provider ownership by apps.

## Key Files

| Path | Role |
| --- | --- |
| `gate.rs` | AI request validation, authorization, provider call |
| `consent.rs` | Persisted consent decisions |
| `chat.rs` | AI chat request surface |

## Dependencies

The gate consumes capability/session identity and the LLM registry. Apps call
the gate through SDK/bridge surfaces; they never receive provider credentials.
All model-visible input and usage remain auditable.

## Tests

```bash
cargo test -p cos ai:: -- --test-threads=1
```
