# Authenticated GUI Host

`gui/` owns the Root-supervised lifetime of one verified App GUI. It does not
introduce a permission store or treat a native executable, publisher, App ID,
desktop label or Panel presentation as authority.

## Contracts

Registration uses the existing App session authority. Optional `desktop.needs`
selects only the default GUI's own needs; an explicit empty array requests none.
Without that field, the constrained union of fixed/wild **operation** needs
retains its legacy semantics. A named `desktop.entries.<name>` always declares
its own needs, never a union with operations, MCP or another GUI. Fixed needs
outside launcher delegation use the ordinary
exact-owner approval flow before registration; a signature or GUI declaration
grants nothing. Wild needs still inherit only the launcher's bounded scopes.
Argument-bound GUI needs are unsupported/unbound. Explicit conditional GUI
needs are also unbound, not unconditional grants; the legacy operation union
still does not evaluate `when`. Neither binds a later file-picker selection.

`desktop.optional_needs` and each `desktop.entries.<name>.optional_needs` are
independent best-effort feature lists, empty when absent. Missing, denied,
ask-unapproved, unbound, package/policy-refused or unsupported optional rights
do not block startup and never create an approval request. Mandatory `needs`
remain strict, including required `ui.panel`; optional declarations do not
subtract from them or from the default GUI's legacy operation union.

Root first settles the complete mandatory plan, then admits optional caps only
from the authenticated parent or a still-live, exact owner/session/scope
reusable approval. Reusable admission spends one bounded use, never a once
grant or a restoration receipt, and retains its expiry and revocation checks.
Optional Wild requests retain the existing bounded parent-only inheritance;
they never scan unrelated approved scopes or other GUI/MCP declarations.
Activity confirmations, Workspace limits, App denials, package ceilings and
the normal protected-resource/mount checks still apply. A disallowed optional
mount is omitted, not made available by weakening the resource gate.

Registration returns `optional_needs_ungranted`, zero-based indexes in the
selected surface's optional list. Root projects the same startup snapshot as
`COS_APP_OPTIONAL_NEEDS_UNGRANTED_JSON` when the selected list is nonempty.
An index can also mean only part of an inherited bounded set was available.
This presentation data is not authority or feature readiness: live
policy/provider checks still return ordinary denial errors, and AI consent,
budgets and provider availability remain separate. Existing sessions never
widen after a later approval. Expiry/revocation still uses checked instance
retirement. The bounded selected-file-read path below issues separate
operation-bound authority, never a dynamic widening of this startup snapshot.

The default GUI uses `desktop.entry`, falling back to top-level `entry` and the
runtime default. Neither override changes ordinary operations or `mcp.entry`.
Additional entries require a package-relative `entry`, literal `exec`, localized
`name` and explicit `needs`. Names match `[a-z][a-z0-9_-]*`, at most 64 ASCII
bytes; there are at most 64 additional entries. Only
`cos app <id> --gui:<name> [literal args...]` selects one. Default `--gui` and the
exact existing `desktop.exec` remain supported. No user-supplied path or
executable is interpreted.
Default and named launchers without `COS_SESSION` use the existing authenticated Root
registration directly, not an Admin/bootstrap parent. Root independently
retains their actual registered ancestry or bounded unregistered-launcher
policy; unsetting an environment variable cannot erase App ownership.

Root retains the selected name, package snapshot, entry and selector before
authorization and checks the same binding at launch. Every selected target
uses the generic signed regular-file/executable admission and descriptor-pinned
sandbox path. The launch request cannot change its GUI/MCP/entry/runtime.
All surfaces keep the same App/data/audit identity and per-App mutation fence.
The protected review lists every target and its needs/purposes, including
unbound requests. New fields enter its comparison digest, not approval authority.

For Binary, Node and Shell runtimes, [argument preparation](../../bridge/gui_args.rs)
appends `[selected.exec, ...user_args]` after any interpreter/script arguments.
The selector must equal the selected verified declaration's `exec`; it is not
rewritten, split as a shell command or required to name an operation key.
Python loads the selected signed file through its existing `run(selector, args)`
wrapper instead of receiving native argv.
The environment contains `COS_APP_GUI=1`, `COS_COMMAND=selected.exec` and
`COS_ARGS_JSON` encoding **only** the user arguments. Empty user arguments,
spaces, quotes and metacharacters remain literal.

