# Root Display Login Handoff

This OS-only PAM session module is loaded by the packaged `cosmic-greeter`
stack, not by an App or a setuid launcher. It checks the PAM-authenticated
user against the kernel audit login UID and session established by
`pam_loginuid`, captures the Root PAM worker, and starts only the fixed
Root display supervisor with a cleared environment and two private descriptors.

The `open-after` entry belongs after `common-session`, when login session and
cgroup membership exist. The `close-before` entry belongs before it so checked
display retirement precedes logind/PAM teardown. No username, UID, PID, epoch,
executable or path is accepted as a module argument. Greetd retains the Root
PAM worker while its user child executes; the Root binding precedes user
startup profiles.

Both greetd's general service and its separately configured default/greeter
service use this PAM stack. The fixed helper and every executable ancestor
must be Root-owned and non-writable to other users. Startup waits for the
protected broker endpoint with a bound; it never falls back to an owner
socket. The existing `pam_env` locale values are copied from the authenticated
PAM handle through a closed, bounded presentation-only field. `LD_PRELOAD`,
`LOCPATH`, display claims and arbitrary environment keys cannot be projected.

The private broker registration transfers the Root-owned supervisor pidfd and
authority channel. The broker derives login identity from the actual Root PAM
peer and assigns the display epoch. The module retains the checked workload
cgroup descriptor before compositor execution is released. Closure and
emergency cleanup address that exact workload and owned child, never a UID,
process name or caller-named cgroup.

Errors return `PAM_SESSION_ERR` and a PAM diagnostic. FFI panics are caught only
to fail the session, never to authorize it. Inherited PAM data in a forked
child cannot run the parent's privileged cleanup.

Build with `cargo build -p claw-display-login` from the repository root.
The system PAM development/link library is required. The resulting
`libpam_claw_display.so` installs as `pam_claw_display.so` in the system PAM
module directory; it is not part of an App package or the public SDK.

## Private authenticated process fixture

Run from the OS repository root on Linux/WSL with the normal native compiler,
PAM development library, `dbus-daemon` and existing worker sandbox dependencies:

```bash
runtime_target="$PWD/build/gui-runtime-target"
fixture_target="$PWD/build/gui-fixture-target"
CARGO_TARGET_DIR="$runtime_target" cargo build -p cos \
  --bin cos --bin claw-display-host --bin claw-display-session \
  --bin claw-gui-runner --bin claw-app-runner --bin claw-file-manager --locked
CARGO_TARGET_DIR="$runtime_target" cargo build -p claw-display-login --locked
CARGO_TARGET_DIR="$runtime_target" cargo test -p cos --lib --locked \
  --no-run --message-format=json > "$runtime_target/gui-tests.json"
test_binary="$(python3 -c 'import json,sys; print(next(x["executable"] for x in map(json.loads,sys.stdin) if x.get("executable") and x.get("profile",{}).get("test") and x.get("target",{}).get("name") == "cos"))' < "$runtime_target/gui-tests.json")"
CARGO_TARGET_DIR="$fixture_target" cargo build \
  --manifest-path desktop/comp/test/display-control/Cargo.toml --locked
CARGO_TARGET_DIR="$fixture_target" cargo build \
  --manifest-path desktop/session/Cargo.toml --locked
cc -std=c11 -Wall -Wextra -Werror \
  crates/claw-display-login/test/fixtures/pam_login.c \
  -lpam -ldl -o "$runtime_target/pam-login-fixture"
scratch="$PWD/build/gui-authenticated.$BASHPID"
mkdir -m0700 "$scratch"
sudo unshare --mount --net --ipc --uts --fork \
  bash --noprofile --norc crates/claw-display-login/test/fixtures/bootstrap.sh \
  "$test_binary" "$runtime_target/debug/claw-display-host" \
  "$fixture_target/debug/claw-display-headless-fixture" \
  "$runtime_target/debug/libpam_claw_display.so" \
  "$runtime_target/pam-login-fixture" "$scratch" \
  "$runtime_target/debug/cos" "$runtime_target/debug/claw-gui-runner" \
  "$runtime_target/debug/claw-app-runner" "$fixture_target/debug/claw-gui-probe" \
  "$runtime_target/debug/claw-display-session" \
  "$fixture_target/debug/cosmic-session" \
  "$runtime_target/debug/claw-file-manager" \
  desktop/session/data/claw-file-manager.desktop
```

