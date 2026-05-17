# Claw OS rootfs features

This directory contains composable units that `rootfs/build.sh` applies to
the bootstrapped Debian rootfs to produce different distribution flavours.
A feature is a directory with up to two files:

| File | Purpose |
|---|---|
| `packages.txt` | apt packages installed in the chroot (one per line, `#` for comments) |
| `install.sh` | Arbitrary shell run on the **host** with `$ROOTFS`, `$PROJECT_DIR`, `$COS_VERSION` set |

Both are optional. `packages.txt` runs first, then `install.sh`.

## Available features

| Feature | What it adds |
|---|---|
| `base` | Core CLI tools, Node.js 24 (+ pnpm/typescript/tsx), Python apt packages, runtime dirs, `/etc/cos/profile.sh` sourcing, version injection |
| `cos-core` | The `cos` binary, `apps/`, `skills/` |
| `browser` | Chromium runtime libs, the `cos-browser` and (optional) `cos-browser-worker` binaries |
| `desktop` | Wayland desktop stack (cosmic-comp + greeter + panel + launcher + settings + apps) built from the vendored monorepo at `<repo>/desktop/`. Wires `cosmic-greeter` as the display manager and sets `graphical.target` as default. Set `DESKTOP_SKIP=1` to install runtime deps only (skip the ~30–60 min cargo build). |
| `claw-mail-ai` | Packs the `extensions/claw-mail-ai` MailExtension as an `.xpi`, force-installs it into Thunderbird, deploys the Python Native Messaging host (`apps/mail-ai`) under `/usr/lib/cos/mail-ai`, and drops the NM manifest + policies. Requires Thunderbird (already pulled in by `desktop`). |
| `copilot-cli` | Installs `@github/copilot` globally via npm so `copilot` is on every user's `$PATH`. Used by cosmic-term's `@`-trigger AI integration (`desktop/term/src/ai/`). |
| `vmware` | Optional VMware Tools guest integration (`open-vm-tools`, `open-vm-tools-desktop`) for VMware Fusion / Workstation / ESXi images. Include only for VMware builds, after `systemd`. |

Default feature set (when no `--features` is given): `base,cos-core,browser`.
Docker and WSL use the headless Claw OS runtime feature set:
`base,cos-core,browser,systemd,apt-source`. This is the full non-desktop OS
surface: Claw's own `cos`/`clawd` agent runtime, apps, skills, browser
automation, service units, and upgrade source. In every system target, systemd
starts `clawd.service` as part of boot. Target-specific boot/install features
(`kernel`, `grub-disk`, `vm`, `vmware`, `live`, `installer`), desktop UI, and
third-party agent providers (`copilot-cli`) are opt-in.

## Usage

```bash
# Default — same as the legacy invocation
sudo ./rootfs/build.sh

# Headless rootfs, no browser engine
sudo ./rootfs/build.sh --features base,cos-core

# Headless rootfs, no cos at all (just Debian + Node)
sudo ./rootfs/build.sh --features base

# Desktop VMware image rootfs with VMware Tools guest integration
sudo ./rootfs/build.sh --features base,cos-core,systemd,kernel,desktop,vmware,grub-disk,vm,apt-source
```

## Adding a new feature

1. Create `rootfs/features/<your-feature>/`.
2. Drop a `packages.txt` and/or executable `install.sh`.
3. Use it from a target build script via `--features`.

`install.sh` runs on the **host** (not in chroot) so it can `cp` from the
project dir, `sed` overlay files, etc. Use `chroot "$ROOTFS" ...` when you
need to execute commands inside the rootfs.
