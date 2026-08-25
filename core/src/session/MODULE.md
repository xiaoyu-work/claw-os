# Session Module

## Purpose

`session/` stores and validates Claw OS session identity, capability context,
and lifecycle state used across broker and agent operations.

## Responsibilities

- Create, load, update, and expire sessions.
- Bind role/scope/capability context to session identity.
- Preserve transaction and concurrency invariants.
- Provide session queries to CLI, agent, and broker consumers.

## Key Files

| Path | Role |
| --- | --- |
| `mod.rs` | Session API and lifecycle |
| `tests.rs` | Persistence, expiry, concurrency, and validation tests |

## Dependencies

Sessions consume capability types and persistence helpers. Broker/tool/app
consumers trust only validated session state, never caller-supplied authority.
Persistent format changes require migration and recovery coverage.

## Tests

```bash
cargo test -p cos session:: -- --test-threads=1
```
