# Claw OS

<p align="center">
  <img src="assets/brand/clawos-symbol.png" alt="" width="140">
</p>
<p align="center">
  <img src="assets/brand/clawos-wordmark.png" alt="Claw OS" width="240">
</p>

<p align="center"><strong>The first agent-native operating system — where the AI is part of the system, not an app on top of it.</strong></p>

<p align="center">
  <a href="https://xiaoyu-work.github.io/claw-os"><img alt="Website" src="https://img.shields.io/badge/website-xiaoyu--work.github.io%2Fclaw--os-2563eb?style=flat-square"></a>
  <a href="https://github.com/xiaoyu-work/claw-os/actions/workflows/build.yml"><img alt="Build" src="https://img.shields.io/github/actions/workflow/status/xiaoyu-work/claw-os/build.yml?branch=main&style=flat-square"></a>
  <a href="LICENSE"><img alt="License" src="https://img.shields.io/badge/license-MIT-1f7a3a?style=flat-square"></a>
  <a href="https://github.com/xiaoyu-work/claw-os/releases"><img alt="Release" src="https://img.shields.io/github/v/release/xiaoyu-work/claw-os?include_prereleases&style=flat-square"></a>
</p>

Claw OS is a complete Linux-based environment where the AI agent is a **system-level layer**, not an application running on top of one. The agent runs as a privileged system daemon (`clawd`) with direct, scoped access to the kernel, processes, logs, network, and every installed app — so it can reason about and act on the whole machine, the way an operator would, instead of being trapped inside a single app's sandbox.

## Why it is agent-native

Agent-native is the whole bet: the next operating system is one where the assistant is not a tab you open but the layer the system runs through. Here that means:

- **The AI is part of the system, not an app on top of it.** It runs as a privileged system service, not in a sandbox, so you can just ask the things an app could never reach — *"why is my network so slow?"*, *"why did that app just crash?"*, *"why can't I get online?"*, *"what's eating my disk?"* — and it actually looks at your processes, logs, network, and resources to answer.
- **One agent across every app.** Instead of a separate, siloed assistant inside each app, a single agent spans the whole machine — it can drive several apps in one request (read a document, draft an email, post a notification) and reasons from real system state, not just one app's view.
- **It remembers.** The agent has persistent memory across conversations: what you did in your apps, what changed on the system, your preferences, and context that carries from one app to the next — so you never re-explain yourself. It's your memory; you can review it and forget any of it, down to a single app.
- **Local-first.** The system's intelligence belongs on your own machine, not someone else's servers. As edge hardware (CPU / GPU / NPU) matures, fully on-device "edge AI" becomes the default; today it runs local models where your hardware can handle it and falls back to cloud AI APIs where it can't — without changing how you use it.
- **Apps open up to the agent.** Every app publishes what it can do, and the agent decides which to use. Developers expose their app to the system agent through the [`claw-os-sdk`](claw-os-sdk/) (Rust, Python, Node, Go).
- **Safe by construction.** Every privileged action goes through a capability check and your approval, so "system-level" never means "unrestricted" — looking, changing, and touching the kernel are each gated separately. Primitives return structured, machine-readable data instead of scraped UI, and setting up, asking, chatting, and diagnosing are first-class parts of the OS.

What runs today is the foundation for that future: the system-level agent, its persistent memory, the local-first runtime, the scoped permission model, the cross-app session store, and the app SDK are already here.

## Quick Start

Pick an entry point:

- **WSL** — recommended
- **Docker / OrbStack** — recommended
- **Desktop / ISO / VM** — experimental
- **Azure Compute Gallery** — generalized fixed VHD

All artifacts share the same composed Debian rootfs; platform profiles only
add their user, boot, and provisioning policy. See
[Image architecture](docs/image-architecture.md).

### WSL

Import the latest successfully published `main` WSL rootfs (`wsl-latest`,
rolling pre-release):

```powershell
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "arm64" } else { "amd64" }
$tarball = "claw-os-wsl-$arch.tar.gz"
$url = "https://github.com/xiaoyu-work/claw-os/releases/download/wsl-latest/$tarball"

curl.exe -L --fail --retry 5 --retry-delay 3 -C - --output $tarball $url
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

Build a bootable desktop VM disk image and load it in VMware — see
[Building the Claw OS Desktop Image](docs/building-desktop.md) for the steps on
Windows (WSL2), macOS, and Linux.

### Azure Compute Gallery

Build a generalized Hyper-V Generation 2 fixed VHD:

```bash
sudo ./build.sh azure
```

Azure supplies the administrator and SSH key for each VM through cloud-init.
See [Building an Azure Compute Gallery image](docs/building-azure.md) for
desktop builds and publishing commands.

### Drive the OS

```bash
cos                                    # list primitives
cos app web read https://example.com   # fetch URL → {url, title, text, links}
```

### Talk to the agent

```bash
cos agent setup                               # configure providers and credentials
cos agent ask "why is my network so slow right now?"
cos agent ask "why did my last app crash?"
cos agent chat                                 # interactive REPL with cross-app memory
```

## License

MIT for the kernel and apps. See the [`NOTICE`](NOTICE) for vendored sources and their upstream licenses.
