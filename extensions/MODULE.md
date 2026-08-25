# Extensions Module

## Purpose

`extensions/` contains optional integrations that add browser/mail capabilities
without expanding the privileged core.

## Responsibilities

- Package optional integration logic at a clear external boundary.
- Communicate through stable SDK, CLI, or protocol surfaces.
- Keep credentials and provider policy owned by core services.

## Key Files

| Path | Role |
| --- | --- |
| `claw-agent-browser/` | Browser/agent extension integration |
| `claw-mail-ai/` | Mail AI host/extension integration |

## Dependencies

Extensions depend on public contracts and cannot bypass capabilities, consent,
budgets, or audit. Changes coordinate with the matching core/app host surface.

## Tests

Use each extension manifest/package test command and the affected core/app
integration tests.
