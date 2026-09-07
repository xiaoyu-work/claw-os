# AI Gate Module

## Purpose

`ai/` is the policy, consent, budget, and provider-facing gate used by apps and
system callers that request AI work outside the interactive agent loop.

## Responsibilities

- Authorize AI verbs/scopes and app identity.
- Apply consent and budget decisions.
- Build provider-neutral requests through the configured agent provider.
- Record usage and deny direct provider ownership by apps.
- Serve SDK `cos ai chat` through the authenticated `clawd` `ai.chat` route.
  Workers receive neither the session registry, provider credentials nor network
  access. The gate uses the live exact App capability, owner configuration,
  verified manifest, consent, budget and safety policy; no local fallback exists.
- Persist safety-filtered provider requests in the owner's AI budget database
  (`ai_inputs`), correlated to the authenticated App session. This history is
  not App or agent memory. Cancellation retains the dispatched reservation's
  conservative estimate; completed calls settle actual usage.

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
