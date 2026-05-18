# Claw OS

<div align="center">
  <h3>The OS for agents.</h3>
  <p>
    Claw OS is a Linux-based environment where agents can use apps, files,
    browser sessions, credentials, jobs, permissions, and rollback through
    structured, auditable OS primitives.
  </p>
  <p>
    <a href="https://xiaoyu-work.github.io/claw-os">Website</a>
    |
    <a href="https://github.com/xiaoyu-work/claw-os/releases/tag/wsl-latest">WSL release</a>
    |
    <a href="https://github.com/xiaoyu-work/claw-os/pkgs/container/claw-os">Container image</a>
  </p>
</div>

## The operating system for agents

Claw OS is not a chatbot bolted onto Linux. It exposes the computer as
explicit contracts that a built-in agent can use safely:

```bash
cos checkpoint create "clean state"
cos app web read https://example.com
cos agent ask "find risky changes and explain rollback options"
```

The result is an agent-native OS surface: structured outputs for apps, scoped
permissions for risky work, audited AI activity, and checkpoints for rollback.

## What it provides

| Capability | What it does |
|---|---|
| **Built-in agent** | `cos agent setup/ask/chat`, desktop overlay, voice input |
| **Structured primitives** | JSON-first commands for apps, files, system info, package search, browser reads, web fetches, notifications, and more |
| **Scoped permissions** | Capability checks and approvals for risky actions |
| **Checkpoints** | Snapshot, diff, and rollback file changes |
| **Credentials and jobs** | Encrypted credential store and agent-native scheduling |
| **AI app boundary** | Apps call models through `cos ai chat` and execute tools through `cos ai tool` |
| **Local inference** | `cos model` and `cos engine` manage on-device runtimes |

## Trust architecture

| Layer | Guarantee |
|---|---|
| **Identity is pinned** | Apps inherit identity from the OS-spawned process tree |
| **Capabilities are checked** | Tools bind to verbs and scopes before any side effect |
| **Activity is audited** | Model calls and tool executions are recorded as structured events |
| **Changes can roll back** | Checkpoints make agent work inspectable and reversible |

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

On the desktop, **Super+A** opens the Spotlight-style agent overlay;
**Super+Shift+A** arms voice input.

## Drive the OS

```bash
cos                                    # list primitives
cos checkpoint create "clean state"    # snapshot the workspace
cos app web read https://example.com   # fetch a page as Markdown
```

## Talk to the agent

```bash
cos agent setup                               # configure providers and credentials
cos agent ask "find the largest files and tell me why"
cos agent chat                                # interactive REPL
```

## APT repository

The public website and APT repository are served from the same GitHub Pages
origin. The homepage lives at `/`, while APT clients continue to fetch
`/dists/...` and `/pool/...` from the same base URL.

```bash
echo "deb [trusted=yes] https://xiaoyu-work.github.io/claw-os trixie main" \
  | sudo tee /etc/apt/sources.list.d/claw-os.list
sudo apt update
sudo apt install claw-os-base
```

## License

MIT for the kernel and apps. See the [`NOTICE`](NOTICE) for vendored sources
and their upstream licenses.
