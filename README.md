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

### Docker

```bash
docker pull ghcr.io/xiaoyu-work/claw-os:latest
docker run -it --name claw -v ./workspace:/workspace ghcr.io/xiaoyu-work/claw-os
```

Other targets — bootable ISO (live + installer), WSL image, and `.deb` + apt repo — are produced from `packaging/`. See [CONTRIBUTING.md](CONTRIBUTING.md).

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
