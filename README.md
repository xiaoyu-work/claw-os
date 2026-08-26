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
  <a href="https://github.com/xiaoyu-work/claw-os/actions/workflows/release.yml"><img alt="Release pipeline" src="https://img.shields.io/github/actions/workflow/status/xiaoyu-work/claw-os/release.yml?branch=main&style=flat-square"></a>
  <a href="LICENSE"><img alt="License" src="https://img.shields.io/badge/license-MIT-1f7a3a?style=flat-square"></a>
  <a href="https://github.com/xiaoyu-work/claw-os/releases"><img alt="Release" src="https://img.shields.io/github/v/release/xiaoyu-work/claw-os?include_prereleases&style=flat-square"></a>
</p>

Claw OS is a Linux-based environment where AI is a **system-level layer**, not
an assistant added separately to each app. One built-in agent and one shared
model runtime connect the user, system, and every app through secure,
structured APIs.

## Why Claw OS is agent-native

Agent-native is the whole idea: AI is not a tab you open or a feature owned by
one app. It is a shared OS layer that understands the machine, provides
intelligence to apps, and uses apps as tools.

- **The agent is built into the OS.** It runs through the privileged `clawd`
  service and structured system tools, giving it scoped access to processes,
  storage, services, logs, networks, devices, and installed apps.
- **It can explain the whole system.** The agent can connect app activity,
  permissions, accessible resources, history, logs, crash evidence, and current
  system state to answer questions such as *"why did this app crash?"*, *"what
  can it access?"*, or *"what is using my network?"*
- **One agent works across every app.** Instead of separate assistants trapped
  in separate sandboxes, the built-in agent carries context and persistent
  memory across the machine. Users can inspect and forget that memory.
- **One model runtime serves every app.** Apps call local or configured cloud
  models through the Claw OS SDK instead of embedding a model or rebuilding
  provider, credential, consent, budget, and logging infrastructure.
- **Any developer can build AI apps.** Developers focus on the app and call the
  system AI API; Claw OS owns model selection, execution, safety, and usage
  records.
- **Apps open up to the agent.** Each app can publish typed operations and
  permission requirements through the [`claw-os-sdk`](claw-os-sdk/), allowing
  the system agent to discover and call its API.
- **Cross-app workflows are native.** The agent can combine operations from
  multiple apps into one workflow — gathering information, transforming it,
  taking action, and returning a single result.
- **System-level power remains user-controlled.** Local-first model execution,
  capabilities, approvals, scoped permissions, memory controls, and
  session/audit history keep privileged access visible and revocable.

## Quick Start

Pick an entry point:

- **WSL** — recommended
- **Docker / OrbStack** — recommended
- **Desktop / ISO / VM** — experimental
- **Azure Compute Gallery** — generalized fixed VHD

All artifacts share the same composed Debian rootfs; platform profiles only
add their user, boot, and provisioning policy. See
[Image architecture](docs/image-architecture.md).

### Updating an existing installation

Package-level fixes to `cos`, `clawd`, bundled apps, browser automation, and
systemd units do **not** require reinstalling the operating system or replacing
its image. Claw OS uses the same signed APT update path across WSL, installed
desktop/VM systems, Azure instances, and long-running containers:

```bash
sudo apt update
sudo apt full-upgrade
```

See [Updating Claw OS](docs/updating.md) for repository checks, setup for older
images, service restart behavior, container guidance, and the exceptional cases
that require a new image.

### WSL

Install the latest successfully published `main` WSL distribution (`wsl-latest`,
rolling pre-release):

```powershell
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "arm64" } else { "amd64" }
$package = "claw-os-wsl-$arch.wsl"
$url = "https://github.com/xiaoyu-work/claw-os/releases/download/wsl-latest/$package"
$installPath = "C:\WSL\claw-os"

curl.exe -L --fail --retry 5 --retry-delay 3 -C - --output $package $url
wsl --install --from-file ".\$package" --name claw-os --location $installPath --version 2
```

### Docker

Run the container:

```bash
docker pull ghcr.io/xiaoyu-work/claw-os:latest
mkdir -p ./workspace
docker run -d --name claw --privileged \
  -e CLAW_USER=yourname \
  -v ./workspace:/workspace \
  ghcr.io/xiaoyu-work/claw-os:latest
docker exec -it claw /usr/local/sbin/claw-container-entrypoint shell
```

Replace `yourname` with the UNIX username to create. The default UID/GID is
1000; set `CLAW_UID` and `CLAW_GID` when a Linux host needs numeric ownership
to match a bind mount.

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

### Connect Discord or Telegram

The same system agent can receive and answer allowlisted Discord DMs, server
mentions, threads, and Telegram chats:

```bash
cos credential store discord_bot_token "<bot-token>"
cos app gateway-discord configure \
  "users=<discord-user-id> guilds=<discord-server-id> require_mention=true"
cos app gateway-discord start
```

See [External communications](docs/external-communications.md) for Discord bot
permissions, persistent service setup, Telegram, security policy, and the
current platform support matrix.

## License

MIT for the kernel and apps. See the [`NOTICE`](NOTICE) for vendored sources and their upstream licenses.
