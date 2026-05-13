# Contributing to Claw OS

## Building from Source

### Prerequisites

- Linux (or WSL2 on Windows)
- Rust 1.94+
- Python 3
- Docker (for building the image)
- Root access (for rootfs bootstrap)

### Build the Rust Core

```bash
cd core
cargo build --release
```

### Build the Rootfs + Docker Image

```bash
# Bootstrap Debian rootfs, install Node.js 24, apps, browser engine
sudo ./rootfs/build.sh

# Build the Docker image (base profile)
./build.sh docker

# Build a profile variant (openclaw, deerflow, ironclaw)
PROFILE=openclaw ./build.sh docker

# Or via cos-ctl (equivalent to base profile)
./cli/cos-ctl build
```

### Build a WSL2 Tarball

```bash
# Produces build/claw-os-wsl-amd64.tar.gz
sudo ./build.sh wsl

# On Windows: import + launch
wsl --import claw-os C:\WSL\claw-os build\claw-os-wsl-amd64.tar.gz --version 2
wsl -d claw-os
# Or run the helper:
.\targets\wsl\install.ps1
```

### Build a Live ISO

```bash
# Host requirements (Debian/Ubuntu):
sudo apt install squashfs-tools xorriso grub-pc-bin grub-efi-amd64-bin \
                 grub-common mtools

# Produces build/claw-os-live-amd64.iso (hybrid BIOS+UEFI bootable)
sudo ./build.sh iso-live

# Boot in QEMU (BIOS):
qemu-system-x86_64 -m 2G -cdrom build/claw-os-live-amd64.iso -nographic

# Boot in QEMU (UEFI, requires ovmf package):
qemu-system-x86_64 -m 2G -bios /usr/share/ovmf/OVMF.fd \
                   -cdrom build/claw-os-live-amd64.iso
```

### Build a VM Disk Image

```bash
# Host requirements (Debian/Ubuntu):
sudo apt install qemu-utils parted dosfstools rsync

# Produces build/claw-os-vm.qcow2 (hybrid BIOS+UEFI bootable)
sudo ./build.sh vm

# Multiple formats in one build:
sudo FORMATS="qcow2 vmdk vhdx" ./build.sh vm

# Larger disk:
sudo SIZE=16G ./build.sh vm

# Boot in QEMU (BIOS):
qemu-system-x86_64 -m 2G -nographic \
    -drive file=build/claw-os-vm.qcow2,format=qcow2,if=virtio

# Boot in Hyper-V Gen 2 (UEFI): disable Secure Boot first
#   Set-VMFirmware -VMName claw-os -EnableSecureBoot Off
```

### Build an Installable ISO

```bash
# Host requirements (same as iso-live):
sudo apt install squashfs-tools xorriso grub-pc-bin grub-efi-amd64-bin \
                 grub-common mtools

# Produces build/claw-os-installer-amd64.iso (hybrid BIOS+UEFI bootable).
# This ISO boots into a kiosk-mode Calamares installer that copies the
# live system to a real disk and configures GRUB on the target.
sudo ./build.sh iso-installer

# Test in QEMU with an empty target disk:
qemu-img create -f qcow2 build/claw-os-target.qcow2 16G
qemu-system-x86_64 -m 4G \
    -cdrom build/claw-os-installer-amd64.iso \
    -drive file=build/claw-os-target.qcow2,format=qcow2,if=virtio \
    -boot d
```

The installed system has the Claw OS apt repository pre-configured, so
`sudo apt update && sudo apt upgrade` will pull newer `cos` releases.

### `apt` repository

The `claw-os-base`, `claw-os-browser`, and `claw-os-systemd` `.deb`
packages are built as part of every rootfs build and uploaded as CI
artifacts. On pushes to `main`, the matching apt repository is published
to GitHub Pages and consumable as:

```bash
echo "deb [trusted=yes] https://xiaoyu-work.github.io/claw-os trixie main" \
  | sudo tee /etc/apt/sources.list.d/claw-os.list
sudo apt update
sudo apt install claw-os-base
```

For details see `packaging/README.md`.

### Run Locally (Development)

```bash
# Point cos to local apps directory
COS_APPS_DIR=./apps COS_DATA_DIR=/tmp/cos-data ./core/target/debug/cos sys info
COS_APPS_DIR=./apps COS_DATA_DIR=/tmp/cos-data ./core/target/debug/cos fs ls .
```

### Run Tests

```bash
cd core && cargo test
python -m pytest tests/
```

### Project Structure

```
claw-os/
├── core/              Rust binary (cos)
│   └── src/
│       ├── main.rs        Entry point
│       ├── router.rs      Command dispatch
│       ├── sandbox.rs     Namespace + cgroup isolation
│       ├── proc.rs        Process session manager
│       ├── browser.rs     cos-browser (Obscura) lifecycle
│       ├── bridge.rs      Python app subprocess bridge
│       ├── audit.rs       JSONL audit logging
│       ├── sysinfo.rs     Native system info
│       └── apps.rs        App manifest discovery
├── apps/              Python apps (fs, web, db, doc, etc.)
├── rootfs/            Linux rootfs build scripts + overlay
├── targets/           Per-distribution build scripts (docker, wsl, iso, vm)
│   └── docker/          Dockerfiles + build.sh for the docker target
├── build.sh           Top-level dispatcher (./build.sh <target>)
├── cli/               cos-ctl management tool
├── clients/           Bridge (LLM ↔ Claw OS)
└── tests/             Integration tests
```
