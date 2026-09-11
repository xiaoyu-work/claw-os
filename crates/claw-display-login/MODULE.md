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
  --bin claw-gui-runner --bin claw-app-runner --locked
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
  "$fixture_target/debug/cosmic-session"
```

The C fixture uses real PAM authentication and kernel login identities with
private NSS/password files. Its replay/inheritance cases call the actual
installed PAM module against that authenticated handle. Every simulated login
has its own Root cgroup, as real display sessions do. The runner checks and
cleans only its own kernel subtree; its named scratch directory retains logs
and private overlay files for inspection.

The full runner has twelve positional inputs; the final input is the actual
`cosmic-session` executable. It also tests a missing settings daemon through an
empty private `PATH`, without a production test switch. A separate authenticated
display observer and private session bus stay alive while the real session must
exit with its original startup panic within five seconds. PAM must then complete
checked retirement, with an outer 25-second session/PAM deadline. Append
`--session-startup-only` to the same twelve-input command for the isolated
`clawd::server::gui_fixture::private_session_startup_failure` regression.
The six-input activation-only mode is unchanged.

This is an explicit Root-only fixture, never a command to restart a real login
or inspect a user's clipboard. It does not prove a real greetd/logind/KMS
installation, hardware access or native producer permission readiness.
