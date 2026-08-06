---
name: claw-os
description: "Claw OS — agent-native operating system. Run cos for OS primitives, cos app for apps. Read child docs in skills/claw-os/ for detailed usage."
---

# Claw OS

You are running on Claw OS. All commands return structured JSON.

## Quick Reference

**OS primitives** (run with `cos <name> <command>`):

| Primitive | Purpose |
|---|---|
| `checkpoint` | Snapshot, diff, rollback workspace ([details](checkpoint.md)) |
| `service` | Lifecycle hooks, graceful shutdown ([details](service.md)) |
| `credential` | Encrypted secrets, namespaces, TTL, bundles ([details](credential.md)) |
| `cron` | Job scheduling with context and overlap protection ([details](cron.md)) |
| `agent` | Manage agent tasks: list / show / stop / undo / resume ([details](sessions.md)) |
| `sys` | System info, resources, processes |

**Agent-only tools** (call directly via the tool interface, not the shell):

| Tool | Purpose |
|---|---|
| `cos_sandbox` | Run untrusted code in a Linux-namespace sandbox ([details](sandbox.md)) |
| `cos_proc` | Spawn and manage processes by session ([details](process.md)) |
| `cos_ipc` | Messages, locks, barriers, streaming pipes ([details](ipc.md)) |
| `cos_watch` | Event-driven file/process/service watching ([details](watch.md)) |
| `cos_netfilter` | Outbound firewall and rate limiting ([details](network.md)) |
| `cos_trace` | Execution tracing — tree-structured observability ([details](trace.md)) |
| `cos_browser` | Standalone CDP server lifecycle for external Puppeteer/Playwright clients (the `web` app already uses cos-browser per-request and does not need this) |
| `cos_diagnose` | Structured system diagnosis with bounded probes, evidence IDs, confidence, and recommendations ([diagnostic protocol](diagnostics.md)) |

## System Diagnosis

For system-level symptoms, call `cos_diagnose` before proposing a cause or
mutation. Then follow the matching playbook:

| Symptom | Playbook |
|---|---|
| Slow, frozen, high CPU or memory | [Performance](diagnostics-performance.md) |
| Offline, slow network, DNS or connectivity | [Network](diagnostics-network.md) |
| Disk full, storage latency, missing mount | [Storage](diagnostics-storage.md) |
| Crash, OOM kill, segmentation fault | [Crash](diagnostics-crash.md) |
| Failed or unhealthy service | [Service](diagnostics-service.md) |
| Heat, fan, battery or throttling | [Thermal](diagnostics-thermal.md) |
| Suspicious login, denial or exposed port | [Security](diagnostics-security.md) |

Never present a system-state claim without naming the evidence that supports
it. Read-only investigation comes first; capability approval and a recovery
plan come before mutation.

Permission roles and app capability gates are documented in [permissions.md](permissions.md).

**Apps** — `cos app <name> <command>` ([all apps](apps.md)):

