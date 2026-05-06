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

> Other distribution targets (`iso-live`, `iso-installer`, `vm`) are
> being added in subsequent milestones — see `targets/` for the current set.

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
