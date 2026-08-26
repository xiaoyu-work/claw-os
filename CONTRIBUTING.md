# Contributing to Claw OS

Coding agents and contributors should read [`AGENTS.md`](AGENTS.md) for task
routing and authoritative validation commands, then
[`ARCHITECTURE.md`](ARCHITECTURE.md) for component boundaries and data flows.

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

# Build the Docker image
./build.sh docker

# Or via cos-ctl
./cli/cos-ctl build
```

### Build a WSL2 Distribution

```bash
# Produces build/claw-os-wsl-amd64.wsl
sudo ./build.sh wsl
```

On Windows, import and launch from PowerShell:

```powershell
wsl --install --from-file .\build\claw-os-wsl-amd64.wsl `
    --name claw-os --location C:\WSL\claw-os --version 2

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
sudo apt install qemu-utils parted dosfstools mtools rsync

# Produces build/claw-os-vm-amd64.qcow2 (hybrid BIOS+UEFI bootable)
sudo ./build.sh vm

# Multiple formats in one build:
sudo FORMATS="qcow2 vmdk vhdx" ./build.sh vm

# VMware Fusion / Workstation build with VMware Tools guest integration
# for guest resize / clipboard (preset wrapper sets the desktop features):
sudo ./presets/desktop.sh
# ...or spell out the equivalent FEATURES manually:
sudo FEATURES=base,cos-core,systemd,kernel,desktop,vmware,copilot-cli,grub-disk,vm,apt-source,local-user \
     FORMATS=vmdk ./build.sh vm

# Larger disk:
sudo SIZE=16G ./build.sh vm

# Boot in QEMU (BIOS):
qemu-system-x86_64 -m 2G -nographic \
    -drive file=build/claw-os-vm-amd64.qcow2,format=qcow2,if=virtio

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
`sudo apt update && sudo apt full-upgrade` will pull newer Claw OS packages.
See [`docs/updating.md`](docs/updating.md) for the unified update path across
installed targets.

### `apt` repository

The reusable `claw-os-agent` and Claw OS integration `claw-os-base` `.deb`
packages are assembled from compiled binaries and source-tree files; building
those APT artifacts does not require a rootfs. The optional desktop package is
staged by the desktop rootfs feature. The manually dispatched
**Build APT repo (.deb packages)** workflow builds both architectures, signs the
repository, and publishes it to GitHub Pages. The umbrella **Release
everything (test + Docker + WSL + APT)** workflow includes the same channel.
Desktop `.deb` updates are built by the separately dispatched **Build Desktop
packages** workflow; pass that run ID to either publication workflow.
The repository is consumable as:

```bash
curl -fsSL https://xiaoyu-work.github.io/claw-os/claw-os-archive-keyring.gpg \
  | sudo tee /usr/share/keyrings/claw-os-archive-keyring.gpg >/dev/null
echo "deb [signed-by=/usr/share/keyrings/claw-os-archive-keyring.gpg] https://xiaoyu-work.github.io/claw-os trixie main" \
  | sudo tee /etc/apt/sources.list.d/claw-os.list
sudo apt update
sudo apt install claw-os-agent
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
# Core tests share process-global environment variables.
(cd core && cargo test -- --test-threads=1)

# Exact CI lint.
(cd core && cargo clippy -- -D warnings)

# From the repository root.
PYTHONPATH=claw-os-sdk/python/src:cos-runtime/python/src \
  python3 -m pytest -q apps adapters claw-os-sdk/python/src cos-runtime/python/src

# Browser crate.
cargo test -p cos-browser
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
└── .github/workflows/ Test and publication workflows
```

## Architecture Rules

These rules apply to every subsystem in `core/src/`. They exist so
agent state stays auditable and so cross-machine work (sandbox,
remote provider) does not require forked implementations.

### Capability seam requires all three roles

A *capability seam* is what lets one implementation of a primitive
be swapped for another (e.g. a local-only filesystem provider
replaced by one that transparently proxies to a sandbox or a remote
node). It is not a seam unless all three roles are present:

1. **Service Definition** — the interface. A trait or explicit type
   in a well-known module. Callers depend on this, not on the
   provider. Example: `agent::memory::app_memory`.
2. **Service Provider** — the implementation behind the interface.
   There MAY be more than one (SQLite FTS, vector, remote); the
   interface is what makes them interchangeable. Example:
   `agent::memory::sqlite_fts::MemoryDb` and
   `agent::memory::semantic`.
3. **Consumer** — typically a model-facing `cos_*` tool, or another
   subsystem that goes through the definition. Example: the
   `cos_memory` / `cos_recall` tools.

One role alone does not constitute a seam. A concrete type with no
interface is not a seam (it is an implementation). An interface
with no consumer is speculative. An interface with a single
hard-wired provider that callers reach past is not a seam either.

**Why this matters here.** Claw OS wants to run agent work in a
sandbox or on another machine without forking every subsystem.
That property only holds if filesystem, subprocess, memory, and
network all share the same three-role shape: pointing the provider
layer at a remote target then relocates the whole capability
without touching consumers. Subsystems that skip the definition
(consumer imports the concrete provider) block that entire
property for their capability.

**When adding a new subsystem** (diagnostics, storage, network,
sandbox surface, credential store, …):

- Introduce the Service Definition first, in its own module. Give
  it a name that describes the capability, not the implementation
  (`storage`, not `sqlite_storage`).
- Land at least one Consumer against the Definition in the same
  change. If nothing consumes it yet, the seam is speculative and
  the definition should wait.
- If you only need one provider today, the seam is still worth
  it — the second provider is what pays off the design, and
  retrofitting a seam onto a hard-wired consumer is the expensive
  case this rule is meant to prevent.

### Model-visible content must be logged

Anything that reaches a model request has to be reconstructable
from the session log. Concretely: system-prompt injections
(`MEMORY.md`, `USER.md`, due nudges, per-session extras) are
recorded as `injected` rows in the memory DB by
`agent::prompt::build_system_prompt_traced`. Adding a new kind of
model-visible input requires adding a corresponding record path
*in the same change* — a transcript reader must never have to
guess what the model saw.

### Long-running mutations are three-phase bracketed

Any operation that mutates durable state and can be interrupted by
a crash appends a `start` event *before* the mutation, then the
`end` (`Completed` / `Failed`) event *after* the mutation returns
success. A crash in between leaves an orphan `start` with no
matching `end`, which is detectable and explicitly treated as such
(`agent::memory::curator::CurationLog::orphaned_runs`). Do not
record "the operation finished" before the durable write returns.
