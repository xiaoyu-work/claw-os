# clawd Module

## Purpose

`clawd/` is the privileged system broker behind daemon-backed `cos` operations
and agent tasks.

## Responsibilities

- Accept authenticated Unix-socket RPC.
- Derive client/session identity and capability context.
- Dispatch privileged services and app/MCP session operations.
- Own task ownership/lease and expose task lifecycle RPC.
- Supervise unprivileged `claw-agentd` workers; never run the model/tool loop
  in this process (see `core/src/agentd/MODULE.md`).
- Install audit hooks around broker-visible work, including runtime audit
  forwarded by a worker.

## Key Files

| Path | Role |
| --- | --- |
| `server.rs` | Socket lifecycle and request admission order, agentd supervision start |
| `transport/` | Frame reader/writer, per-message peer credentials, admission ceilings |
| `wire/` | Versioned envelope, bounded field types, one typed request body per route |
| `routes.rs` | The route registry: wire name, typed decode, access class, budget, handler |
| `agent_client.rs` | Client RPC for agent task submit/result/cancel/status |
| `tasks.rs` | Task queue and lifecycle |
| `app_sessions.rs` | App/native/MCP session authority: derives identity and capabilities, plans approvals, issues launch handles |
| `scheduler.rs` | Proactive-scheduler authority: validates `cos cron` / `cos triggers` requests and derives what a job may carry |
| `system_caps.rs` | System capability derivation |
| `session_scope.rs` | Trusted-session override and its owner-policy clamp |
| Service modules | One privileged capability provider per domain |

## Wire Protocol

`/run/cos/clawd.sock` carries broker protocol v1: one length-prefixed frame per
message, one request per connection, then close. The header is `CBK1`, a kind
byte, a reserved flag byte that must be zero, and a big-endian `u32` length. The
length is checked against the direction's ceiling before a body buffer exists,
so a peer cannot make the daemon reserve memory it has not justified, and there
is no terminator to scan for — a short read is a truncation, not a partial
record the daemon waits on.

The body is a closed envelope: `deny_unknown_fields` over a version, a bounded
correlation id, the route name, and that route's parameters. There is no legacy
shape and no fallback parse. A frame that does not carry the magic is refused
with a named error; a peer that opens with the pre-v1 newline protocol receives
one newline-terminated JSON error so an out-of-date `cos` prints something
actionable, but nothing it sent is parsed, authorized or dispatched. The
correlation id is correlation only — it selects no uid, pid or session, and one
request per connection means responses cannot be crossed.

Identity is per message, not per connection. `SO_PASSCRED` is set on the
listener before the first `accept`, Linux copies the flag onto every accepted
socket, and the kernel stamps `struct ucred` onto each `sk_buff` at `sendmsg`
time from the sending task — including on connections still sitting in the
accept queue. Every segment of a frame must carry the same credentials, so a
descriptor handed to another process mid-request is a fault rather than an
identity change. `SO_PEERCRED` is used for exactly one thing: choosing which
accounting bucket a new connection counts against, before any message exists.
The credentials are then re-verified through `/proc`, and a peer whose real and
effective uid disagree with what the kernel stamped is refused.

`clawd` accepts no descriptor from any peer. Ancillary data is received
deliberately — never with a null `msg_control`, which would drop passed
descriptors into the daemon unnoticed — every `SCM_RIGHTS` descriptor is closed
with `MSG_CMSG_CLOEXEC` set, and the request is refused.

`routes.rs` is the only route surface. A row declares the wire name, the typed
`deny_unknown_fields` body, the access class, whether the route mutates, its
concurrency and time budget, and its handler; the `Command` enum, the table and
the name lookup are all generated from those rows, so a route cannot exist
without declaring every one of them, and an in-repo client cannot name a route
that does not exist. Unknown commands, undeclared fields, wrong types,
oversized strings and over-deep payloads all fail closed *before* the access
class is consulted. Mutating routes are never cancelled by the broker: dropping
one at an await point could leave a package half-installed, so they are bounded
by their own tool and lock timeouts plus a per-route in-flight ceiling.