The C fixture uses real PAM authentication and kernel login identities with
private NSS/password files. Its replay/inheritance cases call the actual
installed PAM module against that authenticated handle. Every simulated login
has its own Root cgroup, as real display sessions do. The runner checks and
cleans only its own kernel subtree; its named scratch directory retains logs
and private overlay files for inspection.

The full runner has fourteen positional inputs; inputs twelve through fourteen
are the actual `cosmic-session`, actual `claw-file-manager`, and the session's
unmodified autostart source resource. It also tests a missing settings daemon through an
empty private `PATH`, without a production test switch. A separate authenticated
display observer and private session bus stay alive while the real session must
exit with its original startup panic within five seconds. PAM must then complete
checked retirement, with an outer 25-second session/PAM deadline. Append
`--session-startup-only` to the same fourteen-input command for the isolated
`clawd::server::gui_fixture::private_session_startup_failure` regression.
The six-input activation-only mode is unchanged.

Append `--file-manager-only` to the fourteen-input command to select
`clawd::server::gui_fixture::private_file_manager_session`. The complete
`private_authenticated_gui` run also includes it. `scripts/ci/test-boundaries.sh
display` builds/passes both real executables and the autostart resource and
executes the complete fixture, not a test-name selection that can pass with
zero tests.

This case genuinely authenticates PAM, retains its kernel login, and runs
`claw-display-session --exec /usr/bin/cosmic-session`. The actual session starts
the actual facade as its child through the staged XDG resource. Only its
`--app` routing preference is overridden to a private, freshly signed
`gui-file-manager-test` package with explicit empty GUI needs. No parent App or
model grant is registered. Assertions bind the session/facade/cos pidfds and
kernel login, check absence of stale App/model environment, and verify zero
GUI capabilities. The existing real headless compositor and ordinary Root GUI
Host run the signed native probe, which commits a real Wayland surface,
checks literal URI argv and refuses both raw bus paths. Three standard methods
must produce checked successful GUI outcomes; a fourth native exit-7 case
must produce a checked failure with counts-only arguments and static
`gui_launch_failed` audit classification. Receipt markers alone are not GUI
completion evidence.

A second locked disposable NSS identity calls the same facade over a
fixture-only second listener. The canonical owner runtime directory remains
`0700`; only the isolated second socket is cross-UID reachable. Its authenticated
reply must come from the facade's unique bus owner, not a daemon-policy denial.
Root observes the current-thread facade with ptrace fork/vfork events and
requires no spawn across the different-UID call. Apps receive neither listener:
the signed GUI probe independently requires actual syscall-level bus refusal.
The same ptrace capability is already needed by the GUI transport's kernel
process/descriptor checks; failure is a test failure, not a skip.

Finally a live GUI and separately grouped descendant retain the instance
transport while the actual session exits. The test waits for the real facade,
cos and GUI pidfds, checked-empty/deleted instance cgroup and runtime removal,
then actual PAM retirement and an empty login cgroup. Private JSON/JSONL/log
artifacts must contain neither URI nor StartupID/credential sentinels.

Unrelated COSMIC component slots use explicitly idle C-fixture executables
in a private PATH; they never report a GUI outcome. A private `/run/systemd/system`
availability marker also exercises the session-owned path without contacting a
real user manager. This proves authenticated session-child transport and Root
GUI custody, not the Settings/Panel/Files Applet UIs, a real user-manager/logind
installation, the full Files product, or KMS/image acceptance. Linux compilation
and execution are mandatory downstream evidence; formatting/static checks do
not establish a passing authenticated session.

This is an explicit Root-only fixture, never a command to restart a real login
or inspect a user's clipboard. It does not prove a real greetd/logind/KMS
installation, hardware access or native producer permission readiness.
Its named-GUI case selects a second signed executable with an explicit empty
needs list under the same App identity, verifies literal argv and the actual
executed filename, and retains zero selection/LayerShell rights despite the
default GUI's operation needs and MCP declarations.
The named launcher has no `COS_SESSION` environment value: Root still derives
its exact registered parent from process identity, without an Admin bootstrap.
Additional cases bind explicit `ui.panel:Wild` grants to the same signed App
and executable, create real overlay surfaces with their own keyboard focus,
and verify panel-only, independent clipboard read/write and ordinary
`ui.window`/Panel-label-only admission. Panel revocation and display-control
expiry retain the actual instance cgroup, descendants and transfer descriptor
until checked retirement. Invalid owner/authority/process/expiry and
reused-instance creator requests remain refused even when they request Layer-shell.
