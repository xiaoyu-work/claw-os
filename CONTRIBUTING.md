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
# Bootstrap Debian rootfs, install Node.js 24, apps, Jina Reader
sudo ./rootfs/build.sh

# Build the Docker image
./cli/cos-ctl build
```

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
│       ├── browser.rs     Jina Reader lifecycle
│       ├── bridge.rs      Python app subprocess bridge
│       ├── audit.rs       JSONL audit logging
│       ├── sysinfo.rs     Native system info
│       └── apps.rs        App manifest discovery
├── apps/              Python apps (fs, web, db, doc, etc.)
├── rootfs/            Linux rootfs build scripts + overlay
├── docker/            Dockerfile
├── cli/               cos-ctl management tool
├── clients/           Bridge (LLM ↔ Claw OS)
└── tests/             Integration tests
```
