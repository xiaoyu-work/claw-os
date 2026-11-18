# Root Display Login Handoff

The OS-only PAM module is loaded by the `claw-desktop` greetd service for
authenticated user sessions. The text greeter has a separate PAM service
and does not start a compositor. The module checks the PAM account against
the kernel audit UID/session established by `pam_loginuid`, captures the
exact Root PAM worker, and starts only the protected display supervisor.

The `open-after` entry follows `common-session`, when logind has created the
session and cgroup. The `close-before` entry runs before keyring revocation
and logind teardown. Keyring revocation may change the PAM worker's saved GID
even after restoring its effective GID; display close needs the unchanged
authenticated Root peer. The only module argument is its phase.
Root derives identity, epoch, cgroup and executable; the bounded PAM locale
projection cannot pass loader or display variables.

The private registration sends the Root supervisor's pidfd and authority
channel. The module retains the exact workload cgroup before releasing
compositor execution. Cleanup affects that owned workload and child, never
all processes of a UID. The module fails with `PAM_SESSION_ERR` when it cannot
confirm startup or teardown. Inherited PAM state cannot clean up the parent's
session.

Build `cargo build -p claw-display-login --locked` with GNU PAM development
files. Agent installs the resulting `libpam_claw_display.so` in the
architecture-specific system PAM module directory. This is not a public App SDK.

The private display test command is `bash scripts/ci/test-boundaries.sh display`
from the repository root with its declared fixture prerequisites and writable
cgroup v2. The independent [headless protocol fixture](test/headless/Cargo.toml)
does not install a desktop. The six-input [PAM runner](test/fixtures/bootstrap.sh)
can use the real Hyprland executable for activation-only evidence; its
`--native-desktop` selector also checks the sealed Root request and response.
Neither establishes graphical login, rendering or installed signed-App acceptance.

See the [session module](../../core/src/display_session/MODULE.md) and
[native compositor integration](../../desktop/hyprland-claw/MODULE.md).