Selectors are limited to 1024 UTF-8 bytes, must not be whitespace-only and
cannot contain control characters. There are at most 128 user arguments, each
at most 4096 bytes under the broker's bounded Text rules; their encoded JSON
must fit 64 KiB. The launcher checks these bounds before session authorization.
Root validates the selector during registration and independently checks user
argument bounds when decoding the launch request, before authorization.

Locale categories and `LANGUAGE` remain presentation. GUI execution does not
retain the tool worker's forced `LC_ALL` when the launcher left it unset;
stdio workers keep their existing deterministic environment.
The existing Iced renderer preference `ICED_BACKEND` is also closed presentation:
only `wgpu` and `tiny-skia` are accepted from the launcher and independently
decoded by Root. Unset retains the product's normal renderer selection.
This hint neither grants a renderer device nor changes filesystem, display,
content, Panel or other authority; other toolkits may ignore it.

Root exposes the verified package's read-only `resources/` as
`COS_APP_RESOURCE_DIR`. Its `usr/share` precedes standard system data roots in
`XDG_DATA_DIRS`, so toolkit schema/icon lookup does not depend on global App
resource activation. `COS_OWNER_HOME` supplies the authenticated owner path
for UI choices; `HOME` and App state remain private. Neither environment path
grants filesystem access, defeats protected-store checks or supplies a user bus.

### Filtered Home metadata

A signed surface's explicit unconditional mandatory `fs.meta` need for the
authenticated owner's `Home/**` or one literal `Home/subdirectory/**` may select
the read-only filtered native view. The exact mandatory capability, not a broader
Home grant or optional declaration, binds that one directory.
Legacy operation unions, optional needs, App identity and broad parent rights
do not prove this window's browsing consent. Before changing Reserved to
Starting, `supervision::admit_home_metadata` uses the ordinary protected
exact-owner approvals flow under this unique GUI session and a digest of its
immutable window/surface/display/launcher/Home binding. The CLI waits on the
existing approval-required response and retries this same registration.
The manager preflight call is required before Starting; the supervisor's
second admission check is fail-closed and cannot replace that retry point.

The supervisor spends that decision once and retains its exact authorization,
expiry and revocation generation. The original GUI capability remains in the
live grant; only raw Home bind derivation is suppressed. The filtered mount
precedes the nested private App-data bind. HOME and App state remain private,
and independent/broad Home content mounts still refuse rather than exposing a
raw directory or inventing a selected-content grant.

A protected outer setup gate lets Root bind the exact normal-UID runner and
checked instance cgroup before bubblewrap can inspect the FUSE source.
The source pins retain both authenticated Home and the exact declared directory.
Subroots must exist, be owner-owned directories and resolve without symlinks,
protected roots or aliases. Distinct overlapping or multiple mandatory metadata
roots refuse rather than widening the one-view node API. Inode 1 is precisely
the declared directory; Root-held node/candidate names remain relative to
authenticated Home, not relabelled relative to the subroot. Uncontrolled
Home-child metadata still refuses before raw mount derivation, including absent
paths. A separate `ui.panel` declaration changes no filesystem eligibility.
Releasing setup does not release App code: the existing inner App gate still
waits for transport, live grant and creator installation. Every metadata lease
renewal follows a fresh registration snapshot and exact window-consent check;
the bounded lease cannot outlive its original approval. No production content
watch API or mutation fault injector is promoted with metadata support. The
filtered proxy mount is write-capable only so later exact write selection can
operate; its default file permissions and callbacks still deny writes.

### Draft selected regular-file read

`selected_read.rs` admits only a proposed virtual inode from this window's
controlled metadata view. Typed request/status bodies carry no App-selected
owner, source path, window, lease or approval flag. Root authenticates the
live process/start and exact instance cgroup, then pins the source's raw bytes,
device/inode and revision and checks this signed surface's own read declaration.
Other GUI/MCP needs and parent `fs.read` are never selection. Unsupported
argument-bound needs and missing Wild inheritance remain unbound.

Selected-file declaration checks freshly verify the exact package retained by
registration, rather than rediscovering the whole installed App catalogue.
The complete signature, file contents, permissions and inventory are still
checked on every use. The retained directory identity and running package
reference must also match; descriptor pinning is not treated as immutability.
Only that freshly matched package can validate the retained manifest's needs.

