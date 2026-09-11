# clawd Module

## Purpose

`clawd/` is the privileged system broker behind daemon-backed `cos` operations
and agent tasks.

## Responsibilities

- Accept authenticated Unix-socket RPC.
- Derive client/session identity and capability context.
- Dispatch privileged services and app/MCP session operations.
- Reject App-origin App invocation and system Agent task orchestration, including
  App-owned agents and intermediate registered helper/Host sessions.
- Manage owner/App-scoped persistent MCP service Hosts and mint single-use,
  action-bound authorizations for calls relayed by authenticated task Hosts.
- Own task ownership/lease, durable approval waits, retry, and task
  lifecycle RPC.
- Expose owner-scoped approval views used by the Agent Web control center;
  permission decisions still cross the polkit helper.
- Expose owner-scoped notification publication, subscription, state, and
  delivery-leasing RPC.
- Supervise unprivileged `claw-agentd` workers and task-owned
  `claw-extension-host` processes; never run the model/tool loop or dynamic
  extension code in this process (see `core/src/agentd/MODULE.md`).
- Install audit hooks around broker-visible work, including runtime audit
  forwarded by a worker.

## Key Files

| Path | Role |
| --- | --- |
| `server.rs` | Socket lifecycle and request admission order, agentd supervision start |
| `transport/` | Frame reader/writer, per-message peer credentials, admission ceilings |
| `wire/` | Versioned envelope, bounded field types, one typed request body per route |
| `client.rs` | Broker client with correlation checks and pre-/post-dispatch transport classification |
| `routes.rs` | The route registry: wire name, typed decode, access class, budget, audit fields, authorization descriptor, handler |
| `authority/` | The capability authority: grants, opaque handles, attenuation, the route middleware and its audit facts |
| `agent_client.rs` | Client RPC for agent task submit/result/cancel/status |
| `ai.rs` | Authenticated App single-shot AI gate; owner configuration, current transient capabilities, bounded concurrency/deadline, no App-local provider execution |
| `tasks.rs` | Task queue, summary/list, cancel, retry, and session continuity |
| `usage.rs` | Peer-UID-scoped Agent token usage queries |
| `app_sessions.rs` | App/native/MCP session authority: derives identity and capabilities, plans approvals, issues launch grants, consumes service-bound call tickets |
| `app_permissions.rs` | Shared Settings permission service: verified declarations, owner/App deny gates, pending durable restoration, fine-grained revocation; no approval authority |
| `gui/`, `app_sessions/gui.rs` | Root-supervised GUI instances; operation-only needs, live parent/grant/policy checks, independent selection rights and checked retirement |
| `../display_session/` | Root PAM/login activation and compositor control; owner sockets or Wayland labels cannot register authority |
| `system_review.rs`, `system_review/presentation.rs` | Shared App/capability review projection, owner-scoped display revisions, root-only choices, fresh package verification and one-use App confirmation |
| `app_services.rs` | Persistent owner/App service manager, lifecycle policy, permission-policy snapshot retirement, capacity/restart control, and single-use call authorization |
| `../extension_host/broker.rs` | Purpose-bound private proxy: verifies SCM credentials, Host/child ancestry, route class, and nearest child session before normal dispatch |
| `scheduler.rs` | Proactive-scheduler authority: validates `cos cron` / `cos triggers` requests and derives what a job may carry |
| `notifications.rs` | Notification RPC handlers, due-nudge fanout, and external delivery dispatcher |
| `app_notifications.rs` | Closed native post/close and legacy-facade send/list, exact App/action/owner/source grants, and durable Notification Service publication/query |
| `browser.rs` | Attached-browser provider: exact action capabilities, expected-origin injection, owner socket validation, and bounded Native Messaging frames |
| `network_diagnostics.rs` | Host-network diagnostic provider: interface/route inspection, bounded DNS resolution, and DNS-pinned TCP probes for the `netdiag` App |
| `filesystem.rs` | Exact-scope bounded text reads and atomic writes/replacements for App workers; pinned paths, task-owned inverse snapshots, no App dispatch |
| `capture.rs` | App-bound non-interactive screenshot service; fixed native portal client, owner session, screen plus exact output grants, bounded PNG, pinned non-overwriting persistence |
| `media_player.rs`, `media_player/mpris.rs` | Fixed Media Player adapter; separate exact observation/control grants, authenticated owner bus and native executable/unique-name binding, fresh dispatch authorization and deadlines |
| `desktop.rs` | Owner desktop service; Files reveals only its fixed target; Terminal, Store and Settings open only their fixed binaries with an optional directory/package/page under their original independent process-spawn grants |
| `desktop/settings.rs` | Fixed Settings user-service activation; authenticated owner manager, closed GUI environment, independent lifetime and startup acknowledgement without weakening daemon/worker NoNewPrivileges |
| `client_identity.rs` | Peer/owner identity and synchronous thread-local filesystem credentials; trusted owner primary/supplementary groups, distinct from extension execution GID, with restoration on every exit |
| `users.rs` | User Manager provider: status requires `sys.observe:identities`; mutations require `sys.identity:manage`, with exact secret reads for passwords; OS-owned account state and rollback |
| `regional_settings/` | Closed locale/owner-language/static-hostname service; three separate exact grants, authenticated owner, root-owned D-Bus backends and confirmed/indeterminate outcomes |
| `system_caps.rs` | System capability derivation |
| `session_scope.rs` | Trusted-session override and its owner-policy clamp |
| Service modules | One privileged capability provider per domain |

