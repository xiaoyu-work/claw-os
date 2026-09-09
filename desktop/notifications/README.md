# Cosmic Notifications

Layer Shell notifications daemon which integrates with COSMIC.

# Building

Cosmic Notifications is set up to build a deb and a Nix flake, but it can be built using just.

Some Build Dependencies:
```
  cargo,
  just,
  intltool,
  appstream-util,
  desktop-file-utils,
  libxkbcommon-dev,
  pkg-config,
  desktop-file-utils,
```

## Build Commands

For a typical install from source, use `just` followed with `sudo just install`.
```sh
just
sudo just install
```

If you are packaging, run `just vendor` outside of your build chroot, then use `just build-vendored` inside the build-chroot. Then you can specify a custom root directory and prefix.
```sh
# Outside build chroot
just clean-dist
just vendor

# Inside build chroot
just build-vendored
sudo just rootdir=debian/cosmic-notifications prefix=/usr install
```

# Debugging & Profiling

## Profiling async tasks with tokio-console

To debug issues with asynchronous code, install [tokio-console](https://github.com/tokio-rs/console) and run it within a separate terminal. Then kill the **cosmic-notifications** process a couple times in quick succession to prevent **cosmic-session** from spawning it again. Then you can start **cosmic-notifications** with **tokio-console** support either by running `just tokio-console` from this repository to test code changes, or `env TOKIO_CONSOLE=1 cosmic-notifications` to enable it with the installed version of **cosmic-notifications**.

## Claw OS MCP service

The App Host starts `/usr/bin/cosmic-notifications` with `COS_MCP_SERVER=1`.
This mode submits bounded intent through `/usr/local/bin/cos` to the
capability-gated OS Notification Service; it never opens a session bus or starts
a second daemon. The manifest owns the tool catalog.

MCP 0.2 returns a durable `notif-` string. Closing is restricted to the same
authenticated App and owner; legacy numeric desktop IDs are explicitly
unsupported. `app_name` is only a display label. Icons must be theme names, not
file paths/URLs. `expire_ms` (-1/default, 0/forever, positive signed 32-bit
milliseconds) controls the popup, not durable activity retention. `transient`
omits the desktop history copy but never bypasses core durability. Optional
`dedupe_key` partitions replay within the same App/owner.

The existing OS Agent bridge alone consumes desktop delivery leases. It posts
plain-text content to the native daemon and maps genuine UI acknowledgement,
dismissal and durable close updates without another notification store. Native
freedesktop interoperability remains; numeric IDs are now bound to their D-Bus
sender, and the panel uses its private connection for user dismissal.
Existing local desktop DND/configuration remains an additional presentation
mute. No legacy `notify` JSON history or settings are imported or removed.