Each proposal creates an ordinary protected exact-owner review naming file,
read-only access and window, with a digest binding that target, identity,
revision and live GUI. Only a fresh matching decision can be spent once.
Root issues a separate finite `Issuer::Approval` read grant, preserving ordinary
package/denial, Activity and Workspace checks, without changing parent/global
or immutable GUI startup caps. The Root-held grant is `GuiResource`-only and
unindexed; no handle reaches the App. Its effective native lease is one pinned
regular file even when the declared technical upper bound is Home-recursive.
Non-UTF8 names use opaque node identity and escaped inert review display, not
lossy paths.

`pending`/accepted is not `selected`. The fixed read deadline is clipped by its
own consent, metadata and GUI ceilings, including any shorter ordinary grant.
Status polling and metadata refresh never renew or extend that deadline.
Live authority/source checks govern native opens/reads; independent expiry and
ordinary owner/session revocation also trigger checked whole-instance
retirement for retained FDs/mappings and separate-process-group descendants.
Changed sources are stale, not silently rebound. Failed cleanup retains custody.
Checked window retirement also closes pending private read reviews and retires
unused approved decisions, preserving the original owner decision as evidence.

The controlled Home path suppresses all beneath-Home filesystem capabilities
from raw bind derivation, not from capability authority. Optional registration
resource validation uses that same projection only for an explicitly mandatory
Home or Home-subroot metadata surface; it neither grants metadata nor replaces
later protected window admission. Unrelated paths retain ordinary mount/protected-resource
validation. Raw Home refusal is unchanged. The controlled broker also refuses
legacy scalar filesystem read/write/replace so those routes cannot turn a broad
startup cap into selected content. Watch and general mutations remain closed.
Unsupported mandatory Home delete/watch/exec still refuses launch rather
than silently downgrading a mandatory contract to a readonly view.

Selected-file CLI primitives use only the fixed private GUI provider transport.
The mandatory Root connect mediator authenticates the actual socket peer as
this retained Root controller in the host namespace before admitting that hop.
The App's user/PID namespaces can translate host Root to an unmapped UID/PID,
so those translated values are not accepted as approval authority. The narrow
client helper cannot carry lifecycle, installation or review commands; their
ordinary Root-peer checks remain unchanged. Socket names/environment select no
authority, and every resource request still proves its live instance/cgroup.

### Draft selected write and reserved save

The same coordinator independently reviews `fs.write` for an existing regular
file. Mandatory/optional declarations remain eligibility, not selection, and a
read selection cannot become write or vice versa. A protected write decision
can authorize bounded native writes and a Root-reserved replacement of that one
target; it creates no writable siblings, directory, delete or exec authority.
`O_RDWR` requires both independently approved resources.

Root creates one capability-empty canonical mutation evidence session for this
GUI only when a write or creation decision is installed. It uses existing inverse bytes and
MAC-chained `MutationBracket` records, linking each production effect to the
ordinary selected-write grant reference. Effects wait for durable starts; file
and required parent sync plus committed completion precede successful replies.
Unknown outcomes fence and retire the instance while retaining unresolved
canonical evidence. Test fault injectors and direct prototype selectors stay
test-only.

Atomic save gives the App one opaque private scratch name under the pinned
target parent. Only that name can be created and only that existing target can
be replaced. Discarding private scratch is allowed, unlink/rmdir of owner data
and general rename are not. Non-UTF8 inverse targets fail before effects rather
than converting identity lossily. This existing-file path cannot create a new
Save As target.

