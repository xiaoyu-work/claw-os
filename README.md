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
| **Process Coordination** | Raw pipes, signals | IPC messages, locks, barriers, **streaming named pipes** |
| **Process Hierarchy** | PIDs, process groups | Session IDs, named groups, parent-child with context inheritance |
| **Error Recovery** | "Permission denied" | Structured JSON with recovery commands to try |
| **Guardrails** | None | Rapid respawn detection, destructive command warnings |
| **Service Management** | systemd (complex) | Lifecycle hooks, graceful drain, dependency-ordered shutdown |
| **Browser** | Not included | Built-in Chromium engine, URL → Markdown in one call |
| **Audit** | Optional, complex | Every operation logged automatically |
| **Credential Management** | env vars, plaintext files | AES-256-GCM encrypted store with namespaces, TTL, and bundles |
| **Job Scheduling** | crond (no context) | Agent-native cron with tier/scope/credential context, overlap protection |
| **Event System** | inotify (raw events) | Multi-source aggregation (file+proc+service), event history |

## Architecture

```
cos (Rust binary, ~5800 LOC)
├── checkpoint  OverlayFS snapshot, diff, rollback
├── policy      Tier + Scope permission system (6 OpTypes, 4 tiers)
├── proc        Process sessions with groups, hierarchy, wait, signal, result
├── ipc         Messages, locks, barriers, streaming named pipes
├── sandbox     Linux namespace isolation + cgroup v2 resource limits
├── service     Lifecycle hooks, graceful drain, dependency-ordered shutdown
├── watch       inotify-based file watching, multi-source aggregation, event history
├── credential  AES-256-GCM encrypted store, namespaces, TTL, bundles
├── cron        Agent-native job scheduler with context and overlap protection
├── browser     Built-in browser engine lifecycle
├── netfilter   Domain/method/path-level outbound firewall
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
