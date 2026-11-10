# Rootfs Module

## Purpose

`rootfs/` composes the reusable Debian filesystem consumed by every image
target.

## Responsibilities

- Bootstrap the architecture-specific Debian base.
- Apply ordered feature packages, overlays, and install hooks.
- Install Claw OS Debian packages and optional model/runtime assets.
- Stamp complete builds so compatible consumers can safely reuse a rootfs.

## Key Files

| Path | Role |
| --- | --- |
| `build.sh` | Feature parser, bootstrap, composition, reuse stamp |
| `features/<name>/packages.txt` | Debian package dependencies |
| `features/<name>/overlay/` | Files copied into the rootfs |
| `features/<name>/install.sh` | Feature-specific installation logic |
| [`features/README.md`](features/README.md) | Feature contract and available features |
| `../scripts/lib/image-profiles.sh` | Canonical target feature lists |

## Dependencies

Features describe OS capabilities; targets select profiles and package the
result. Feature code must not depend on a WSL/Docker/VM-specific staging path.
Installed Claw OS binaries arrive through packages built from the current
source. Reuse is allowed only when the complete stamp, artifacts, environment,
architecture, and feature list match.

The release workflow downloads its same-run, signed Agent/Base packages and
separately authenticated App APT inputs before
headless image composition. `cos-core/install.sh` looks up the exact version,
including Debian's `%3a` filename encoding for the security epoch, so existing
artifacts satisfy composition without a second binary/package build. It
configures the signed App channel and installs the checked App/support/library
`.deb` closure in the ordinary transaction, without deleting an existing Apps
root or copying product source into Agent. Desktop uses the same independent
package graph, including matching native packages.

Production Desktop no longer resolves App `main`, mounts native App sources or
copies App manifests. Shell product UI/private-library links are removed,
while independent App packages preserve the original native surfaces.
The OS notification renderer consumes public SDK presentation records/proxy.
Native build and installed UX acceptance remain required.
Source-only fixtures preserve a development layout, described below,
without becoming a production fallback.

Historically, desktop fixture builds resolved App `main` once and prepared its native inputs on the host, then
bind `build/native-apps` at the matching relative dependency path inside the
chroot. The shell, shared services and forked toolkit stay OS-owned; the App
source cache and generated inputs never enter the installed image.
The source mounts preserve the repository-relative `desktop`, SDK/runtime
and `build/native-apps` layout, including standalone Launcher/Editor/Files/Terminal/Store/Capture/Media Player
and nested Settings workspace dependencies.
The native preparation's resolved source lock is copied into the desktop
package stage before compilation, so packaging cannot pair an older binary
with a newly fetched App manifest. Source selection adds no runtime Git updater.
Notifications' standalone daemon keeps its private config/util crates inside
that App source fixture. They are not OS build inputs or exported interfaces.
The applet/panel retain their own toolkit graphs and the daemon retains its
independent original graph. Its descriptor/binary are App-owned; durable
service state and legacy `notify` JSON are not image inputs.
Capture keeps its original portal-client graph rather than inheriting the
shell renderer; its binary/resources remain desktop-package assets while
the shared portal and capture authority stay OS-owned.
Media Player retains its independent original renderer/GStreamer graph;
its live native UI/MPRIS state stays product-owned, while the scoped adapter
and authenticated owner-session selection stay OS-owned. No media or user
state enters the build inputs, and its resources stay in the desktop package.

## Tests

Validate changed shell scripts with `bash -n`. A real composition requires a
native Linux filesystem and root privileges:

```bash
bash -n rootfs/build.sh rootfs/features/<name>/install.sh
sudo ./rootfs/build.sh --features <comma-separated-features>
```

Do not run rootfs builds from `/mnt/c` on WSL.

The Mail registration feature does not build or copy runtime code. Its
unprivileged fixture test runs the real install script against a temporary
rootfs and ensures the package-owned App, SDK and XPI remain unchanged:

```bash
python3 -m pytest -q rootfs/features/claw-mail-ai/test_install.py
```
