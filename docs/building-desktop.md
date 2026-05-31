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
rustup default stable          # make sure a default toolchain is selected
```

> You do **not** run `cargo` yourself. `./build.sh` compiles the core
> binaries automatically the first time they're missing (`cos` + `clawd`
> come from one crate; `cos-browser` bundles V8, so its first build is
> slow). The desktop crates (`cosmic-*`, `cos-agent-ui`) are compiled
> separately inside the image's chroot, which installs its own toolchain —
> the host Rust above is only for the core binaries. When building under
> `sudo`, the build reuses `$SUDO_USER`'s toolchain (it points `RUSTUP_HOME`
> at your `~/.rustup`), so a normal user-level rustup install is enough — you
> do not need a second, root-owned toolchain. Confirm your install works with
> `cargo --version` (no `sudo`); if that prints *"rustup could not choose a
> version of cargo to run"*, run `rustup default stable`.

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

> **What to expect / how long it takes.** A from-scratch build runs for
> roughly **1–2 hours** and is mostly silent during the compile stages — this
> is normal, it is not stuck:
> 1. `debootstrap` + base apt + Node/pip — a few minutes.
> 2. `cos-core`: compiles `cos` + `clawd` — a few minutes.
> 3. `browser`: compiles `cos-browser`, which **bundles V8 — 20–40 min** with
>    little output. This is the slowest core step; let it run.
> 4. `desktop`: compiles the COSMIC crates + agent UI inside the chroot —
>    **30–60 min**.
> 5. Disk assembly (`vm`) — a few minutes.
>
> The build detaches itself from the terminal (see below), so it will not be
> stopped by terminal job control; long silent periods are just compilation.

### Linux

Run the **Prerequisites** and **Build command** above directly on a
Debian/Ubuntu machine.

### Windows (WSL2)

WSL2 **is** a Linux build — the steps are identical to **Linux** above. Just
run the same **Prerequisites** and **Build command** inside your WSL distro,
with these three WSL-specific points:

1. Install a Debian or Ubuntu WSL2 distro:
   ```powershell
   wsl --install -d Ubuntu
   ```
2. **Clone into the Linux filesystem, not `/mnt/c`.** `debootstrap` creates
   device nodes and hardlinks that the Windows drive mount (drvfs) cannot
   represent, so a build under `/mnt/c/...` fails partway through. Work from
   your WSL home (`~/workspace/...`) instead. The output disk is then reachable
   from Windows at `\\wsl$\Ubuntu\home\<user>\workspace\claw-os\build\`.
3. Capture the log and read the real exit code (`| tee` otherwise masks it):
   ```bash
   sudo FEATURES=base,cos-core,systemd,kernel,desktop,vmware,copilot-cli,grub-disk,vm,apt-source \
        FORMATS=vmdk SIZE=16G ./build.sh vm 2>&1 | tee /tmp/claw-build.log
   echo "BUILD EXIT = ${PIPESTATUS[0]}"   # real build.sh status, not tee's
   ```
   `build.sh` automatically detaches from the terminal (`setsid`) and runs apt
   non-interactively, so the build can't be suspended by WSL2's tty layer — see
   the **Troubleshooting** `T`-state row if you are on an old checkout.

> On a **Windows-on-ARM** PC, WSL is `arm64`, so you can only build the `arm64`
> image — and VMware has no Windows-on-ARM build. Use Hyper-V (`FORMATS=vhdx`)
> instead, or build the `amd64` image on an x86 machine.

### macOS

macOS cannot run `debootstrap`/`chroot`/loop devices natively. Run the build
inside a **Linux VM** (UTM, VMware Fusion, Lima, …) or a privileged Linux
container, then follow the **Linux** steps inside it. On Apple Silicon the Linux
VM is `arm64`, so you get an `arm64` image.

### Troubleshooting

| Symptom | Cause | Fix |
| --- | --- | --- |
| Build dies partway through `debootstrap` / extraction when run from `/mnt/c/...` | The Windows drive mount (drvfs) can't create device nodes / hardlinks / Unix permissions `debootstrap` needs | Build from the WSL **native** filesystem (`~/workspace/...`), not `/mnt/c` |
| Build "hangs" with no output; `ps` shows processes in state **`T`/`T+`** at an apt/dpkg step | WSL2's tty layer sends a spurious `SIGTSTP` to the foreground process group (or a stray `Ctrl-Z`) | Fixed in current `build.sh` (it `setsid`-detaches + disables apt's pty). On an old checkout: `git pull`, or resume now with `sudo kill -CONT -<pgid>` |
| `error: cos binary not built` / `rustup could not choose a version of cargo to run` | No Rust toolchain, or a rustup proxy with **no default toolchain** (e.g. `apt install rustup`) | Install rustup and `rustup default stable`. Verify with `cargo --version` (no `sudo`) |
| `sudo rustup: command not found` but the build still fails on rustup | Your rustup is a **user-level** install (`~/.cargo/bin`), not on root's `PATH`; under `sudo` it reads the empty `/root/.rustup` | Current `build.sh` reuses your toolchain automatically (`RUSTUP_HOME=$SUDO_USER/.rustup`). On an old checkout, pass it through: `sudo RUSTUP_HOME="$HOME/.rustup" FEATURES=… ./build.sh vm` |
| `env: 'bash\r': No such file or directory` | Scripts were checked out with CRLF line endings on Windows | Ensure `.gitattributes` is present and re-checkout: `git rm --cached -r . && git reset --hard` |
| Build looks frozen for 20–60 min during `browser` or `desktop` | Compiling V8 (`cos-browser`) and the COSMIC crates — both are genuinely slow and quiet | Wait; see **What to expect** above. Watch progress with `tail -f /tmp/claw-build.log` |

> The rootfs build is **not** resumable feature-by-feature: each run starts
> from a fresh `debootstrap`. After fixing a failure, remove the partial rootfs
> and re-run: `sudo rm -rf build/claw-os-rootfs`.

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

