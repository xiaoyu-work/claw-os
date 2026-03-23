# Claw OS

The operating system for [OpenClaw](https://github.com/openclaw/openclaw).

Linux, macOS, and Windows were designed for humans — they return pixels, terminal text, and GUI windows. Claw OS was designed for agents — every system call returns structured data, every process is tracked by session, and every operation is automatically audited.

OpenClaw runs on your devices, in your channels, with your rules. Claw OS is where it lives.

## Agent-Native by Design

The entire system is rebuilt around what agents actually need:

- **Structured I/O** — Every command returns JSON, not text that needs parsing
- **Managed execution** — Processes are tracked by session ID with output buffering, not raw PIDs
- **Built-in sandboxing** — Namespace isolation + resource limits in one command, no Docker-in-Docker
- **Pre-digested content** — PDFs, web pages, documents come back as clean text, not raw bytes
- **Automatic audit trail** — Every operation logged with timestamp, duration, and status
- **Browser as a service** — JavaScript-rendered web pages returned as Markdown, managed by the OS

## Built-in Apps

Claw OS ships with 15 apps purpose-built for agent workflows:

```
cos sandbox exec --mem 512M --timeout 300 --no-network -- python3 untrusted.py
cos proc spawn --session build-1 -- npm run build
cos proc output build-1 --tail 20
cos web read https://example.com
cos doc read paper.pdf
cos fs ls /workspace
cos db query mydb "SELECT * FROM users"
cos kv set project:status "building"
cos net fetch https://api.example.com/data
```

## Architecture

```
cos (Rust binary)
├── sandbox    Namespace isolation + cgroup v2 resource limits
├── proc       Process sessions with output buffering
├── browser    Jina Reader lifecycle management
├── sysinfo    Native system information
├── router     App discovery + command dispatch
├── bridge     Python app subprocess integration
└── audit      Automatic operation logging

11 Python apps
├── fs         File operations with metadata
├── exec       Command execution
├── web        Browser with JS rendering (Jina Reader)
├── db         SQLite databases
├── doc        PDF, DOCX, XLSX, CSV reader
├── net        HTTP client
├── kv         Key-value store
├── log        Audit log search
├── notify     Notifications
├── pkg        Package management
└── sys        System information
```

## Why Not Just Linux?

OpenClaw on Linux needs ~10,000 lines of TypeScript just to manage infrastructure:

| | Linux | Claw OS |
|---|---|---|
| **Sandboxing** | Docker-in-Docker (~6000 LOC) | `cos sandbox exec` |
| **Process tracking** | Manual registry (~900 LOC) | `cos proc spawn` |
| **Browser** | Selenium + Chrome lifecycle (~3000 LOC) | `cos web read` |
| **Binary files** | Install parsers, detect formats | `cos doc read` |
| **Audit** | Implement your own logging | Built-in |

## Quick Start

```bash
docker pull ghcr.io/xiaoyu-work/claw-os:latest
docker run -it --name claw -v ./workspace:/workspace ghcr.io/xiaoyu-work/claw-os
```

You're in. Try:

```bash
cos sys info
cos fs ls /workspace
cos web read https://example.com
```

> Image not published yet? See [CONTRIBUTING.md](CONTRIBUTING.md) to build from source.

## Pre-installed

- **Node.js 24** + pnpm
- **Python 3** + pip
- **Chromium** (via Jina Reader)
- **ripgrep**, **git**, **curl**, **sqlite3**, **jq**

## License

MIT
