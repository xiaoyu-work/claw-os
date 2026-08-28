# clawd Module

## Purpose

`clawd/` is the privileged system broker behind daemon-backed `cos` operations
and agent tasks.

## Responsibilities

- Accept authenticated Unix-socket RPC.
- Derive client/session identity and capability context.
- Dispatch privileged services and app/MCP session operations.
- Own task ownership/lease and expose task lifecycle RPC.
- Expose owner-scoped notification publication, subscription, state, and
  delivery-leasing RPC.
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
| `routes.rs` | The route registry: wire name, typed decode, access class, budget, audit fields, authorization descriptor, handler |
| `authority/` | The capability authority: grants, opaque handles, attenuation, the route middleware and its audit facts |
| `agent_client.rs` | Client RPC for agent task submit/result/cancel/status |
| `tasks.rs` | Task queue and lifecycle |
| `app_sessions.rs` | App/native/MCP session authority: derives identity and capabilities, plans approvals, issues launch grants |
| `scheduler.rs` | Proactive-scheduler authority: validates `cos cron` / `cos triggers` requests and derives what a job may carry |
| `notifications.rs` | Notification RPC handlers, due-nudge fanout, and external delivery dispatcher |
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
concurrency and time budget, safe audit fields, its authorization descriptor,
and its handler; the `Command` enum, the table and the name lookup are all
generated from those rows, so a route cannot exist without declaring every one
of them, and an in-repo client cannot name a route that does not exist. Unknown
commands, undeclared fields, wrong types, oversized strings and over-deep
payloads all fail closed *before* the access class is consulted. Unknown,
malformed, unauthorized, unavailable and handler execution failures have
separate stable response codes. Subsystems with authorization or
backend-availability decisions return typed `BrokerError`s at those decision
points, and an authority refusal is one of them; ordinary validation/provider
failures remain execution errors without classifying by message text. Mutating
routes are never cancelled by the broker: dropping one at an await point could
leave a package half-installed, so they are bounded by their own tool and lock
timeouts plus a per-route in-flight ceiling.
## Capability Authority

`authority/` holds the one thing that decides what a request may do. A **grant**
is the daemon's own record of authority it handed out, and it is never parsed
from a request: it binds an authenticated principal (uid, pid, that pid's start
time, and the cgroup `/proc` reports), a subject (session, App, task), an
audience, an exact `CapSet`, an issuer, issue and expiry instants, a remaining
use budget, revocation state, and lineage back to the parent it was attenuated
from.

A grant is referenced by an opaque handle: 32 bytes of kernel entropy, stored
only as its SHA-256, rendered as `<grant-handle>` under `Debug`/`Display`, and
implementing neither `Serialize` nor `Deserialize` so it cannot reach a log or a
journal payload by accident. **Possession is insufficient.** Every resolve
re-checks the principal against the credentials the kernel stamped on *that*
message, so a same-uid sibling, an fd recipient, a recycled pid or a process
that re-`exec`ed cannot exercise it. A session id is an index into the store,
not authority: naming somebody else's session finds their grant and then fails
the principal check, which is the same answer an unknown session gives.

Attenuation is the only way one grant derives from another and is monotonic in
every dimension: child caps ⊆ parent caps, audience ⊆ parent audience, expiry no
later, use budget no larger, owner unchanged, depth bounded, children bounded.
`Scope::Wild` cannot be introduced for a verb that addresses a real resource
namespace even when the parent holds it — only where the catalog says `Wild` is
the canonical scope. Revocation and expiry cascade to every descendant;
exhausting a use budget retires only the grant that was spent, because its
children were already clamped to it.

The store is in memory and dies with the process. That is the design: a `clawd`
that restarted can no longer prove the bindings it made, so every ephemeral
grant fails closed rather than surviving into a daemon that cannot re-verify it.
Work that must outlive a restart is a scheduled job, re-issued from root-owned
durable provenance through `session_scope.rs` and `system_caps.rs`, never from a
serialized handle. Grants are bounded globally, per owner, per session and per
process, and are swept on every entry point plus a periodic tick, so a process
that exits, a session that finishes, a task that is cancelled, a worker lease
that lapses and a deadline that passes all drop their rows.

`server.rs` calls `authority::authorize` after the typed decode and before
dispatch. It is not optional: every route declares an `RouteAuthority` — an
audience, where its subject comes from, a capability resolver over its validated
body, and whether a denial there may become a consent prompt. A route whose
descriptor says it derives its own exact capability must spend it through
`Decision::require_all` before it answers; if it did not, the response is
withheld, so "the provider forgot to check" fails closed instead of succeeding
silently.

Three subject kinds cover the surface. `Peer` routes act for the connecting
process and resolve no grant. `Session` routes are addressed by an App/MCP
session and run under the grant derived at bind. `Handle` routes are addressed
by the opaque handle itself. `PeerSession` is the seam for callers that
legitimately hold no standing grant — the rollback client finishing a mutation
it already recorded, an agent runtime refreshing a credential its session was
granted: the middleware authenticates the root-owned registry row from the
peer's process ancestry and start time, then mints a single-use, two-minute
grant so the capability spend, the audit trail and the obligation are identical
to every other route.

