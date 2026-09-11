# Root Display Session

## Boundary

The [OS PAM module](../../../crates/claw-display-login/MODULE.md) activates the
display before greetd drops into the user's startup shell. The authenticated
PAM account must match the real kernel audit login UID/session. A protected
Root channel registers the exact Root supervisor child, its pidfd and its
parent/start identity; the request has no owner, session, epoch or executable
fields. The broker generates a fresh epoch and retains the workload guard
before the compositor can dispatch protocols.

`host.rs` executes only the protected `/usr/bin/cosmic-comp`, with the
authenticated account's credentials and a cleared environment. The inherited
control descriptor precedes Wayland dispatch. Bounded locale values come
from the PAM handle, not a loader environment; the compositor and downstream
session share the canonical logind user bus. Neither presentation metadata
nor that bus authorizes a GUI instance.

| File | Responsibility |
| --- | --- |
| `registry.rs` | Root activation admission, epochs, heartbeat and correlated creator/retirement requests |
| `host.rs` | Owned compositor process, private control, read-only login subscriptions and display teardown |
| `containment.rs` | Exact Root login/workload cgroups, pre-exec membership and checked cleanup |
| `creator.rs` | One Root security-context creator per GUI instance, refresh and retirement |
| `runtime.rs` | Root-created instance sockets; protected backend versus traversable credential-checking fronts |
| `client.rs` | Unprivileged `claw-display-session --watch` / `--exec` attachment |

The shared closed protocol is
[`claw-display-control`](../../../crates/claw-display-control/MODULE.md), an
unpublished OS dependency, not public SDK 1.0.0. GUI rights and supervised App
execution belong to [`clawd/gui`](../clawd/gui/MODULE.md).

Locale projection currently preserves only the bounded PAM environment.
There is no AccountsService `Language` or locale1 startup resolver here.
The compositor receives that snapshot before the downstream user login shell
can customize its own environment. Owner preference precedence, installed-locale
selection and the larger language-list bounds still need coordinated startup
work; regional persistence/readback does not establish startup consumption.

## Installation and lifecycle

| Installed path | OS package |
| --- | --- |
| `/usr/lib/cos/bin/claw-display-host` | Agent |
| `/usr/local/bin/claw-display-session` | Agent |
| `/usr/local/bin/claw-gui-runner` | Agent |
| `/usr/lib/<GNU-multiarch>/security/pam_claw_display.so` | Agent |
| `/usr/bin/cosmic-comp`, `/usr/bin/cosmic-session` | Desktop |
| `/etc/pam.d/cosmic-greeter` and greetd startup integration | Desktop |

Agent provides the OS-only `claw-os-display-session-v1` ABI; Desktop requires
it. The PAM library is built for the GNU target and requires the system PAM
development library. Package assembly takes real architecture-checked ELF
inputs, never an App payload or a source-only substitute.

Normal PAM logout allows a bounded compositor shutdown, then confirms the
owned workload is empty and waits for GUI resource retirement. Control loss,
expired authority and crashes fail closed and can report a PAM/session error
even after emergency containment succeeds. An interrupted `--exec` child is
not a successful session command. Existing disclosed bytes cannot be recalled.
Ordinary login-leaf background processes remain logind-owned; they are not
killed or moved by UID inference.

An incomplete final cleanup remains owned by its existing authority thread.
The display is unavailable, but its registration, capacity slot and workload
reference remain quarantined until checked GUI cleanup succeeds. Retries back
off from 250 ms to 2 seconds; the 16-registration ceiling also bounds these
retirement owners. Verified workload retirement is monotonic: a retry does not
reopen an empty cgroup that PAM has already removed. Only the matching
registration is released, so a stale completion cannot remove another display.
An in-flight bounded egress connect can outlast an individual retirement
deadline; that is pending cleanup, not permission to abandon its owner.

Replacing/restarting the authority or compositor retires the affected display
epoch. Agent/Desktop activation changes therefore need coordinated package
installation and logout/re-login, not a hot owner-socket registration. Package
update controllers must surface that interruption. Independent App replacement
would need App-scoped retirement through authenticated Root mutation
coordination and admission fencing. That installer/rollback path is not wired:
calling a process-local GUI manager from standalone `cos` is not a checked
retirement of the Root broker's instances.
See the installed update contract in [updating](../../../docs/updating.md).

## Validation and limits

The private process fixture is
`clawd::server::gui_fixture::private_authenticated_gui`. Its maintained
[runner](../../../crates/claw-display-login/test/fixtures/bootstrap.sh) requires
Root in private mount/network/IPC/UTS namespaces. It installs actual built
runtime binaries and PAM, authenticates a disposable NSS account, creates real
kernel login identities and supervises signed synthetic native Apps. No real
desktop, user selection/history, provider model or account data is used.

Inputs, in order, are the core libtest executable, display host, headless
compositor, PAM library, compiled PAM C fixture, private scratch directory,
`cos`, GUI runner, App runner, native protocol probe, display-session client and
the actual `cosmic-session` executable. The fixture also requires `dbus-daemon`.
The separate six-input activation-only mode remains available. Build commands
and the private fixture entry points are documented in the
[PAM module guide](../../../crates/claw-display-login/MODULE.md).

The fixture covers authentication/installation/locale failures, duplicate and
inherited PAM handles, subscriber reuse, forged display environment, descriptor
custody, independent selection rights, actual write-only payloads/clears, DnD,
socket aliases, parent/permission revocation, helper/transfer FD teardown,
supervisor death and expiry. Revocation cases distinguish the system bucket,
UID 0 and other owners, and force an audit-write failure after a durable App
denial through the actual owner CLI. Checked teardown must finish before that
CLI reports its audit error. This does not add a capability-acquisition path.
The startup-unwind case keeps an authenticated display subscription and private
bus alive while a missing component makes the actual session panic; both that
session and PAM must exit within bounds, retaining the original diagnostic.
It is not a real greetd/logind/KMS login or an installed-image acceptance test.

The separate ignored
`display_session::registry::tests::private_deferred_retirement_reclaims_the_exact_slot_and_workload`
case requires Root and cgroup v2. It confines only its own test process and
sleeping fixture children to a fresh cgroup, defers checked completion past
the original attempts, and verifies quarantine, retry pacing, exact workload
reference/capacity reclamation and unaffected displays. Run that exact case
with `--ignored --test-threads=1`; it never uses a real login or GUI.

Headless/no-display systems retain CLI operation and explicitly refuse GUI
launches. GUI execution additionally requires the kernel facilities documented
by [its transport](../worker/gui_transport/MODULE.md); no rlimit or X11 fallback
replaces missing containment.
