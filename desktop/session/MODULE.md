# OS Desktop Session

This GPL session-manager fork retains its [license](LICENSE.md) and
[provenance](../PROVENANCE.md). It starts downstream OS desktop components;
it no longer creates the compositor or an owner-authored display handshake.

`data/start-cosmic` retains user startup-shell customization, then invokes the
unprivileged `/usr/local/bin/claw-display-session --exec /usr/bin/cosmic-session`.
`src/comp.rs` attaches through the
[Root-authenticated login subscription](../../crates/claw-display-control/MODULE.md).
The canonical display/runtime/user-bus values replace inherited display
claims; there is no divergent `dbus-run-session` fallback. Arbitrary compositor
executable/argument overrides are refused.

The [PAM display handoff](../../crates/claw-display-login/MODULE.md) precedes
user startup profiles. The Root compositor retains bounded PAM locale values
from the existing PAM environment stack. Downstream user customization remains
presentation, not an owner, display epoch or permission grant.

`src/main.rs` watches both the component/session requests and the authenticated
display lifetime. Display loss is an explicit failure, not an owner-compositor
restart or a successful interrupted command. An explicit session restart
restarts downstream components and reattaches the existing Root display; a
new compositor epoch requires the coordinated login lifecycle.
The cancellation guard is installed before attaching the blocking display
watcher, so a downstream component startup panic cannot leave Tokio runtime
shutdown waiting on a live display subscription. Normal exits still cancel
and check the watcher join; startup failures retain their original diagnostic.

## File-manager desktop intents

The optional `data/claw-file-manager.desktop` resource installs in
`/etc/xdg/autostart/`. `file_manager.rs` consumes this specific XDG entry even
when the general `autostart` feature is disabled or systemd is running.
It honors the first user/system resource's Hidden/OnlyShowIn/NotShowIn
availability, accepts only the Agent-installed helper with an optional
`--app <canonical-id>`, and starts it directly after authenticated display
attachment. The helper is an ordinary same-owner child of `cosmic-session`,
not a systemd-user or D-Bus-activated service. The already-declared desktop
entry parser is therefore a normal session dependency, not optional startup
authority.

`X-systemd-skip=true` prevents the user manager's autostart generator from
starting a second helper without this kernel login ancestry. The general
non-systemd loop skips the same entry. Session exit/restart sends SIGTERM,
waits, and escalates only its retained helper if needed; there is no automatic
restart. Drop also contains startup unwinds. This does not replace the Root
display watcher, user/display-epoch retirement or PAM logout.

Desktop owns this COSMIC-only resource, while Agent provides the
`claw-os-file-manager-v1 (= 1)` executable interface. OnlyShowIn is availability,
not authority: a forged desktop label or a user-manager launch still cannot
satisfy Root GUI admission without a real PAM/kernel login. Apps keep no raw
bus/IPC access. Full Files UI/ELF/resources and its coordinated App-owned
FileManager1 removal remain a separate product cutover.

From this directory, use `cargo build --locked` and `cargo clippy --locked`.
`cargo test --locked file_manager::tests` checks optional startup selection;
the core `claw-file-manager` bin tests exercise the actual private-bus
transport with a test-only executable, not an installed GUI or real login.
The private [PAM/runtime fixture](../../core/src/display_session/MODULE.md)
exercises the installed subscription wrapper, credentials, environment
replacement, FD custody, repeated subscribers and display loss. Its additional
`cosmic-session` executable input exercises missing-component startup against
a private bus and an authenticated display kept alive throughout the bounded
session-exit check, then requires checked PAM exit. Append
`--session-startup-only` to the full fixture command in the
[PAM module guide](../../crates/claw-display-login/MODULE.md) to run only this
regression. It does not start a real user service manager, KMS desktop or live
login.
