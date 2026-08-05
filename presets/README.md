# Build presets

Thin wrappers around the core build (`./build.sh <target>`) that pick the right
feature set and output format for a given **distribution form**, so you don't
have to remember the long `FEATURES=…` string.

Each preset just exports the right environment and calls the core build — it
adds no new build logic. Edit `_common.sh` to change the shared desktop feature
list.

| Preset | Command | Produces |
| --- | --- | --- |
| **Desktop VM** | `sudo ./presets/desktop.sh` | Bootable COSMIC desktop VM image (`vmdk`, 50G) for VMware. Full graphical Claw OS. |
| **WSL** | `sudo ./presets/wsl.sh` | WSL2 root filesystem tarball — headless Claw OS inside Windows. |
| **Docker** | `./presets/docker.sh` | Headless Claw OS Docker image (container runtime). |

## Notes

- **Desktop** is the only preset that overrides `FEATURES` (it adds `desktop`,
  `vmware`, `copilot-cli` and the VM/boot features). `wsl` and `docker` use
  their target's own correct defaults, so they pass no override.
- The desktop image is large and slow to build from scratch (**~1–2 h**: V8 for
  `cos-browser`, then the COSMIC crates). See
  [`docs/building-desktop.md`](../docs/building-desktop.md) for prerequisites,
  VMware setup, and incremental-rebuild tips.
- `docker.sh` doesn't need `sudo` (the Docker daemon does the privileged work);
  `desktop.sh` and `wsl.sh` do (`debootstrap`/`chroot`/loop devices).
- To pick a different VM format, override `FORMATS` yourself, e.g.
  `sudo FORMATS=vhdx ./presets/desktop.sh` for Hyper-V.