The trusted Mail Native Messaging host derives its standing capabilities from
the same MCP-only manifest as Agent calls. Its compiled ceiling allows only
untrusted-content AI and `mail-ai`-scoped memory writes. Conditional,
argument-bound and mailbox/credential capabilities are refused, not inherited
when the product gains another tool. Native launch still requires the
root-owned launcher and Thunderbird parent; consent and AI accounting remain
in the broker gate.

Settings restores are non-execution receipts in the existing approvals store,
bound to the exact owner/App/capability and both App and owner/session
generations. Expired legacy Settings execution-shaped receipts retain policy
consent only; ordinary grant redemption refuses them. Decisions and revocations
are committed before success, and receipt/journal failures are explicit.
The ignored `clawd::desktop::settings::tests::settings_user_service_process_fixture`
requires root solely for owner-UID dropping; it exercises the installed
`systemd-run` against a private authenticated session-manager fixture and
harmless processes, never real Settings, polkit or user grants.

## Wire Protocol

`system.regional-settings.control` is available to authenticated sessions and
their existing private Host provider relay, never as a Host lifecycle action.
It requests only `sys.locale:system`, `sys.language:self`, or
`sys.hostname:static`, without App-name, publisher or Admin-bootstrap exceptions.
Inputs are closed and validated before authority/backend access. System locale
and the owner's AccountsService language are independent writes; setters
require fresh live capability spending and uncached matching readback.
See the [wire contract](../../../claw-os-sdk/wire/v1/regional-settings.md).

Run the focused normal-user suite from the repository root:

```bash
cargo test -p cos --lib -- regional_settings caps::catalog caps::role caps::verb --test-threads=1
```

The ignored
`clawd::regional_settings::dbus::tests::root_private_bus_exact_effects_refusals_and_ambiguous_results`
must be run explicitly as root against the built unit-test binary with
`--exact --ignored --test-threads=1`. It starts only a private `dbus-daemon`
and synthetic locale1/hostname1/AccountsService objects, with task-local
authority/audit data. It must never point at the live system bus.
Native Settings still requires declared/admitted GUI needs; these fixtures
do not accept its broad polkit payload or prove Wayland/GUI bootstrap.

