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
| `cos-core` | `claw-os-agent` plus `claw-os-base`: Agent/browser binaries, headless apps, skills, SDKs, and Claw OS integration |
| `browser` | Chromium executable and target runtime integration for the browser binaries already shipped by `claw-os-agent` |
| `qwen3-embedding` | Downloads the public Hugging Face `johnucm/Qwen-Qwen3-Embedding-0.6B-onnx` ONNX GenAI bundle into `/var/lib/cos/models/qwen3-embedding-0.6b/v1` and installs the pinned `ort-genai` runtime where upstream provides a Linux asset |
| `desktop` | Builds the vendored desktop workspace into `claw-os-desktop.deb`, then installs it. Includes compositor, greeter, panel, launcher, settings, apps, desktop defaults, and graphical boot wiring. Set `DESKTOP_SKIP=1` to install runtime deps + overlay only. |
| `claw-mail-ai` | Packs the `extensions/claw-mail-ai` MailExtension as an `.xpi`, force-installs it into Thunderbird, deploys the Python Native Messaging host (`apps/mail-ai`) under `/usr/lib/cos/mail-ai`, and drops the NM manifest + policies. Requires Thunderbird (already pulled in by `desktop`). |
| `copilot-cli` | Installs `@github/copilot` globally via npm so `copilot` is on every user's `$PATH`. Used by cosmic-term's `@`-trigger AI integration (`desktop/term/src/ai/`). |
| `systemd` | Claw OS system services plus `systemd-coredump`, so the system agent can diagnose process crashes on desktop and headless images. |
| `vm` | Hypervisor-neutral serial console, GRUB command line, serial getty, and VM power defaults. Does not create a user or install a provider agent. |
| `local-user` | Adds the local `cos` login account when no metadata service can provision one. Skips graphical images that use the desktop first-boot wizard. |
| `cloud-init` | Provider-neutral cloud provisioning, SSH, root filesystem growth, and locked root account. |
| `azure` | Azure datasource policy, WALinuxAgent, Hyper-V daemons/initramfs modules, and Azure serial-console settings. Requires `cloud-init`. |
| `vmware` | Optional VMware Tools guest integration (`open-vm-tools`, `open-vm-tools-desktop`) for VMware Fusion / Workstation / ESXi images. Include only for VMware builds, after `systemd`. |

Default feature set (when no `--features` is given): `base,cos-core,browser`.
Docker and WSL use the headless Claw OS runtime feature set:
`base,cos-core,browser,systemd,gpu-drivers,apt-source,qwen3-embedding`. This is
the full non-desktop OS surface: Claw's own `cos`/`clawd` agent runtime, apps,
skills, browser automation, service units, local embedding stack, and upgrade
source. This feature set builds on both amd64 and arm64 (WSL/Docker arm64 are
Linux targets). In every system target, systemd starts
`clawd.service` as part of boot. The shared rootfs contains no target-specific
human account: WSL creates one through its first-launch OOBE, while Docker
creates the account requested through its runtime environment.
Target-specific boot/install features (`kernel`, `grub-disk`, `vm`,
`local-user`, `cloud-init`, `azure`, `vmware`, `live`, `installer`), desktop
UI, and third-party agent providers
(`copilot-cli`) are opt-in.

Supported feature combinations are centralized in
`scripts/lib/image-profiles.sh`. Features define capabilities; targets define
artifact formats and finalization.

## Usage

```bash
# Default — same as the legacy invocation
sudo ./rootfs/build.sh

# Headless rootfs without a Chromium renderer
sudo ./rootfs/build.sh --features base,cos-core

# Headless rootfs, no cos at all (just Debian + Node)
sudo ./rootfs/build.sh --features base

# Desktop VMware image rootfs with VMware Tools guest integration
sudo ./rootfs/build.sh --features base,cos-core,systemd,kernel,desktop,vmware,grub-disk,vm,apt-source,local-user

# Generalized Azure rootfs (the azure target also performs final cleanup)
sudo ./rootfs/build.sh --features base,cos-core,systemd,kernel,grub-disk,vm,cloud-init,azure
```

## Adding a new feature

1. Create `rootfs/features/<your-feature>/`.
2. Drop a `packages.txt` and/or executable `install.sh`.
3. Use it from a target build script via `--features`.

`install.sh` runs on the **host** (not in chroot) so it can `cp` from the
project dir, `sed` overlay files, etc. Use `chroot "$ROOTFS" ...` when you
need to execute commands inside the rootfs.
