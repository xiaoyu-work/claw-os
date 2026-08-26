# Building the Claw OS Desktop Image

This guide covers building a bootable Claw OS **desktop VM disk image** and loading
it in VMware. Only the steps are covered here — see
`targets/common/disk-image.sh` for the full disk-layout details.

## What you get

The build produces a virtual disk under `build/`, e.g.
`build/claw-os-vm-amd64.vmdk`, that you can boot in a VM.

> Requires a Linux host with root. Builds natively only — the image
> architecture matches the host (`amd64` → `amd64`, `arm64` → `arm64`).

## 1. Build steps

### Prerequisites (Debian/Ubuntu host)

> **On Windows?** This is just a normal Linux build run inside WSL2. Install a
> Debian/Ubuntu WSL2 distro first (`wsl --install -d Ubuntu`), then run every
> step below inside it.

System tools for the rootfs/disk pipeline:

```bash
sudo apt update
sudo apt install -y git build-essential pkg-config \
                    debootstrap qemu-utils parted dosfstools mtools rsync \
                    util-linux e2fsprogs grub-efi-amd64-bin grub-pc-bin
# On an arm64 host, use grub-efi-arm64-bin instead (and drop grub-pc-bin).
```

A **Rust toolchain** on the host, used to compile the `cos` / `clawd` /
`cos-browser` binaries that go into the image:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
. "$HOME/.cargo/env"
rustup default stable          # make sure a default toolchain is selected
```

> You do **not** run `cargo` yourself — `./build.sh` compiles the core binaries
> on demand (`cos` + `clawd` come from one crate; `cos-browser` bundles V8).
> The desktop crates are built separately inside the image's chroot with their
> own toolchain. Under `sudo` the build reuses your user-level toolchain
> (`RUSTUP_HOME=$SUDO_USER/.rustup`), so one normal rustup install is enough.

### Get the source

```bash
git clone https://github.com/xiaoyu-work/claw-os.git
cd claw-os
```

> **On WSL2:** clone into the Linux filesystem (e.g. `~/workspace`), **not**
> `/mnt/c/...`. `debootstrap` needs device nodes and hardlinks that the Windows
> drive mount (drvfs) cannot represent, so a build under `/mnt/c` fails partway
> through. The finished disk is reachable from Windows at
> `\\wsl$\Ubuntu\home\<user>\workspace\claw-os\build\`.

### Build command

Run from the repository root (the `claw-os` directory you just cloned):

```bash
sudo ./presets/desktop.sh
```

This preset sets the desktop feature set, `FORMATS=vmdk` and `SIZE=50G` for
you, then calls `./build.sh vm`. See [`presets/README.md`](../presets/README.md)
for the `wsl` and `docker` presets too.

Output: `build/claw-os-vm-<arch>.vmdk`.

<details>
<summary>Manual equivalent (if you want to tweak features/format/size)</summary>

```bash
sudo FEATURES=base,cos-core,systemd,kernel,desktop,vmware,copilot-cli,grub-disk,vm,apt-source,local-user \
     FORMATS=vmdk SIZE=50G ./build.sh vm
```

- `FORMATS` — output format: `vmdk` (VMware), `vhdx` (Hyper-V), `qcow2`
  (QEMU), or `raw`. Azure's fixed `vhd` is produced by `./build.sh azure`.
- `SIZE` — virtual disk size (sparse, so the file is much smaller).

`FORMATS` and `SIZE` also work with the preset, e.g.
`sudo FORMATS=vhdx ./presets/desktop.sh` for Hyper-V.

</details>

A from-scratch build typically takes roughly **30–60 minutes** on a prepared
host and is mostly silent while it compiles the desktop crates. Initial
dependency downloads or slower hardware can take longer; quiet periods are
normal, not a hang.

> On a **Windows-on-ARM** PC, WSL is `arm64`, so you can only build the `arm64`
> image — and VMware has no Windows-on-ARM build. Use Hyper-V (`FORMATS=vhdx`)
> instead, or build the `amd64` image on an x86 machine.

> **macOS** cannot run `debootstrap`/`chroot`/loop devices natively — run the
> steps above inside a Linux VM (UTM, VMware Fusion, Lima, …). On Apple Silicon
> that VM is `arm64`, so you get an `arm64` image.

## 2. Load it in VMware

1. Open **VMware Workstation / Player** (Windows/Linux) or **VMware Fusion**
   (macOS, Intel or Apple Silicon).
2. **Create a New Virtual Machine** → *Custom* → *I will install the operating
   system later*.
3. Guest OS: **Linux** → *Debian 13.x 64-bit* (or a generic *ARM 64-bit*
   Linux guest for an `arm64` image).
4. When prompted for a disk, choose **Use an existing virtual disk** and select
   the built `build/claw-os-vm-<arch>.vmdk`. Keep the existing format if asked.
5. (arm64 only) In *VM Settings → Options → Advanced*, set firmware to **UEFI**.
   amd64 images boot with either BIOS or UEFI.
6. **Power on** the VM.

## 3. Iterating without rebuilding the image

You do **not** need to rebuild the whole `.vmdk` for most changes.

### Python `cos` apps (`apps/`) — no build

The kernel re-reads each app's `main.py` from disk on every
`cos app <id> <op>` call, so there is no build and no restart. Edit
`/usr/lib/cos/apps/<id>/main.py` in the running VM and the next command
picks it up.

### Rust desktop UI (`desktop/`) — recompile one crate, no full image

The desktop components are compiled binaries (e.g. `cosmic-files`,
`cosmic-store`, `cosmic-edit`, `cos-agent-ui`). Changing them needs a
recompile, but only of the single crate — then drop the new binary into
the running VM:

```bash
# On a Linux build host (or inside the VM), build just the one crate:
cargo build --release -p cosmic-files      # or -p cos-agent-ui, etc.

# Copy it into the running VM and replace the installed binary:
scp target/release/cosmic-files user@vm:/tmp/
ssh user@vm 'sudo install -m755 /tmp/cosmic-files /usr/bin/cosmic-files'
```

> App binaries live in `/usr/bin/` (e.g. `cosmic-files`); `cos-agent-ui`
> and `cos-agent-bridge` live in `/usr/local/bin/`.

Then reload only what changed:

- A regular app (files, edit, store, agent-ui, …) → close and reopen its window.
- A session-core component (`cosmic-comp`, `cosmic-session`,
  `cosmic-greeter`, panel) → log out and back in, or restart
  `graphical.target`.

Rebuild the whole `.vmdk` only when you change the feature list, system
overlay files, or want a clean distributable image.