`system.notification.control` has an exact App/action matrix:
`cosmic-notifications` post/close require `ui.notify` Wild; `notify` send
requires `ui.notify` Wild and list requires `data.inbox.read` Wild.
Neither App can use the other's actions, and read does not imply send.
It derives owner, source and session/task correlation from broker authority,
not MCP metadata or a display label.
Post validates bounded plain text, theme-only icon names, signed popup timeout
and flags before any effect; close accepts only a durable `notif-` string owned
by this source and owner. Numeric D-Bus IDs, absolute icon paths, arbitrary
sources/actions and service-state fallbacks are refused. Storage failures are
typed unavailable responses. Worker and App Host relays admit only this typed
service, never owner-wide notification administration or a session bus.

Notify send accepts 1..4000 plain-text characters and a strict urgency boolean.
Urgent is warning severity, not critical, and both modes use the normal
Immediate policy subject to DND/channel preferences. List accepts 1..100 and
projects only the owner's `app:notify` source (not native/task/other-App rows).
It returns durable IDs, full message, urgency, UTC creation timestamp without
a suffix, coarse `read` (state is not unread), explicit state and the complete
retained, unexpired source total. Reads do not reorder publication history.
No service intent reads or migrates legacy `notifications.json`.

`notifications_actual_native_worker_durable_delivery_and_owner_bound_ui` is an
ignored explicit-input fixture: build the native Notifications executable and
`notification-presentation-fixture`, the desktop bridge's
`notification-delivery-fixture`, and `cos`; supply the corresponding
`COS_NOTIFICATIONS_BINARY`, `COS_NOTIFICATIONS_PRESENTER`,
`COS_NOTIFICATIONS_DELIVERY`, `COS_NOTIFICATIONS_COS` and
`COS_NOTIFICATIONS_MANIFEST` paths. It exercises the real strict worker,
authenticated private relay, typed provider, SQLite, desktop consumer and
native presentation subscription against a private bus and task-local data.
No real desktop, remote delivery, user history or polkit is involved.

`notifications_signed_notify_worker_persists_and_shares_native_delivery` also
takes `COS_NOTIFY_PACKAGE`, the staged Python App directory. It signs and
verifies that complete payload, pins its entries and live package identity,
then runs the actual MCP/SDK/CLI through the strict worker/private broker.
It derives the separate grants from the signed manifest, refuses wrong grants
and forged fields, restarts both worker and private daemon, preserves both old
JSON namespaces without reading them, and verifies real native presentation,
acknowledgement and dismissal. It shares the native fixture's broker and
delivery consumer rather than implementing another notification provider.

`system.media-player.control` is restricted to `cosmic-player`. Status spends
`desktop.media.observe:cosmic-player`; the six playback actions spend only
`desktop.media.control:cosmic-player`. Both require explicit consent and
support the existing Settings deny gate. The fixed helper runs with the
owner's UID, no supplementary groups or GUI environment, inherited
NoNewPrivileges, a five-second ceiling and a root-parent socket. After
discovery it waits for a fresh broker grant check before dispatch. Only the
installed native executable's owner/PID-bound MPRIS endpoint is eligible;
missing, spoofed or multiple instances fail closed. No media is opened and
no other player is selected. Accepted playback actions cannot be rolled back
by a later cancellation.

Private-bus unit tests cover exact scopes, owner/executable identity, all
seven actions, live UI metadata, missing/ambiguous instances and a withdrawn
dispatch gate. The ignored
`media_player_actual_native_mcp_crosses_worker_relay` test takes explicitly
built `COS_MEDIA_PLAYER_BINARY`, `COS_MEDIA_PLAYER_MPRIS_FIXTURE`,
`COS_MEDIA_PLAYER_MANIFEST` and `COS_MEDIA_PLAYER_COS` inputs and exercises
the real strict worker, private relay, typed authority and native backend.
`core/test/support/media_player_helper.py` separately exercises the installed
executable and actual dropped-owner helper under a private mount namespace;
run its documented command as root. Neither fixture uses live user media or
the user's bus. These checks do not claim interactive Wayland acceptance.

