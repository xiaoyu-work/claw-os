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

Desktop builds prepare all pinned native App inputs on the host and
bind `build/native-apps` at the matching relative dependency path inside the
chroot. The shell, shared services and forked toolkit stay OS-owned; the App
source cache and generated inputs never enter the installed image.
The source mounts preserve the repository-relative `desktop`, SDK/runtime
and `build/native-apps` layout, including standalone Launcher/Editor/Files/Terminal/Store/Capture/Media Player
and nested Settings workspace dependencies.
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
