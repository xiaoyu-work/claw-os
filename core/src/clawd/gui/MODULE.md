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
retirement; there is no new live capability acquisition or file-picker grant.

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

Root exposes the verified package's read-only `resources/` as
`COS_APP_RESOURCE_DIR`. Its `usr/share` precedes standard system data roots in
`XDG_DATA_DIRS`, so toolkit schema/icon lookup does not depend on global App
resource activation. `COS_OWNER_HOME` supplies the authenticated owner path
for UI choices; `HOME` and App state remain private. Neither environment path
grants filesystem access, defeats protected-store checks or supplies a user bus.

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
| `inputs.rs` | Closed launch/wait/stop shapes, argument limits and presentation-only keys |
| `manager.rs` | Reservation, exact launcher ownership, results and checked retirement barriers |
| `supervision.rs` | Sandbox/cgroup preparation, binding, creator installation, refresh and cleanup |
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
