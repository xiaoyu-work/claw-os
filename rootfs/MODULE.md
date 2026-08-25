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

## Tests

Validate changed shell scripts with `bash -n`. A real composition requires a
native Linux filesystem and root privileges:

```bash
bash -n rootfs/build.sh rootfs/features/<name>/install.sh
sudo ./rootfs/build.sh --features <comma-separated-features>
```

Do not run rootfs builds from `/mnt/c` on WSL.
