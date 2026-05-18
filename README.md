# Claw OS

<p align="center">
  <img src="assets/brand/clawos-wordmark.png" alt="Claw OS" width="220">
</p>

<p align="center"><strong>The first agent native operating system.</strong></p>

<p align="center">
  <a href="https://xiaoyu-work.github.io/claw-os"><img alt="Website" src="https://img.shields.io/badge/website-xiaoyu--work.github.io%2Fclaw--os-2563eb?style=flat-square"></a>
  <a href="https://github.com/xiaoyu-work/claw-os/actions/workflows/build.yml"><img alt="Build" src="https://img.shields.io/github/actions/workflow/status/xiaoyu-work/claw-os/build.yml?branch=main&style=flat-square"></a>
  <a href="LICENSE"><img alt="License" src="https://img.shields.io/badge/license-MIT-1f7a3a?style=flat-square"></a>
  <a href="https://github.com/xiaoyu-work/claw-os/releases"><img alt="Release" src="https://img.shields.io/github/v/release/xiaoyu-work/claw-os?include_prereleases&style=flat-square"></a>
</p>

Claw OS is a complete Linux-based environment designed for AI agents. It keeps the normal Linux system visible while adding structured OS primitives, scoped permissions, credentials, jobs, and local model runtime management through `cos`.

## Why it is agent-native

Claw OS is built around the idea that an agent should not control a computer through fragile shell guesses alone. It gives the agent explicit operating-system interfaces:

- **Structured primitives** — apps, files, browser reads, system info, package search, credentials, jobs, and model runtimes are exposed through predictable `cos` commands.
- **Machine-readable results** — primitives return structured output so an agent can inspect state without scraping human UI.
- **Scoped permissions** — risky actions go through capability checks and approvals instead of granting broad access by default.
- **Built-in agent entry points** — `cos agent setup`, `cos agent ask`, and `cos agent chat` are first-class OS commands.
- **Local runtime support** — `cos model` and `cos engine` manage on-device inference where available.

## Quick Start

Pick an entry point:

- **WSL** — recommended
- **Docker / OrbStack** — recommended
- **Desktop / ISO / VM** — experimental

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
cos app web read https://example.com   # fetch a page as Markdown
```

### Talk to the agent

```bash
cos agent setup                               # configure providers and credentials
cos agent ask "find the largest files and tell me why"
cos agent chat                                 # interactive REPL
```

## License

MIT for the kernel and apps. See the [`NOTICE`](NOTICE) for vendored sources and their upstream licenses.
