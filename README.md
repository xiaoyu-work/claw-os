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

## AI is part of the OS, not an app

Every AI assistant today is an *app*: it lives in its own window, sees only its own context, and has no real access to the system. Claw OS inverts that. The agent ships as a first-class OS service with operating-system privileges, so you can just ask it things that an app simply cannot reach:

- **"Why is my network so slow?"** — it looks at live throughput, connections, and routes for you.
- **"Why did that app just crash?"** — it inspects crash dumps, failed services, and system logs.
- **"Why can't I get online?"** — it checks interfaces, DNS, and connectivity from inside the system, not from a sandbox.
- **"What's eating my disk / CPU / memory?"** — it reads the real resource state of the machine.

Because the agent sits *below* the apps rather than beside them, understanding the whole system is a built-in capability, not a plugin.

## One agent across every app

Today every agent is siloed inside one app, with its own disconnected session. The Claw OS system agent is shared across the whole machine:

- **Cross-app context & session** — one persistent memory follows you across apps and conversations instead of resetting every time you switch.
- **Cross-app orchestration** — the agent knows about every installed app and can drive several of them in a single request, so one ask can read a document, draft an email, and post a notification at once.
- **System context, not just app context** — its answers are grounded in the real state of your machine, not only what one app can see.
- **Apps open their capabilities to the system agent** — each app publishes what it can do, and the agent decides which app to use. Developers expose their app's functionality to the system agent through the [`claw-os-sdk`](claw-os-sdk/) (Rust, Python, Node, Go). The future is every app publishing its functions to one shared agent — that integration is the app developer's job, and Claw OS gives them the contract for it.

## It remembers

The agent has real, persistent memory — it doesn't forget the moment a conversation ends. Over time it builds up a picture of you and your machine:

- **App activity** — what you did in your apps, so later you can ask "what did I do in my email this morning?" without reopening anything.
- **System changes** — what was installed, configured, or changed on the system, so the agent can explain how your machine got into its current state.
- **Cross-app sessions** — context carries across apps and conversations, so you never have to re-explain yourself when you switch tools.
- **Your preferences** — how you like things done, remembered and applied automatically.

It's your memory: you can review what the agent has remembered and forget any of it — down to a single app — whenever you want.

## Local-first AI

The system agent should run **entirely on your own machine**. An assistant that lives at the system level — reading your processes, logs, files, and network — has no business shipping that off to someone else's servers. The right place for the OS's intelligence is the device it runs on.

We believe advances in CPUs, GPUs, and NPUs will make fully on-device "edge AI" the default. We're not all the way there yet, so today Claw OS runs local models and inference engines where your hardware can handle it, and falls back to cloud AI API calls where it can't. As edge hardware catches up, the balance shifts toward fully local — without changing how you use the agent.

## Why it is agent-native

To make a system-level agent safe and reliable, Claw OS gives it explicit operating-system interfaces instead of fragile shell guesses:

- **Structured primitives** — system info, services, files, processes, browser reads, package management, credentials, scheduled jobs, and model runtimes are exposed through predictable interfaces.
- **Machine-readable results** — primitives return structured data so the agent can inspect state without scraping a human UI.
- **Scoped permissions** — every privileged action goes through a capability check and your approval, so "system-level" never means "unrestricted." Just looking at something, changing it, and touching the kernel are each gated separately.
- **Built-in agent entry points** — setting up, asking, chatting, and diagnosing are first-class parts of the OS, not a separate app you install.
- **Local runtime support** — on-device models and inference engines are managed for you where available.

## Vision

Claw OS is a bet on the next operating system: one where the assistant is not a tab you open but the layer the whole system runs through. The agent should hold context across every app, remember what you and your machine have done, reason from real system state, orchestrate apps on your behalf, and run entirely on your own hardware — while every app, in turn, publishes its functions to that one shared agent. What runs today is the foundation for that future; the system-level agent, its persistent memory, the local-first runtime, the scoped permission model, the cross-app session store, and the app SDK are already here.

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

Build a bootable desktop VM disk image and load it in VMware — see
[Building the Claw OS Desktop Image](docs/building-desktop.md) for the steps on
Windows (WSL2), macOS, and Linux.

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
