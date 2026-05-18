# Claw OS

<p align="center">
  <img src="assets/brand/clawos-wordmark.png" alt="Claw OS" width="220">
</p>

The First Agent Native Operating System

Website: https://xiaoyu-work.github.io/claw-os

Claw OS is a Linux-based environment where apps, files, browser access, credentials, jobs, permissions, and rollback are exposed as structured `cos` primitives and controlled by a built-in agent (`Claw`).

## What it provides

| Capability | What it does |
|---|---|
| **Built-in agent** | `cos agent setup/ask/chat`, desktop overlay, voice input |
| **Structured primitives** | JSON-first commands for apps, files, system info, and browser reads |
| **Scoped permissions** | Capability checks and approvals for risky actions |
| **Checkpoints** | Snapshot, diff, and rollback file changes |
| **Credentials and jobs** | Encrypted credential store and agent-native scheduling |
| **Local inference** | `cos model` and `cos engine` manage on-device runtimes |

## Quick Start

Pick an entry point:

| Target | Status |
|---|---|
| **WSL** | Recommended |
| **Docker / OrbStack** | Recommended |
| **Desktop / ISO / VM** | Experimental |

### WSL

Import the latest WSL rootfs:

```powershell
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "arm64" } else { "amd64" }
$tarball = "claw-os-wsl-$arch.tar.gz"
$url = "https://github.com/xiaoyu-work/claw-os/releases/download/wsl-latest/$tarball"

Invoke-WebRequest $url -OutFile $tarball
wsl --import claw-os C:\WSL\claw-os .\$tarball --version 2
wsl -d claw-os
```

### Docker

Run the container:

```bash
docker pull ghcr.io/xiaoyu-work/claw-os:latest
docker run -d --name claw --privileged -v ./workspace:/home/cos/workspace ghcr.io/xiaoyu-work/claw-os
docker exec -it --user cos claw bash --login
```

### Desktop / ISO / VM

Desktop images are **experimental**.

### Drive the OS

```bash
cos                                    # list primitives
cos checkpoint create "clean state"    # snapshot the workspace
cos app web read https://example.com   # fetch a page as Markdown
```

### Talk to the agent

```bash
cos agent setup                               # configure providers and credentials
cos agent ask "find the largest files and tell me why"
cos agent chat                                 # interactive REPL
```

On the desktop, **Super+A** opens the Spotlight-style agent overlay; **Super+Shift+A** arms voice input.

## License

MIT for the kernel and apps. See the [`NOTICE`](NOTICE) for vendored sources and their upstream licenses.