System review records live under the existing root-owned approvals root.
`system.review.prepare` re-verifies the source using the authenticated owner's
filesystem identity and compiled trust roots; request JSON cannot choose an
owner or assert approval. Only the privileged OS helper may call
`system.review.decide`, and confirmation is consumed separately before App
publication. App confirmations carry no capability grants. Capability
selections are separate actions delegated to the existing approval authority;
App-policy restoration remains a non-execution receipt.
The core and shared clients require a root Unix peer for this route family,
so selecting a user-owned socket cannot forge an approved response.
The core client also requires UID 0 on the actual `app_session.register`
connection, including a Host's Root-owned private broker. Registration
refusals preserve their structured review details for the existing controller.

`system.review.pending` merges the owner's App reviews and existing capability
requests. Every route returns the shared `clawd-client::system_review` DTO.
The root helper submits `{owner_uid, review: ReviewDecision}`; `owner_uid`
comes from polkit, never the frontend's JSON. The closed decision is bounded
to 64 KiB, names the displayed revision, and has only an action plus explicit
supported permission selections. The owner cancellation route accepts
`{review: ReviewDecision}` with only `cancel` and no choices, without
elevation. Within `system.review.*`, legacy id/decision input remains limited
to immutable, non-grant App confirmation/cancellation; it cannot select
capability permissions.
The broker reconstructs the current projection and refuses stale revisions,
unknown/duplicate choices, unsupported policies and mixed confirmation/grant
actions before the underlying authority rechecks its own bounds.
Protected revision files carry no authority and do not replace owner
generation, package verification, atomic decision or consumption checks.

The older `permission.*` routes and capability-helper mode remain compatible
with existing clients. This typed terminal/native cutover does not establish
that every Settings or Agent Web approval caller has migrated to it.

App operation/GUI registration, native-host registration and prepared MCP
calls require an owner review before capability settlement. Unreviewed
`always-on` services are not started automatically. Service maintenance retires
stale review/permission-policy snapshots; this does not claim instantaneous
revocation of every direct resource or complete granular policy controls.

`/run/cos/clawd.sock` carries broker protocol v2 over the `CBK1` framing: one length-prefixed frame per
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

The human `app_service.cli_call` route uses bounded `McpArguments` for JSON
business content up to 1008 KiB, leaving envelope headroom inside the unchanged
1 MiB request frame. General metadata retains `Structured`'s 64 KiB string
limit; argument depth, node, array and object limits remain enforced.
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

## Error Boundaries

- `state::StateError` owns transaction recovery, in-memory context/transaction
  locks, ownership conflicts, and corrupted daemon state. Poisoned locks are
  unavailable state and are never recovered with `PoisonError::into_inner`.
  Session decode/corruption and invalid persisted timestamps remain `Corrupt`;
  missing sessions, held leases, and I/O/lock failures retain their distinct
  not-found, conflict, and unavailable categories with original sources.
- `server::DaemonError` owns socket setup, daemon initialization, and state
  recovery. Socket parent creation, stale removal, credential passing, bind,
  chmod, and accept preserve their `io::Error` sources and operation names.
  The binary reports runtime/server initialization failures instead of
  panicking.
- Context and transaction handlers preserve `StateError` until the route
  boundary. `protocol::BrokerError` translates it once: corruption and
  unavailable state use #39's stable `unavailable` wire code; authorization and
  ordinary execution retain their existing codes and messages.
- Leaf device handlers still return `String` behind `routes.rs`; the registry
  converts those once to `BrokerError`. Their remaining `unwrap`/`expect` calls
  consume option fields immediately after the same handler's action validator
  accepted the required combination. They are local decoder invariants, not
  I/O, initialization, or shared-state failure handling.

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

Agent-facing App calls arrive only through `app_service.call` on an
authenticated task Host's private broker. The request cannot assert its Host,
lease, owner, or task identity: the private broker supplies those facts after
SCM credential, pid/start-time, purpose, and lease verification. The daemon
then re-verifies the signed package and caller authority, resolves typed
arguments and target capabilities, and hands `app_services.rs` a closed
authorization plan. That canonical argument map remains authoritative inside
the service Host; it is schema-validated there without following filesystem
paths a second time. Filesystem calls also carry a daemon-captured mount
source/target/mode/class and device/inode snapshot, which the worker must match
before launch.

