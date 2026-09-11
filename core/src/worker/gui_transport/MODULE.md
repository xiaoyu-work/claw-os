# GUI Transport Containment

This Linux-only bootstrap augments the existing complete worker sandbox for
Root-managed GUI execution. A private network namespace or a read-only socket
bind alone is insufficient: Unix pathname sockets and hardlink aliases can
still reach the host.

`claw-gui-runner` inherits a private stdin packet socket only during trusted
bootstrap. It installs the mandatory seccomp notification filter, transfers
exactly one listener FD to the retained Root endpoint, receives a read-only
stdin FIFO and replaces the socket before the existing App runner executes.
Other held descriptors are close-on-exec. The stdio/MCP runner has no new
bootstrap branch.

`kernel.rs` validates actual notification tasks and descriptor tables. A
notification PID is a TID: `kcmp(KCMP_FILES)` must establish the shared table
before leader-addressed `pidfd_getfd`. Socket addresses are copied once with a
bound and emulated by Root; the Host never inspects memory and then uses
`SECCOMP_USER_NOTIF_FLAG_CONTINUE` on a mutable request.

Only the fixed instance Wayland, private broker and prepared egress targets are
connectable. The [GUI relay](../../clawd/gui/MODULE.md) checks every App-written
segment's kernel credentials against the exact instance cgroup. Static
`SO_PEERCRED` from Root-emulated `connect` is not an App identity.
Socket/directory/pidfd transfers are refused, and egress carries no FDs.
Additional filters, arbitrary listeners, datagrams, raw network transports and
unmediated X11 are refused rather than reopened through enclosing mounts.

A helper's private process group is only its cancellation unit. Forked/execed
descendants still inherit the GUI syscall filter and remain in the checked
instance cgroup; SDK, provider and `cos` executable names grant no exception.
Private probes create separate process groups, check denied raw-socket and
allowed instance connections, and retain transfer descriptors through checked
retirement. This generic containment coverage is not acceptance of an Applet
provider's owner-data view or its complete SDK/helper/`cos` call chain.

Required facilities include cgroup v2 with delegated CPU/memory/PID controllers,
checked cgroup killing, pidfds, seccomp user notification, `pidfd_getfd`, `kcmp`,
namespace isolation and the existing bubblewrap capabilities. Missing kernel
support or authority is an explicit GUI launch/retirement error. Normal CLI
operation does not acquire a display dependency.

Bootstrap/frame tests live under `core/test/unit/worker/gui_transport/`.
The actual signed native fixture also checks forged environment, no inherited
control descriptors, alternate/hardlinked sockets and retained descendants.
The egress lifetime regression uses an owned Unix/TCP pair, transfers bytes in
both directions, then confirms shutdown and relay-thread completion.
