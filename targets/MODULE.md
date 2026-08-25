# Targets Module

## Purpose

`targets/` packages a composed rootfs into platform artifacts without
redefining the operating-system feature set.

## Responsibilities

- Build WSL, Docker, VM, ISO, and cloud-specific artifacts.
- Apply platform integration and first-boot identity policy.
- Keep platform-only overlays out of the shared rootfs.
- Produce deterministic artifact paths consumed by workflows/releases.

## Key Files

| Path | Role |
| --- | --- |
| `wsl/build.sh` | Modern `.wsl` package and OOBE overlay |
| `docker/Dockerfile` | Scratch image from the shared rootfs |
| `docker/container-entrypoint.sh` | Container identity and startup |
| `vm/` | Disk-image staging |
| `iso-live/`, `iso-installer/` | Live and install media |
| `azure/` | Azure generalization and fixed-VHD packaging |
| `common/` | Shared target helpers |
| `../scripts/lib/image-profiles.sh` | Target-to-feature source of truth |

## Dependencies

Targets consume `build/claw-os-rootfs` and profile definitions. WSL and Docker
share one rootfs per architecture in CI. Azure obtains identity from
cloud-init; WSL uses first-launch OOBE; Docker creates the requested user at
container startup. These target policies must not leak back into reusable
rootfs features.

## Tests

Run syntax checks for changed provisioning scripts and the narrowest target
build that exercises the change:

```bash
bash -n targets/<target>/*.sh
sudo ./build.sh wsl
./build.sh docker
```

See [`../docs/image-architecture.md`](../docs/image-architecture.md) before
changing profile or identity behavior.