Authenticated local **CLI** App calls arrive through a separate
`app_service.cli_call` route (`Access::User`). It is the human `cos app <id>
<command>` counterpart to `app_service.call` and never overlaps it: the request
body carries only the exact App id, tool, and arguments. The daemon derives the
caller principal (`McpPrincipalKind::Cli`), call context, capability ceiling,
verified package, owner uid, and deadline from the peer's `ClientIdentity`, its
process ancestry / registered launcher session, and the installed manifest —
never from the request, and never accepting a caller-supplied identity, call
context, audit binding, capabilities, package identity, owner uid, or deadline.
An Extension Host (task or App-service) can neither reach this route nor forge a
CLI identity through it, and the route can never mint a private-task-host
principal. A CLI principal may address a service even when
`mcp.access.system_agent` is false, but it must still hold exact
`agent.invoke:<app>/<tool>` authority; the target `needs[]` are independently
derived, ceiling-clamped, approved, single-call bound, and audited, and App
code receives only the target capabilities, never the caller's invoke
authority. Both routes converge on the same `AppServiceManager`, so lifecycle,
restart, capacity, ticketing, and the launch gate are shared and there is no
alternate App process path, compatibility fallback, raw MCP invocation, or
local execution. This CLI path is selected only for MCP-only Apps (empty
`operations` plus an `mcp` service); Apps that still declare operations keep the
legacy `run(command, args)` dispatch until they migrate.

The manifest-defined `launcher` App-service sandbox receives no Wayland socket
or session bus and must not spawn desktop binaries. Provenance-classified
native desktop services remain the sole explicit transport-bearing exception.
The `launcher` App validates an exact AppID, bounded non-file URIs, and
canonical local paths, requiring exact
`fs.read` authority before converting local files to `file://` URIs and calling
the typed `system.desktop.control` `launch` provider. The provider independently
requires the `launcher` App identity, exact `desktop.launch:name:<app-id>`
authority, and exact `fs.read:path:<path>` authority for every local file URI,
then reconstructs the owner's desktop environment and invokes the
image-provided `gtk4-launch` outside the sandbox without accepting an
executable path from the App. The other desktop actions remain restricted to
`desktop-manager`.

The service manager keys instances by owner uid and App id, not by task, so a
`lazy` or `always-on` MCP service can survive the task that first used it.
Execution crosses a second private broker into an App-service-purpose Host.
The daemon's random call token expires at the caller deadline, is single-use,
and is bound to the
exact service Host instance, package, generation, caller context, target
capabilities, authorized mount snapshot, and executable action digest.
`app_session.set_transient`
atomically consumes it; a mismatch burns the token. No public route, business
argument, environment value, or App process can mint or widen one. Reusable
children complete their zero-capability handshake before token issuance;
single-call sandboxes initially execute only the trusted `claw-app-runner`,
blocked on a private stdin gate. After binding that process, the service Host
atomically consumes the token and installs the grant before releasing the
runner to `exec` package code and begin its handshake. The gate provides
ordering only; it cannot replace or widen daemon authorization.

The installed package's provenance ceiling is applied here too, and here is
where it is authoritative. `app_sessions.rs` resolves it from its own verified
package — never from the launcher's report — and clamps the fully resolved plan
before `authorize_plan`, before the session row is written and before any grant
is minted, so a developer-trusted package cannot reach a forbidden capability
through a manifest need, a `wild` scope binding, an approval, a wider parent, a
GUI launch or a session-tool re-scope. Audiences are filtered the same way:
developer content receives `AppLaunch` alone, is issued no relay grant, and its
session grant addresses no provider route. `register` returns the set the daemon
actually granted; the launcher adopts it for the sandbox policy and refuses to
launch if it is wider than the ceiling it computed itself.

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