| App | Purpose |
|---|---|
| `accessibility-manager` | COSMIC magnifier/filter/inversion and AT-SPI screen reader |
| `audio-manager` | PipeWire/WirePlumber volume, mute, routes, and profiles |
| `backup-center` | Mounted Restic backup, retention, check, forget, and restore |
| `bluetooth-manager` | BlueZ discovery, pairing, connection, trust, and power |
| `fs` | File operations, search, metadata |
| `hardware-center` | CPU, GPU, PCI, USB, memory, storage, driver, and thermal inventory |
| `exec` | Command execution |
| `web` | URL → Markdown (JS rendered) |
| `search` | Web and image search (Google/Brave) |
| `security-center` | Authentication, sudo, SSH, MAC, port, and security-event analysis |
| `email` | Send, search, read (SMTP/Gmail/Outlook) |
| `event-center` | Persistent udev, systemd, journal, storage, security, and pidfd events |
| `firewall-manager` | Scoped nftables allow/drop rules with durable rollback |
| `calendar` | Events and scheduling (local/Google/Outlook) |
| `camera-manager` | PipeWire camera discovery and bounded PNG/JPEG capture |
| `container-manager` | Docker, Podman, containerd lifecycle, logs, cgroups, and namespaces |
| `config-editor` | Validated atomic /etc edits with durable backup and rollback |
| `clipboard-manager` | Sensitive Wayland clipboard read, write, types, and clear |
| `doc` | Read PDF, DOCX, XLSX, PPTX, CSV |
| `crash-doctor` | Coredump, OOM, segfault, journal correlation, and backtraces |
| `db` | SQLite databases |
| `desktop-manager` | COSMIC Wayland window discovery, focus, close, and restart |
| `display-manager` | COSMIC layout, modes, scale, mirror, rotation, and brightness |
| `net` | HTTP client |
| `kv` | Key-value store |
| `log` | Audit log search |
| `location-manager` | GeoClue coordinates and offline timezone suggestions |
| `netdiag` | Link, route, DNS, TCP reachability, and latency diagnosis |
| `network-manager` | Wi-Fi, VPN, and NetworkManager radio control |
| `notify` | Notifications |
| `pkg` | Package management |
| `power-manager` | UPower status and logind sleep/reboot/shutdown |
| `printer-manager` | CUPS discovery, capabilities, queues, printing, and cancel |
| `systemd` | Native system service status and lifecycle control |
| `system-snapshot` | Snapper, Btrfs, or LVM full-system recovery points |
| `storage-manager` | UDisks2 mount/eject, SMART, and filesystem health |
| `usb-guard` | USB inventory, authorization, persistent blocking, and eject |
| `user-manager` | Local users, groups, passwords, shells, membership, and rollback |

## Discovery

```bash
cos                              # list OS primitives
cos app                          # list apps
cos <name>                       # show commands for a primitive
cos app <name>                   # show commands for an app
cos <name> <command> --schema    # full parameter schema (JSON)
```

All errors include a `code` field for programmatic handling ([error codes](errors.md)).

## Provider fallback

The system agent supports an ordered cross-provider fallback chain in
`~/.config/cos/config.json`:

```json
{
  "agent": {
    "provider": "anthropic",
    "model": "claude-sonnet-4-6",
    "provider_fallbacks": [
      {"provider": "openai", "model": "gpt-4.1", "api_key_env": "OPENAI_API_KEY"},
      {"provider": "llama_local", "model": "local-default"}
    ]
  }
}
```

Fallback occurs only for transport, authentication, quota/rate-limit, stream,
or malformed-upstream failures. Invalid requests and internal policy/budget
errors do not switch providers. Each fallback model is independently checked
by `ai.chat` capability and budget gates. Switches are emitted to the chained
Agent audit log and surfaced as degraded-mode metadata. Streaming only switches
before a provider stream is established; it never mixes providers after output
has begun.

## User isolation

`cos agent serve` runs as one non-root Unix uid per instance. It refuses root,
wrong-owner, or symlinked state roots and tightens its per-user data directory
to mode `0700`. Web task lifecycle, approval resolution, memory sessions, and
context-event inbox reads are filtered to that uid; legacy records without an
owner are hidden from the web surface. Privileged clawd APIs retain an explicit
all-owner view only for root callers.

## Web TLS

Loopback `cos agent serve` may use HTTP. Any non-loopback bind is rejected
unless both `--tls-cert /absolute/cert.pem` and
`--tls-key /absolute/key.pem` are supplied. The built-in server uses rustls;
the private key must be owned by the serving uid with no group/other
permissions. API query-string tokens are never accepted; the loopback `?t=`
value is consumed only by the frontend bootstrap exchange. A TLS reverse proxy
can instead terminate HTTPS while `cos agent serve` remains bound to
`127.0.0.1`.

`serve.token` is only a bootstrap secret. The browser exchanges it at
`POST /api/auth/token` for a uid-bound HMAC-SHA256 access token valid for at
most one hour; normal APIs reject the persistent bootstrap secret. Run
`cos agent serve --rotate-token` to rotate both bootstrap and signing secrets,
immediately invalidating every issued access token.

For detailed usage of any feature, read the corresponding doc linked above.
