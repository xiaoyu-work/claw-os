# Authenticated GUI Host

`gui/` owns the Root-supervised lifetime of one verified App GUI. It does not
introduce a permission store or treat a native executable, publisher, App ID,
desktop label or Panel presentation as authority.

## Contracts

Registration uses the existing App session authority and only the constrained
union of fixed/wild **operation** needs. MCP tools and argument-bound needs do
not join that union. Needs outside launcher delegation stay absent; GUI launch
does not turn them into approval prompts. The union does not evaluate `when`
conditions, so they cannot express per-tab GUI authorization.

For Binary, Node and Shell runtimes, [argument preparation](../../bridge/gui_args.rs)
appends `[desktop.exec, ...user_args]` after any interpreter/script arguments.
The selector must equal the verified manifest's `desktop.exec`; it is not
rewritten, split as a shell command or required to name an operation key.
Python retains its existing wrapper dispatch instead of receiving native argv.
The environment contains `COS_APP_GUI=1`, `COS_COMMAND=desktop.exec` and
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

The Root [display registry](../../display_session/MODULE.md) supplies the
authenticated owner and display epoch. The Host creates the instance ID, holds
the actual launch-gated child pidfd and mandatory cgroup, and installs one
creator lease before releasing App execution. The authority alias/revision,
expiry and rights come from the bound live grant, not App JSON or Wayland
security-context strings.

`clipboard.read:selection` and `clipboard.write:selection` are independent
checks. History, display and Panel metadata imply neither. Zero GUI needs
therefore give zero direct selection rights. Layer-shell is a separate lease
field; this generic launch path currently issues it as false. Producers needing
such authority require a coordinated authorization path, not a manifest flag
that bypasses policy.

Every renewal rechecks package/instance validity, process identity, the existing
App grant and App deny gate, the authenticated parent delegation and its existing
approval revocation generation. Authority changes retire the instance rather
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

These functions are not an installer RPC. `MANAGER` is process-local, and its
absence in a standalone `cos` process says nothing about instances held by the
Root broker. Directory installation, replacement, provenance rollback and
trust-revoke completion still need authenticated Root mutation coordination
and admission fencing. This unit supplies neither that protocol nor APT
package-to-App ownership. Provenance must not acquire a `clawd` dependency.

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
