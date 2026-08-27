# clawd Module

## Purpose

`clawd/` is the privileged system broker behind daemon-backed `cos` operations
and agent tasks.

## Responsibilities

- Accept authenticated Unix-socket RPC.
- Derive client/session identity and capability context.
- Dispatch privileged services and app/MCP session operations.
- Run agent task workers and expose task lifecycle RPC.
- Install audit hooks around broker-visible work.

## Key Files

| Path | Role |
| --- | --- |
| `server.rs` | Socket lifecycle, request routing, peer checks |
| `agent_client.rs` | Client RPC for agent task submit/result/cancel/status |
| `tasks.rs` | Task queue and lifecycle |
| `app_sessions.rs` | App/native/MCP session authority: derives identity and capabilities, plans approvals, issues launch handles |
| `scheduler.rs` | Proactive-scheduler authority: validates `cos cron` / `cos triggers` requests and derives what a job may carry |
| `system_caps.rs` | System capability derivation |
| Service modules | One privileged capability provider per domain |

## Dependencies

The broker consumes capability definitions and service providers. Callers use
RPC clients rather than importing server internals. Never trust request fields
for identity or authority; derive them from the connection/session boundary.

Nothing a caller sends is written to a durable record on trust.
`server.rs` projects every request through [`crate::audit_policy`] before
dispatch and hands the same projection to the broker audit log and the system
operations journal, so the two sinks cannot disagree. That policy is an
allowlist keyed by command: a route contributes only the fields it has
classified as safe, and a command with no entry is audited by outcome alone —
no name, no arguments. `USER_COMMANDS` and `ROOT_COMMANDS` in `server.rs` are
the canonical route list a unit test checks the policy table against, so a new
command cannot reach a sink unclassified. Handler messages are caller-derived
and are stored as a length plus a keyed digest; a route that wants its failure
named uses `BrokerError::classified` or `Response::error_classified`.

App and MCP session rows are root-owned authority that privileged providers
later trust. `app_sessions.rs` therefore mints them from the installed manifest
plus schema-validated arguments, bounded by the launcher authority resolved
from the peer's process ancestry. An unresolvable ancestry fails closed. A
launcher with no registered session receives an unprivileged, home-bounded
policy ceiling from `system_caps.rs`; anything above it needs an approved
permission grant, which only the privileged approval helper can create. That
grant is bound to an identity the daemon derives from the peer itself — the
authenticated parent session, or the peer's exact uid/pid/start-time — never to
a session string the request supplied and never to anything a sibling process
shares.

A launch is authorized as one plan: the complete canonical capability set is
derived first, every capability the launcher cannot delegate is collected, a
deduplicated pending request is filed for each, and their ids are returned as
non-secret metadata. The launcher process stays alive, polls `permission.status`
with a bounded timeout and cancellation, and retries over the same authenticated
connection. Grants are settled all-or-none under an approvals-store lock, so a
launch never burns part of a set, and every duration is retired on first use.
Nothing carries between processes: knowing a request id authorises nothing.

Mutating a registered session requires the opaque launch handle issued at
registration, bound to the launching process and single-use for the pid bind.
It never appears in any durable record.
Caller-supplied capabilities may only narrow the ceiling.

`scheduler.rs` applies the same rule to proactive jobs, whose stored capability
snapshot is root-owned authority the heartbeat later executes. The route
validates the subsystem, command, arguments and job/rule id first, then resolves
the caller from the peer and the routed registry — never from a terminal,
`NoNewPrivs`, or anything the request carries — and refuses App sessions
outright. Owner-scoped reads and retirements get only the capability their gate
requires. Creating, re-arming or running a job delegates authority beyond the
call, so it needs capabilities the peer holds or one-shot grants approved by the
privileged helper for that exact peer, verb and scope. What a job stores is
bounded by the same home-scoped ceiling its executor applies.

## Tests

```bash
cargo test -p cos clawd:: -- --test-threads=1
```

For a service change, include malformed input, exact scope, broker error, and
successful provider-path coverage.
