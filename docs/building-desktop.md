# Building the Claw OS Desktop Image

This guide covers building a bootable Claw OS **desktop VM disk image** and loading
it in VMware. Only the steps are covered here — see `targets/vm/build.sh` for the
full details.

## What you get

The build produces a virtual disk under `build/`, e.g.
`build/claw-os-vm-amd64.vmdk`, that you can boot in a VM.

> Requires a Linux host with root. Builds natively only — the image
> architecture matches the host (`amd64` → `amd64`, `arm64` → `arm64`).

## 1. Build steps

### Prerequisites (Debian/Ubuntu host)

System tools for the rootfs/disk pipeline:

```bash
sudo apt update
sudo apt install -y git build-essential pkg-config \
                    debootstrap qemu-utils parted dosfstools rsync \
                    util-linux e2fsprogs grub-efi-amd64-bin grub-pc-bin
# On an arm64 host, use grub-efi-arm64-bin instead (and drop grub-pc-bin).
```

A **Rust toolchain** on the host, used to compile the `cos` / `clawd` /
`cos-browser` binaries that go into the image (the build invokes `cargo`
for you — see note below):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
. "$HOME/.cargo/env"
```

> You do **not** run `cargo` yourself. `./build.sh` compiles the core
> binaries automatically the first time they're missing (`cos` + `clawd`
> come from one crate; `cos-browser` bundles V8, so its first build is
> slow). The desktop crates (`cosmic-*`, `cos-agent-ui`) are compiled
> separately inside the image's chroot, which installs its own toolchain —
> the host Rust above is only for the core binaries. When building under
> `sudo`, the toolchain is still found via `$SUDO_USER`'s home, so a
> normal user-level rustup install is enough.

### Get the source

```bash
git clone https://github.com/xiaoyu-work/claw-os.git
cd claw-os
```

### Build command

Run from the repository root (the `claw-os` directory you just cloned):

```bash
sudo FEATURES=base,cos-core,systemd,kernel,desktop,vmware,copilot-cli,grub-disk,vm,apt-source \
     FORMATS=vmdk SIZE=16G ./build.sh vm
```

- `FORMATS` — output format: `vmdk` (VMware), `vhdx` (Hyper-V), `qcow2` (QEMU), or `raw`.
- `SIZE` — virtual disk size (sparse, so the file is much smaller).

Output: `build/claw-os-vm-<arch>.vmdk`.

### Linux

Run the prerequisites and build command above directly on a Debian/Ubuntu machine.

### Windows (WSL2)

1. Install a Debian or Ubuntu WSL2 distro:
   ```powershell
   wsl --install -d Ubuntu
   ```
2. **Clone into the Linux filesystem, not `/mnt/c`.** `debootstrap` creates
   device nodes and hardlinks that the Windows drive mount (drvfs) cannot
   represent, so a build under `/mnt/c/...` fails partway through. Work from
   your WSL home instead:
   ```bash
   mkdir -p ~/workspace && cd ~/workspace
   git clone https://github.com/xiaoyu-work/claw-os.git
   cd claw-os
   ```
   Then run the **Prerequisites** and **Build command** steps inside WSL.
   The output disk is reachable from Windows at
   `\\wsl$\Ubuntu\home\<user>\workspace\claw-os\build\`.
3. Run the build so it can't be paused or interrupted by a stray key:
   ```bash
   sudo FEATURES=base,cos-core,systemd,kernel,desktop,vmware,copilot-cli,grub-disk,vm,apt-source \
        FORMATS=vmdk SIZE=16G ./build.sh vm 2>&1 | tee /tmp/claw-build.log
   echo "BUILD EXIT = ${PIPESTATUS[0]}"
   ```
   - `${PIPESTATUS[0]}` is the real `build.sh` exit code — with `| tee`, a
     plain `$?` only reports `tee`'s status.
   - The build runs apt fully non-interactively with the dpkg pty disabled
     (`Dpkg::Use-Pty "0"` + `DEBIAN_FRONTEND=noninteractive`), so it will **not**
     stop on its own at *"Processing triggers …"* the way older builds did.
   - If the build *does* freeze with no output, it was suspended (`SIGTSTP`),
     usually from a stray `Ctrl-Z` in the build window. Find the stopped
     process group and resume it:
     ```bash
     ps -eo pid,pgid,stat,cmd | awk '$3 ~ /T/'   # look for STAT containing T
     sudo kill -CONT -<pgid>                      # note the leading '-' (whole group)
     ```
   - For a long build that survives a closed terminal, run it inside `tmux`
     (`sudo apt install -y tmux`, `tmux new -s build`, detach with
     `Ctrl-b d`, reattach with `tmux attach -t build`).

> On a **Windows-on-ARM** PC, WSL is `arm64`, so you can only build the `arm64`
> image — and VMware has no Windows-on-ARM build. Use Hyper-V (`FORMATS=vhdx`)
> instead, or build the `amd64` image on an x86 machine.

### macOS

macOS cannot run `debootstrap`/`chroot`/loop devices natively. Run the build
inside a **Linux VM** (UTM, VMware Fusion, Lima, …) or a privileged Linux
container, then follow the **Linux** steps inside it. On Apple Silicon the Linux
VM is `arm64`, so you get an `arm64` image.

## 2. Load it in VMware

1. Open **VMware Workstation / Player** (Windows/Linux) or **VMware Fusion** (macOS, Intel).
2. **Create a New Virtual Machine** → *Custom* → *I will install the operating
   system later*.
3. Guest OS: **Linux** → *Debian 11.x 64-bit* (or *ARM 64-bit* for an `arm64` image).
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

