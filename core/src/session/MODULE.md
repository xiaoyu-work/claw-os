# Session Module

## Purpose

`session/` stores and validates Claw OS session identity, capability context,
and lifecycle state used across broker and agent operations.

## Responsibilities

- Create, load, update, and expire sessions.
- Bind role/scope/capability context to session identity.
- Record typed client source, locality, and attended state from trusted entry
  points.
- Preserve transaction and concurrency invariants.
- Provide session queries to CLI, agent, and broker consumers.

## Key Files

| Path | Role |
| --- | --- |
| `mod.rs` | Session API and lifecycle |
| `../../test/unit/session.rs` | Persistence, expiry, concurrency, and validation tests |

## Dependencies

Sessions consume capability types and persistence helpers. Broker/tool/app
consumers trust only validated session state, never caller-supplied authority.
Persistent format changes require migration and recovery coverage.

`SessionMeta::origin` is a typed provenance marker, not a role: it names the
trusted issuer that minted the session's capabilities so a consumer can tell an
ambient conversation apart from a snapshot of authority a user proved when they
created an unattended job. Only a daemon-side authority writes it, and a
consumer may act on a delegation variant only after `record_is_root_owned`
confirms the record could not have been authored by the account it would
delegate to. `None` always means "no delegation".

`SessionMeta::client` is separate interaction provenance. Daemon-created task
records snapshot source and locality into each job, while attended presence
uses a non-persistent pid/start-time lease. The lease is bound into one signed
worker grant, so queued, recovered, or post-restart work cannot inherit stale
attendance.

Generic detached child processes receive a derived `child-process` source with
`attended = false`; copying the parent's terminal presence is forbidden.

The same trusted origin selects consent context: ambient system-Agent tasks are
attended, while cron/trigger delegations are unattended and cannot open a new
approval prompt at execution time.

## Tests

```bash
cargo test -p cos session:: -- --test-threads=1
```