Whether a `PeerSession` route sees an App session's *transient* capabilities —
the ones `app_session.set_transient` grants for exactly one MCP tool call — is
declared per route rather than inferred. `credential.oauth-refresh` excludes
them, matching what the credential broker checked before the authority existed,
so a tool call granted a secret for one invocation cannot be turned into a token
refresh for a different one. The two rollback routes include them, matching what
`packages` and `systemd` checked. A unit test asserts both.

A successful spend returns an `Authorized` proof: `#[must_use]`, constructible
only inside the authority, and neither `Clone` nor `Copy`. The highest-risk
privileged mutations — package install and restore, `systemctl` actions and unit
restore, identity mutation, storage mount/unmount/eject, and config
apply/restore — take one by reference, so the type system sequences the side
effect after the authorization instead of trusting each call site to have done
the check *and* handled its `Err`. An empty capability set is refused rather
than treated as a vacuous success, and only a successful spend discharges the
route's obligation, so a provider that ignored a denial still has its answer
withheld.

Provider-side checks remain — a privileged mutation should be refused twice —
but they now run *through* the same decision instead of each re-reading the
process registry and re-deriving five checks of their own. Thirty hand-copied
policies were thirty places for one to drift.

Every issuance, attenuation, use, exhaustion, expiry and revocation is recorded
through typed facts in `authority/audit.rs`. A grant is named by a keyed,
non-reversible reference; capabilities are recorded as verb plus scope kind plus
a digest of the canonical scope, so `secret.read:openai/prod` is distinguishable
from `secret.read:openai/test` in the trail without either name being written
down. No handle, no scope value and no caller-authored string reaches a record.

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
system operations journal, so the two sinks cannot disagree. Each route carries
the allowlist of fields it has classified as safe; an empty list records the
registry-owned command name and outcome but no arguments. There is no second
command-keyed policy table. Handler messages are caller-derived and are stored
as a length plus a keyed digest; a route that wants its failure named uses
`BrokerError::classified` or a typed `BrokerError` constructor.

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

Mutating a registered session requires the opaque grant handle issued at
registration. It references a launch grant bound to the launching process, is
resolved by the route middleware against the credentials the kernel stamped on
that message, expires, and is revoked — together with the session grant derived
from it — when the session is deregistered. Binding is one-shot because the
authority refuses a second claim on a live session index, not because a boolean
was flipped. Nothing about the handle appears in any durable record.
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

An approval is a decision about one capability, not a standing licence.
`approvals.rs` stamps every approved record with a `GrantBinding`: a wall-clock
deadline, a use budget, a revocation generation and a keyed audit reference.
`Once` spends exactly one use; `session` and `forever` bound the same grant by
time and stay revocable, so "always" is a promise about not being re-prompted
during ordinary use rather than a promise that authority never expires. The
scan, the decrement and the retirement run under one store-wide lock, so two
callers cannot both spend the last use. A record written before the binding
existed — a real historical decision with no expiry, no budget and no
provenance — authorises nothing; it is evidence, and re-arming it would turn a
past "yes" into permission the user was never asked for.

Revocation is the generation counter in `approvals/generations.rs`, kept in
root-owned state *outside* the records. A binding captures the generation
current when it was approved; every load compares it against the generation
current now. Retiring authority is therefore an increment, which nothing a
record can say — including a copy restored from a backup taken before the
increment — can undo. Counters are per owner and per grant session, with an
owner-wide increment acting as a floor under every session it holds, so
"retire everything this account approved" is one atomic write rather than a
walk that could race a concurrent approval. Unreadable, unparseable or
group-writable state fails closed, and so does a binding with no generation at
all. `permission.revoke` is the root-only route that performs it, and session
finish, task cancellation and worker-lease teardown all call it for the session
they are tearing down.

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
cargo test -p cos clawd::authority -- --test-threads=1
cargo test -p cos --test clawd_broker_socket -- --test-threads=1
```

For a service change, include malformed input, exact scope, broker error, and
successful provider-path coverage.

For a transport or route change, `clawd_broker_socket` is the one that binds a
real listener and connects to it, so it is the only place the `accept`-time
inheritance of `SO_PASSCRED` and the pre-accept credential window are actually
proved. A new route must not be able to pass `clawd::routes` without a typed
body, an access class, a budget, an authorization descriptor and an audit
policy; `clawd::authority` asserts the descriptor and the route family agree.

For an authority change, cover the adversarial side explicitly: handle guessing
and theft, a same-uid sibling, pid reuse, the wrong audience or session,
expired/revoked/exhausted grants, parent revocation cascades, attenuation that
widens caps/audience/expiry/budget or introduces `Wild`, lineage depth and
count, concurrent double-use, all-or-none multi-capability spends, a forged
persisted approval record, and a fresh daemon holding nothing.
