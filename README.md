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
  <a href="LICENSE"><img alt="License" src="https://img.shields.io/badge/license-Apache--2.0-D22128?style=flat-square"></a>
  <a href="https://github.com/xiaoyu-work/claw-os/releases"><img alt="Release" src="https://img.shields.io/github/v/release/xiaoyu-work/claw-os?include_prereleases&style=flat-square"></a>
</p>

> **Claw OS is under active development; its architecture, APIs, and behavior may change significantly.**

Claw OS is a complete Linux-based environment where the AI agent is a
**system-level layer**, not an application running on top of one. The agent
runs as a privileged system daemon (`clawd`) with direct, scoped access to the
kernel, processes, logs, network, and every installed app — so it can reason
about and act on the whole machine, the way an operator would, instead of being
trapped inside a single app's sandbox.

## Why Claw OS is agent-native

Agent-native is the whole bet: the next operating system is one where the
assistant is not a tab you open but the layer the system runs through. Here
that means:

- **The AI is part of the system, not an app on top of it.** It runs as a
  privileged system service, not in a sandbox, so you can just ask the things
  an app could never reach — *"why is my network so slow?"*, *"why did that app
  just crash?"*, *"why can't I get online?"*, *"what's eating my disk?"* — and
  it actually looks at your processes, logs, network, and resources to answer.
- **It understands every app.** The agent can inspect which apps are installed
  and used, what permissions they have, what resources they can access, their
  activity and history, their logs, and the evidence behind a crash. It can
  connect that app context with the rest of the system instead of diagnosing
  each app in isolation.
- **One agent across every app.** Instead of a separate, siloed assistant inside
  each app, a single agent spans the whole machine — it can drive several apps
  in one request (read a document, draft an email, post a notification) and
  reasons from real system state, not just one app's view.
- **It remembers.** The agent has persistent memory across conversations: what
  you did in your apps, what changed on the system, your preferences, and
  context that carries from one app to the next — so you never re-explain
  yourself. It's your memory; you can review it and forget any of it, down to a
  single app.
- **Every app can use the system's models.** Apps call the built-in model layer
  through the Claw OS SDK, whether the model runs locally or through a configured
  cloud provider. Developers can build AI-powered apps without embedding a
  model or rebuilding provider integration, credentials, consent, budgets,
  safety, and logging.
- **Local-first.** The system's intelligence belongs on your own machine, not
  someone else's servers. As edge hardware (CPU / GPU / NPU) matures, fully
  on-device "edge AI" becomes the default; today it runs local models where your
  hardware can handle it and falls back to cloud AI APIs where it can't —
  without changing how you use it.
- **Apps open up to the agent.** Every app publishes what it can do and the
  permissions it needs through the [`claw-os-sdk`](claw-os-sdk/) (Rust, Python,
  Node, Go). The agent can discover and call those APIs, then combine operations
  from multiple apps into complex workflows.
- **Safe by construction.** Every privileged action goes through a capability
  check and your approval, so "system-level" never means "unrestricted" —
  looking, changing, and touching the kernel are each gated separately.
  Primitives return structured, machine-readable data instead of scraped UI,
  and setting up, asking, chatting, and diagnosing are first-class parts of the
  OS.

Together, the system-level agent, persistent memory, local-first model runtime,
scoped permission model, cross-app session store, audit trail, and app SDK form
one integrated agent-native OS layer.

## Quick Start

All artifacts share the same composed Debian rootfs; platform profiles only
add their user, boot, and provisioning policy. See
[Image architecture](docs/image-architecture.md).

Pick an entry point:

<details>
<summary><strong>Ubuntu — install the Agent layer without replacing Ubuntu</strong></summary>

Install the signed `claw-os-agent` package on an existing Ubuntu system:

```bash
sudo install -d -m 0755 /usr/share/keyrings

curl -fsSL https://xiaoyu-work.github.io/claw-os/claw-os-archive-keyring.gpg \
  | sudo tee /usr/share/keyrings/claw-os-archive-keyring.gpg >/dev/null

ARCH="$(dpkg --print-architecture)"
echo "deb [arch=$ARCH signed-by=/usr/share/keyrings/claw-os-archive-keyring.gpg] https://xiaoyu-work.github.io/claw-os trixie main" \
  | sudo tee /etc/apt/sources.list.d/claw-os.list >/dev/null

sudo apt update
sudo apt install claw-os-agent
```

Verify the daemon:

```bash
systemctl is-enabled clawd.service
systemctl is-active clawd.service
sudo systemctl status clawd.service --no-pager
```

</details>

<details>
<summary><strong>WSL — recommended</strong></summary>

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

</details>

<details>
<summary><strong>Docker / OrbStack — recommended</strong></summary>

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

</details>

<details>
<summary><strong>Desktop / ISO / VM — experimental</strong></summary>

Build a bootable desktop VM disk image and load it in VMware — see
[Building the Claw OS Desktop Image](docs/building-desktop.md) for the steps on
Windows (WSL2), macOS, and Linux.

</details>

<details>
<summary><strong>Azure Compute Gallery — generalized fixed VHD</strong></summary>

Build a generalized Hyper-V Generation 2 fixed VHD:

```bash
sudo ./build.sh azure
```

Azure supplies the administrator and SSH key for each VM through cloud-init.
See [Building an Azure Compute Gallery image](docs/building-azure.md) for
desktop builds and publishing commands.

</details>

<details>
<summary><strong>Updating an existing installation</strong></summary>

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

</details>

### Drive the OS with the agent

Call structured system primitives directly, or ask the built-in agent to choose
and combine them:

```bash
cos  # list primitives
cos app web read https://example.com  # fetch URL → {url, title, text, links}
cos agent setup text  # configure the conversational text model
cos agent ask "why is my network so slow right now?"
cos agent ask "why did my last app crash?"
cos agent chat  # interactive REPL with cross-app memory
```

## License

Apache-2.0 for original Claw OS components. See the [`NOTICE`](NOTICE) for
vendored sources and their upstream licenses.