Resource ceilings are fixed at startup and live in `transport/limits.rs`:
connections and in-flight requests, globally and per authenticated principal;
per-route concurrency from the route's own budget; a read deadline that bounds
slowloris; a write deadline; a response byte cap; and a fixed-capacity record of
recent mutations so a replayed frame cannot repeat a non-idempotent privileged
call. Root has a larger — but still finite — allowance, because `clawd`'s own
rollback and approval clients run as root and must not be starved by a user
flooding the socket.


## Dependencies

The broker consumes capability definitions and service providers. Callers use
RPC clients rather than importing server internals. Never trust request fields
for identity or authority; derive them from the connection/session boundary.

Nothing a caller sends is written to a durable record on trust.
`server.rs` projects every dispatched request through [`crate::audit_policy`]
before dispatch and hands the same projection to the broker audit log and the
system operations journal, so the two sinks cannot disagree. That policy is an
allowlist keyed by command: a route contributes only the fields it has
classified as safe, and a command with no entry is audited by outcome alone —
no name, no arguments. [`routes::ROUTES`](routes.rs) is the canonical route list
a unit test checks the policy table against, so a new command cannot reach a
sink unclassified. Handler messages are caller-derived and are stored as a
length plus a keyed digest; a route that wants its failure named uses
`BrokerError::classified` or `Response::error_classified`.

A request refused before dispatch is recorded differently and more narrowly: a
stable class from `wire::Fault`, the byte count the daemon had accepted, and —
only when a route was actually resolved — the registry's own `&'static str`
name. The frame itself is never stored, not verbatim and not as a digest, and
neither is its ancillary data or any `serde` message: a refused frame is
unparsed caller input that may be a credential or a fragment of another
protocol.

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

`system_caps.rs` owns the same rule for the system Agent. `BASELINE` records one
explicit decision per catalog verb, so a verb the catalog gains without a
decision is denied; catalog risk is one input, not the rule. The default set is
the owner's canonical passwd home plus that owner's daemon-side Agent state
root, its own memory and process-registry rows, read-only status of the owner's
own device, the owner-partitioned data stores, the model, and verbs that carry
no resource. Global filesystem access, arbitrary hosts and browser navigation,
process spawn/exec, credentials, system/package/service/identity/storage/mount/
power mutation, cron persistence, device control, agent spawn/delegate, and the
shared local IPC channels are denied. `sys.observe` is not ambient merely
because it is read-only: `OBSERVABLE_DEVICE_DOMAINS` is exhaustive, and the
window list, the account database, systemd units, firewall state and snapshot
inventory all need an exact approval. A resource-addressing verb never receives
an untyped `Scope::Wild`, and root-owned tasks get the same table bounded to
root's own home — euid, role name, prompt text, model output, terminal, and
socket group are never authority. Every owner root comes from
`verified_owner_home`, which is `paths::verified_home_for_uid`: canonical,
existing, owned by that uid, and with no fallback, so the home stamped at
creation and the ceiling applied at execution cannot disagree.

Everything above the baseline arrives one of two ways: an authenticated
task/session delegation, or an exact one-shot grant the user approved for that
session, verb, and scope. `caps::enforcement` files one pending request per
capability denial and spends it at the gate, so an approval covers the resource
that was refused and nothing adjacent, and is never written back into a
capability set.

`session_scope.rs` closes the loop and is where the two concepts stay apart. It
reads the session's typed `SessionMeta::origin`, believes a delegation marker
only when `session::record_is_root_owned` confirms `clawd` wrote the record, and
then clamps the stored set with the matching policy: the minimal baseline for an
ambient task, or the baseline plus the one executor verb that subsystem proved
at creation (`cron` → `proc.spawn`, `triggers` → `agent.spawn`) and credentials
named exactly. Delegated capabilities are re-admitted verbatim, so a glob
credential or a snapshot's unreviewed `sys.*`, `net.*`, `fs.*` authority grants
nothing, and an unattended job can never persist privileged system mutation.
Stored authority is re-derived rather than trusted, and the result can only
narrow.

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
cargo test -p cos --test clawd_broker_socket -- --test-threads=1
```

For a service change, include malformed input, exact scope, broker error, and
successful provider-path coverage.

For a transport or route change, `clawd_broker_socket` is the one that binds a
real listener and connects to it, so it is the only place the `accept`-time
inheritance of `SO_PASSCRED` and the pre-accept credential window are actually
proved. A new route must not be able to pass `clawd::routes` without a typed
body, an access class, a budget and an audit policy.
