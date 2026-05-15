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

Permission roles and the `cos perms check` app gate are documented in [permissions.md](permissions.md).

**Apps** — `cos app <name> <command>` ([all apps](apps.md)):

| App | Purpose |
|---|---|
| `fs` | File operations, search, metadata |
| `exec` | Command execution |
| `web` | URL → Markdown (JS rendered) |
| `search` | Web and image search (Google/Brave) |
| `email` | Send, search, read (SMTP/Gmail/Outlook) |
| `calendar` | Events and scheduling (local/Google/Outlook) |
| `doc` | Read PDF, DOCX, XLSX, PPTX, CSV |
| `db` | SQLite databases |
| `net` | HTTP client |
| `kv` | Key-value store |
| `log` | Audit log search |
| `notify` | Notifications |
| `pkg` | Package management |

## Discovery

```bash
cos                              # list OS primitives
cos app                          # list apps
cos <name>                       # show commands for a primitive
cos app <name>                   # show commands for an app
cos <name> <command> --schema    # full parameter schema (JSON)
```

All errors include a `code` field for programmatic handling ([error codes](errors.md)).

For detailed usage of any feature, read the corresponding doc linked above.