The separate draft creation path accepts only one materialized directory node
(including the view root) and a bounded literal UTF-8 leaf. Its independent
protected owner/window `fs.write` decision binds the absent child and pinned
parent. A selected or reserved status is not a created file. Root supplies one
private scratch and one finalization attempt, records the genuine Missing
inverse before staging, forces atomic `RENAME_NOREPLACE`, syncs the file and
parent and commits canonical completion before reporting `created`.
An existing or racing target is never overwritten or adopted. Creation grants
neither content read nor subsequent overwrite; unknown effects retain custody
and fence the instance. See the [native creation contract](../../worker/native_fs/MODULE.md#one-absent-regular-child--save-as).
These draft interfaces do not complete installed Editor/Files UI acceptance or
publish a new SDK/platform.

The separate candidate acceptance selector
`clawd::server::gui_fixture::native_products::private_native_editor` requires
the real immutable App-owned Editor release ELF, original installer resources
and licenses, prepared against the published platform pin. It uses the unchanged
manifest, private fixture signing/activation, actual normal-owner PAM login and
common GUI Host. A protected compositor witness must observe a configured,
buffered window while the startup file's independent Root read review is still
pending; only a separate owner decision may produce the actual loaded-tab title
and another rendered frame. Read revocation must acknowledge actual process,
window, cgroup, native provider and runtime retirement. Neither a title alone,
SDK status nor a probe marker satisfies readiness.

This is a pending clean-GHA proof, not passing product acceptance or publication.
Exact source `2f33a5e1a` passed all ten Test jobs, including the actual signed
Editor from immutable App source `cc75d4f1` and the public platform 0.2.1.
The hardware-free CI explicitly selects the real binary's already compiled
`tiny-skia` backend through ordinary presentation. It verifies authenticated
configure, nonuniform 800-by-600 SHM pixels, an independently reviewed startup
file load and checked process/provider/window/PAM retirement. It neither replaces
the ELF nor manufactures pixels. Actual Save As, other native products, the
normal WGPU path and physical-display acceptance remain separate.
On a rendering deadline failure, the fixture records the original witness,
then uses existing checked retirement before the ordinary owner CLI receives
its normal bounded stdout/stderr outcome. Only then may a JSON-escaped 16-KiB
launcher-log tail enter the failure diagnostic. The original 25-second readiness
deadlines, real-buffer predicate and consent order remain unchanged; cleanup
never turns the failed gate into success. Renderer/protocol problems require
separate evidence, not bus/device/Home binds or forced fake parents. Save As
is not exercised without an existing trusted UI/dialog control.

Only verified committed own effects can advance matching selected resources.
They preserve existing read consent and all original deadlines. In-place
changes retain same-object semantics. Atomic replacement uses a new native
inode: new opens read the new object, old read FDs/mappings retain the old object,
and detached old write FDs refuse further effects. Retained scratch reads its
own authored bytes, never later target content. Pending proposals do not follow
own revisions; external changes/conflicts still fail closed.

The Root [display registry](../../display_session/MODULE.md) supplies the
authenticated owner and display epoch. The Host creates the instance ID, holds
the actual launch-gated child pidfd and mandatory cgroup, and installs one
creator lease before releasing App execution. The authority alias/revision,
expiry and rights come from the bound live grant, not App JSON or Wayland
security-context strings.

`clipboard.read:selection` and `clipboard.write:selection` are independent
checks. History, display and Panel metadata imply neither. Zero GUI needs
therefore give zero direct selection rights. Layer-shell is a separate lease
field derived only from `ui.panel:Wild` in that same bound live grant.
`ui.panel` requires explicit authorized delegation or exact protected owner
approval; ordinary `ui.window`, native origin, signatures and Panel labels do
not imply it. The system Agent and unregistered-launcher baselines deny it,
and developer/unsigned content cannot hold it. All signed Apps use this same
gate. The launch reply and bound audit event retain the actual installed right.
Its Layer-shell surfaces may request their own overlay keyboard focus, not
global `ui.input`, virtual keyboards, clipboard selection/history, capture or
App orchestration. Unrelated compositor protocol denials remain unchanged.

Every renewal rechecks package/instance validity, process identity, the existing
App grant and App deny gate, the inherited portion of authenticated parent
delegation and its existing approval revocation generation. Exact owner-approved
needs are not silently reclassified as inherited parent rights. Authority changes retire the instance rather
than widening a cached lease. The existing App-session grant expiry still
applies; no permanent GUI grant or Admin bootstrap is created.

## Code and teardown

| File | Responsibility |
| --- | --- |
| `inputs.rs` | Closed launch/wait/stop and selected-file shapes, bounded node/status references and presentation-only keys |
| `manager.rs` | Reservation, exact launcher ownership, results and checked retirement barriers |
| `supervision.rs` | Sandbox/cgroup preparation, binding, creator installation, refresh and cleanup |
| `resources.rs` | Exact owner/window Home metadata consent, filtered mount, bounded lease and checked provider retirement |
| `selected_read.rs` | Exact file/window proposals and protected decisions, independent read/write/create authority and checked source/lease custody |
| `broker.rs` | Instance-local authenticated provider relay into ordinary broker authorization |
| `transport.rs` | Kernel syscall notification handling and the fixed connection targets |
| `proxy.rs` | Per-segment kernel writer/cgroup checks, bounded bytes/connections/descriptors |
| `output.rs` | Bounded output draining after actual process retirement |
| `retirement.rs` | Absolute-deadline blocking barriers and exact approval owner/session matching |

The socket proxy drains each direction independently. `POLLHUP` is not EOF:
only a zero-byte receive proves that direction has drained. Partial writes
and downstream backpressure retain pending bytes and `SCM_RIGHTS`; the
destination's write half closes only after draining, leaving reverse traffic
active. Real connection errors and checked retirement still end the relay.

`retire_app`, `retire_session`, `retire_owner` and `retire_epoch` return a
process-local Root broker completion result. They retire listeners/clients and confirm process/cgroup,
provider, proxy, transport and output cleanup. The App permission and Root
approval-revocation controllers await these barriers before returning success.
A session barrier also matches the Root-derived launcher grant session.
Internal grant cleanup must not recursively wait on the same job.

Filtered Home teardown also checks instance death before dropping Root source
pins, exact unmount and provider joins. Failed metadata or checked-cgroup
preparation remains retained and cannot acknowledge GUI retirement.
Successfully retired metadata custody is consumed before removing the cgroup;
supervisor drop must not reopen that already removed containment.

The approval `None` owner is the distinct system bucket, not every user or
UID 0. GUI instances are owner-bound, so only `Some(uid)` can match them.
Session approval-cache revocation also checks that exact owner, not just the
session string. Once an App denial is durable, a failed audit write does not
skip resource retirement; audit and incomplete-teardown errors are both
reported without rolling back the denial.

The asynchronous controller captures an absolute deadline before queuing
blocking retirement. Queue time counts toward it. A timeout is an incomplete
acknowledgement, not cancellation of the cleanup retaining custody.
Retries still enter the checked barrier when the App is already denied or
there are no remaining grants to withdraw; neither condition proves retirement.

These functions are not a standalone installer RPC. `MANAGER` is process-local,
and its absence in a standalone `cos` process says nothing about instances held
by the Root broker. Root Store commits now require this actual manager and
retire the App across all owners under the shared per-App admission fence,
alongside checked ordinary and MCP custody. Signed directory installation and
the public provenance App rollback frontend reuse that fence and retirement
through the Root directory controller, not this process-local manager.
Trust-revoke completion and independent OS-package mutation still need their
own Root coordination. Directory delivery refuses dpkg-owned replacement rather
than inventing an ownership transfer. Provenance has no `clawd` dependency.

`Scope::kill` and
`LaunchResources::kill_all` remain best-effort APIs for their other callers;
they are not evidence of GUI retirement. Failed barriers remain explicit
pending/error outcomes and can be retried.

See [GUI transport](../../worker/gui_transport/MODULE.md) for syscall and raw
socket containment. Named private IPC, unsupported descriptor kinds, X11,
Clipboard history/CopyQ, legacy ordinary-display providers and producer
permission acquisition are not silently made available by this Host.

## Focused checks

From the OS repository root on Linux:

```bash
cargo test -p cos --lib -- --test-threads=1 \
  caps::manifest apps::permission_review clawd::app_sessions \
  clawd::system_review::presentation clawd::wire::requests \
  bridge::gui_args clawd::gui::inputs clawd::gui::output clawd::gui::proxy \
  clawd::gui::retirement clawd::app_permissions clawd::authority::store::tests::approval \
  worker::gui_transport \
  worker::seccomp::gui worker::net_broker display_session::registry::tests::spontaneous
cargo clippy -p cos --lib --bin claw-gui-runner \
  --bin claw-display-host --bin claw-display-session -- -D warnings
```

The proxy regressions execute the production relay with real pidfd/cgroup
bindings, not an authorization bypass. They require a non-Root test process
in a protected non-root cgroup with unset kernel login identity, and cover
96-KiB producer-close draining, backpressured descriptor forwarding, both
half-close directions, credential/descriptor refusals and checked stop.

The actual authenticated PAM/App/compositor fixture and installation inputs are
described in the [display guide](../../display_session/MODULE.md).
Its panel cases create actual overlay surfaces through the bound Root lease,
exercise independent read/write selection rights and checked revocation/expiry,
and refuse Layer-shell for an ordinary window with Panel metadata.
