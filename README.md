# Claw OS

The OS for agents.

Linux, macOS, and Windows were designed for humans — they return pixels, terminal text, and GUI windows. Claw OS was designed for agents — every system call returns structured data, every process is tracked by session, every operation is automatically audited, and an agent (`Claw`) is part of the OS.

## Beyond Linux

Claw OS provides primitives that traditional operating systems don't:

| Capability | Linux | Claw OS |
|---|---|---|
| **Built-in agent** | None | `cos agent ask/chat/setup` — local-first, LLM-pluggable, Spotlight-style overlay (Super+A), voice input (Super+Shift+A) |
| **Structured I/O** | Text stdout | JSON from every command |
| **Checkpoint / Rollback** | None | OverlayFS — snapshot, diff, undo any file changes |
| **Permission Model** | uid/rwx (for humans) | Capability system — verb + scope (file/host/app/...) with risk-tiered roles |
| **Interactive Consent** | sudo (binary yes/no) | Approval queue — gated ops surface to a panel applet and `cos perms approve/deny/ask` |
| **Process Coordination** | Raw pipes, signals | IPC messages, locks, barriers, streaming named pipes |
| **Process Hierarchy** | PIDs, process groups | Session IDs, named groups, parent-child context inheritance |
| **Error Recovery** | "Permission denied" | Structured JSON with recovery commands to try |
| **Service Management** | systemd (complex) | Lifecycle hooks, graceful drain, dependency-ordered shutdown |
| **Browser** | Not included | Built-in Chromium engine — URL → Markdown in one call |
| **Audit** | Optional, complex | Every operation logged automatically |
| **Credential Management** | env vars, plaintext files | AES-256-GCM encrypted store with namespaces, TTL, and bundles |
| **Job Scheduling** | crond (no context) | Agent-native cron with tier/scope/credential context, overlap protection |
| **Event System** | inotify (raw events) | Multi-source aggregation (file + proc + service), event history |
| **Skills** | None | Markdown recipes the agent loads on demand; ships kernel-primitive references in `skills/claw-os/` |
| **Local Inference** | None | `cos model` + `cos engine` manage on-device LLM runtimes (llama.cpp, vllm, …) |

## Quick Start

Recommended entry points today:

| Target | Use when | Status |
|---|---|---|
| **WSL** | Windows users who want a full headless Claw OS shell | Recommended |
| **Docker / OrbStack** | macOS/Linux users who want the headless Claw OS runtime in a container | Recommended |
| **Desktop / ISO / VM** | Testing the graphical agent desktop environment | Experimental |

Both recommended headless targets boot systemd and start `clawd.service`
automatically. You should not run `clawd` by hand; if the system is up, the
system agent should already be up.

### WSL

Download the latest WSL rootfs matching your Windows CPU architecture, import it,
then enter the distro:

```powershell
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "arm64" } else { "amd64" }
$tarball = "claw-os-wsl-$arch.tar.gz"
$url = "https://github.com/xiaoyu-work/claw-os/releases/download/wsl-latest/$tarball"

Invoke-WebRequest $url -OutFile $tarball
wsl --import claw-os C:\WSL\claw-os .\$tarball --version 2
wsl -d claw-os
```

### Docker

The Docker image is the **headless Claw OS** runtime — the full non-desktop OS
surface with Claw's own `cos`/`clawd` agent runtime, built-in apps, skills,
browser automation, systemd units, and apt upgrade source. It does not bake in
desktop UI, installer/boot/VM-only features, or third-party agents. The default
user is `cos` (uid 1000, NOPASSWD sudo). The container boots systemd, so
`clawd.service` starts through the same system path used by WSL and VM images.
The published image is multi-arch (`linux/amd64` and `linux/arm64`), so
Docker/OrbStack on Apple Silicon pulls the native arm64 image automatically.

```bash
docker pull ghcr.io/xiaoyu-work/claw-os:latest
docker run -d --name claw --privileged -v ./workspace:/home/cos/workspace ghcr.io/xiaoyu-work/claw-os
docker exec -it --user cos claw bash --login
```

### Desktop / ISO / VM

The desktop environment is in **experimental development**. The target direction
is the same system agent model as headless Claw OS, with desktop context,
timeline, memory, and permission surfaces layered on top. For now, use WSL or
Docker to validate the headless system agent runtime first.

Other targets — bootable ISO (live + installer), VM images, WSL image, and
`.deb` + apt repo — are produced from `packaging/`. See
[CONTRIBUTING.md](CONTRIBUTING.md).

### Drive the OS

```bash
cos                                    # list primitives
cos app                                # list built-in apps
cos sys info                           # system information
cos checkpoint create "clean state"    # snapshot the workspace
cos app web read https://example.com   # fetch a page as Markdown
```

### Talk to the agent

```bash
cos agent setup                                # one-time: pick provider, store key
cos agent ask "find the largest files and tell me why"
cos agent chat                                 # interactive REPL
```

On the desktop, **Super+A** opens the Spotlight-style agent overlay; **Super+Shift+A** arms voice input.

## License

MIT for the kernel and apps. See the [`NOTICE`](NOTICE) for vendored sources and their upstream licenses.
