# Claw OS

The operating system for [OpenClaw](https://github.com/openclaw/openclaw).

Linux, macOS, and Windows were designed for humans — they return pixels, terminal text, and GUI windows. Claw OS was designed for agents — every system call returns structured data, every process is tracked by session, and every operation is automatically audited.

OpenClaw runs on your devices, in your channels, with your rules. Claw OS is where it lives.

## Beyond Linux

Claw OS provides primitives that traditional operating systems don't:

| Capability | Linux | Claw OS |
|---|---|---|
| **Structured I/O** | Text stdout | JSON from every command |
| **Checkpoint / Rollback** | None | OverlayFS — snapshot, diff, undo any file changes |
| **Permission Model** | uid/rwx (for humans) | Tier + Scope (for agents: 4 levels, path-scoped) |
| **Process Coordination** | Raw pipes, signals | IPC messages, locks with dead-process auto-reclaim, barriers |
| **Process Hierarchy** | PIDs, process groups | Session IDs, named groups, parent-child with context inheritance |
| **Error Recovery** | "Permission denied" | Structured JSON with recovery commands to try |
| **Guardrails** | None | Rapid respawn detection, destructive command warnings |
| **Service Management** | systemd (complex) | Declarative JSON service definitions |
| **Browser** | Not included | Built-in Chromium engine, URL → Markdown in one call |
| **Audit** | Optional, complex | Every operation logged automatically |

## Architecture

```
cos (Rust binary, ~3000 LOC)
├── checkpoint  OverlayFS snapshot, diff, rollback
├── policy      Tier + Scope permission system (6 OpTypes, 4 tiers)
├── proc        Process sessions with groups, hierarchy, wait, signal, result
├── ipc         Messages, locks (stale auto-reclaim), barriers
├── sandbox     Linux namespace isolation + cgroup v2 resource limits
├── service     Generic service manager (declarative JSON definitions)
├── watch       File/directory/process change detection
├── browser     Built-in browser engine lifecycle
├── router      App discovery + dispatch + error recovery hints
├── bridge      Python app subprocess integration
├── audit       Automatic operation logging
└── sysinfo     System information

10 Python apps
├── fs          File operations with metadata and search
├── exec        Command execution with language detection
├── web         URL → Markdown (powered by built-in browser engine)
├── db          SQLite databases
├── doc         PDF, DOCX, XLSX, CSV reader
├── net         HTTP client
├── kv          Key-value store
├── log         Audit log search
├── notify      Notifications
└── pkg         Package management
```

## Quick Start

```bash
docker pull ghcr.io/xiaoyu-work/claw-os:latest
docker run -it --name claw -v ./workspace:/workspace ghcr.io/xiaoyu-work/claw-os
```

You're in. Try:

```bash
cos                                    # see all available apps
cos sys info                           # system information
cos web read https://example.com       # fetch a web page as Markdown
cos checkpoint create "clean state"    # snapshot the workspace
cos checkpoint diff                    # see what changed
```

See [CONTRIBUTING.md](CONTRIBUTING.md) to build from source.

## License

MIT
