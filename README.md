# Claw OS

The operating system for [OpenClaw](https://github.com/openclaw/openclaw).

OpenClaw runs on your devices, in your channels, with your rules. Claw OS is where it lives — a purpose-built runtime that gives OpenClaw native access to sandboxed execution, process management, browser rendering, and structured I/O, without the thousands of lines of infrastructure code it needs on a generic OS.

```
cos sandbox exec --mem 512M --timeout 300 --no-network -- python3 untrusted.py
cos proc spawn --session build-1 -- npm run build
cos proc output build-1 --tail 20
cos web read https://example.com
cos doc read paper.pdf
cos fs ls /workspace
```

Every command returns JSON. Every operation is audited. Every process is tracked.

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
