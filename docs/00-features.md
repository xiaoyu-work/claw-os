# Part 0: Features Overview

This is the catalog of **what Claw OS provides**, organized by capability area.
It is feature-focused — if you want to know *how* something is implemented, see
[docs/04-architecture.md](04-architecture.md). For full command syntax, see
[docs/02-cos-commands.md](02-cos-commands.md) (primitives) and
[docs/03-builtin-apps.md](03-builtin-apps.md) (apps).

Claw OS is a Linux-based operating system designed for autonomous AI agents.
Everything below ships in the box; nothing requires extra services or accounts.

---

## At a glance

```
                              ┌──────────────────────────────┐
                              │  Your agent  /  OpenClaw     │
                              └──────────────┬───────────────┘
                                             │
                              cos <primitive> · cos app <name> · cos agent <verb>
                                             │
   ┌─────────────────────────────────────────┴────────────────────────────────────┐
   │                                                                              │
   │   Kernel binary (cos)         Built-in apps          Agent runtime           │
   │   ─────────────────────       ───────────────        ───────────────────     │
   │   sys  proc  checkpoint       fs   exec  web         ask · stream · live     │
   │   sandbox  ipc  watch         db   doc   net         memory · skills · tools │
   │   service  credential         kv   log   notify      LLM providers · MCP     │
   │   cron  netfilter             pkg  search            vision · voice · safety │
   │   policy/perms (caps)         email calendar         hooks · audit · doctor  │
   │   browser  agent  model       gateway-* (×20)                                │
   │   engine  trace                                                              │
   │                                                                              │
   │   Local inference (model + engine)        Web engine (cos-browser, V8)       │
   │   Desktop environment (forked COSMIC)     Skills + plugins (skills/, plugins/)│
   │                                                                              │
   └──────────────────────────────────────────────────────────────────────────────┘

   Build targets: docker · iso-live · iso-installer · vm · wsl · .deb + apt repo
```

Every command returns JSON. Every operation is audited. Every workspace change is reversible.

---

## 1. The `cos` kernel binary

A single static Rust binary at `/usr/local/bin/cos` — no daemons, no dependencies. All the primitives below are subcommands of one executable.

### 1.1 System inspection (`cos sys`)

Read OS state as structured data instead of parsing `/proc` and `/sys` text files.

| Command | What it tells you |
|---|---|
| `cos sys info` | OS identity (name, version, platform, arch, hostname, pid) |
| `cos sys env [pattern]` | Environment variables, optionally filtered |
| `cos sys resources` | Disk, memory, CPU usage |
| `cos sys uptime` | System uptime |
| `cos sys proc` | Every running process with state + CPU/memory |
| `cos sys mounts` | All mount points with device + filesystem + options |
| `cos sys net` | Interfaces (rx/tx bytes) + TCP connections |
| `cos sys cgroup` | Memory/CPU/PID limits + current usage |

### 1.2 Process sessions (`cos proc`)

Named, persistent process handles instead of opaque PIDs.

- Spawn a process with a **stable name**, parent, and group: `cos proc spawn --session build-1 --group ci -- cargo build`
- Query buffered stdout/stderr **after the fact** without attaching
- Kill an entire named group at once
- Inherit context (permissions, scope) from parent to child
- Priority control (`low`, `normal`, `high`, `realtime`)
- CPU/memory/IO stats per session, sourced from `/proc/<pid>/`
- Survives `cos` restarts via an on-disk registry

### 1.3 Reversible workspace (`cos checkpoint`)

Snapshot and roll back the filesystem instantly — no git, no copies.

- `cos checkpoint create "before risky refactor"` — freeze current state
- `cos checkpoint diff` — see modified / created / deleted files since the last snapshot
- `cos checkpoint rollback` — instant undo, any number of files, sub-second
- Multiple independent namespaces (parallel sandbox runs each get their own overlay)
- Quota enforcement: `cos checkpoint quota-set 2G` caps unbounded growth

### 1.4 Sandboxed execution (agent tool)

The agent runs untrusted / model-generated code inside a confined namespace + cgroup + seccomp box. This is exposed only as the `cos_sandbox` LLM tool — not as a user-facing CLI command — so untrusted execution always goes through the agent.

- Memory / CPU / task / runtime limits
- Optional network isolation
- Three seccomp profiles: `minimal`, `network`, `full`
- Exit codes that distinguish OOM kill (137) and timeout (124)

### 1.5 Inter-process coordination (`cos ipc`)

File-based, daemon-free coordination primitives.

- **Messages** — typed message queues per session
- **Locks** — advisory locks scoped to a name
- **Barriers** — N-party rendezvous
- **Streaming named pipes** — long-lived multi-producer / multi-consumer streams

### 1.6 Filesystem watching (`cos watch`)

Aggregate file, process, and service events into one structured stream.

- `cos watch start --path /home/cos --events create,modify,delete`
- Multi-source aggregation (file + proc + service in one watcher)
- Bounded event history for after-the-fact querying

### 1.7 Service management (`cos service`)

Lightweight service supervisor — simpler than systemd, agent-aware.

- Declarative service definitions under `/usr/lib/cos/services/`
- Lifecycle hooks (pre-start, post-start, pre-stop)
- Graceful drain on shutdown
- Dependency-ordered start/stop
- Used internally for the browser service and the model-runtime daemon

### 1.8 Encrypted credentials (`cos credential`)

A kernel keyring analog for API keys, tokens, and passwords.

- AES-256-GCM encrypted at rest
- Permission-controlled (only sufficiently-privileged sessions can load)
- Per-tenant namespaces
- TTL — credentials can auto-expire
- Bundles — load groups of related secrets in one call
- `cos credential store OPENAI_KEY "sk-..." --ttl 3600 --namespace tenant-a`

### 1.9 Job scheduling (`cos cron`)

Cron rebuilt for agents.

- Each job carries its own permission scope and credential bundle
- Overlap protection: `skip` / `queue` / `kill` / `allow` concurrent runs
- Structured result capture: stdout/stderr tails, exit codes, durations
- Queryable run history per job

### 1.10 Network policy (`cos netfilter`)

Declarative outbound firewall.

- `cos netfilter add --allow "api.openai.com" --port 443`
- Wildcard support (`*.github.com`)
- Per-rule rate limiting
- `cos netfilter check "host.example.com"` previews the decision before connecting

### 1.11 Permissions (`cos perms` / `cos policy`)

Claw OS gates every privileged action behind a **capability** — a `(verb, scope)` pair held by the calling session in its capability set. Roles bundle verbs into recognizable sets (e.g., *reader*, *editor*, *operator*, *root*).

- `cos perms list` — see the calling session's capability set
- `cos perms grant <role> <scope>` — grant a role on a path
- `cos perms check <verb> <path>` — preview whether an action would be allowed
- Localized capability catalog (every verb has an i18n label)
- Risk classification per verb (read vs. write vs. destructive)

> **Note**: Earlier docs (see [docs/01](01-agent-first-design.md)) describe a 4-tier `policy` model. That is the legacy interface, still wired in for compatibility; the capability model in `cos perms` is the current direction.

### 1.12 Audit (`cos app log`)

Every `cos` invocation is logged to a JSONL audit trail automatically — no opt-in. Sensitive values (Bearer tokens, API keys, Authorization headers) are redacted before being written.

- `cos app log search --app exec --status error`
- `cos app log tail 20`
- Per-app, per-status, time-range filters

### 1.13 Tracing (`cos trace`)

Per-session, structured call tree for debugging multi-step operations.

---

## 2. Built-in apps (`cos app <name>`)

Higher-level capabilities that ship in the rootfs. Implementation is Python (so they are extensible and replaceable), invoked via `cos app <name> <command>`.

| App | What it gives you |
|---|---|
| `fs` | File I/O with structured metadata + content search |
| `exec` | Run any command with language autodetection and timeout |
| `web` | URL → Markdown / links / structured JSON / PNG screenshot |
| `db` | SQLite query and admin |
| `doc` | Read PDF, DOCX, XLSX, PPTX, CSV as structured data |
| `net` | HTTP client with redacting + retries |
| `kv` | Local key-value store |
| `log` | Search the audit trail |
| `notify` | Local desktop / system notifications |
| `pkg` | Package install / list / remove |
| `search` | Web + image search (Google, Brave backends) |
| `email` | Send, search, read email via SMTP / Gmail / Outlook |
| `calendar` | Events + scheduling via local SQLite, Google, or Outlook |

See [docs/03-builtin-apps.md](03-builtin-apps.md) for full command syntax.

---

## 3. Outbound gateways (`cos app gateway-*`)

Twenty one-way channels for push notifications, alerts, and bot delivery. All read configuration from `cos credential`, all redact sensitive values in audit logs.

| Category | Channels |
|---|---|
| Team chat | `slack` · `teams` · `discord` · `googlechat` · `mattermost` · `rocketchat` |
| Asian chat platforms | `larksuite` · `dingtalk` |
| Open / federated | `matrix` · `zulip` · `telegram` · `webex` |
| Consumer | `whatsapp` · `signal` · `sms` |
| Smart home / mobile push | `homeassistant` · `ntfy` · `pushover` |
| Generic | `webhook` · `email` |

Outbound-only by design — these are alerting/notification rails, not full bidirectional bridges.

---

## 4. The agent runtime (`cos agent`)

A complete LLM agent loop embedded into the OS — think "OpenAI Agents SDK as system primitives."

### 4.1 Conversation loop

| Command | What it does |
|---|---|
| `cos agent ask "<prompt>"` | Single-shot answer |
| `cos agent stream` | Streaming variant |
| `cos agent live` | Multi-turn loop with the full tool registry |
| `cos agent chat` | Interactive REPL |
| `cos agent replay <session>` | Re-render any past conversation from history |
| `cos agent interrupt <session>` | Cancel a running session mid-turn |
| `cos agent service` | Long-lived job-queue worker mode |

### 4.2 LLM providers

Built-in adapters for the major model APIs:

- **Anthropic** (Messages API + real SSE streaming)
- **OpenAI-compatible** (OpenAI, Azure OpenAI, Together, Groq, OpenRouter, …)
- **Google Gemini**
- **AWS Bedrock** (SigV4-signed, streaming via `aws_eventstream`)

Plus **credential pooling** — rotate across many API keys, with per-key cooldowns when a provider returns rate-limit / quota errors. Failure classifier converts raw HTTP errors into a `FailureClass` enum the retry policy understands.

### 4.3 Memory

Three complementary stores, all local:

- **Conversation history** — per-session SQLite with FTS5 full-text search
- **Semantic memory** — vector store backed by a local Qwen3-Embedding model
- **Honcho dialectic** — durable cross-session user facts, distilled by an async LLM curator that watches for "I prefer X" / "always use Y" patterns
- **MEMORY.md / USER.md** auto-injection into the system prompt

### 4.4 Tools

- All `cos` primitives exposed as agent tools via an internal proxy
- **MCP outbound** — connect to any third-party MCP server (filesystem, GitHub, postgres, etc.) and use its tools as first-class
- Per-tool guardrails and approval policies
- `cos agent tools` — introspect the active tool registry

### 4.5 Skills

A skill is a Markdown file (`SKILL.md`) with a small YAML front-matter — a recipe the agent loads on demand.

- A built-in `claw-os` skill covers the OS itself in 13 topic files: `apps`, `checkpoint`, `credential`, `cron`, `errors`, `ipc`, `network`, `permissions`, `process`, `sandbox`, `service`, `trace`, `watch`
- **Skill hub** — discover and install community skills from GitHub Releases: `cos agent skills hub install <id>`
- **Per-skill usage telemetry** — see which skills the agent actually invokes
- **Curator drafts** — auto-distil ad-hoc patterns into proposed skills, then refine via LLM

### 4.6 Hooks & audit

Pluggable pre/post turn and pre/post tool hooks let you observe or veto every step of the loop.

- **AuditHook** — JSONL trail of every turn and tool dispatch (separate from `cos log`)
- **CheckpointHook** — auto-snapshot before destructive tool calls
- **HonchoRecorder** — auto-replay conversations into the dialectic memory
- `cos agent hooks enable/disable` — persistent configuration

### 4.7 Vision & voice

- `cos agent vision sniff <image>` — quick classification
- `cos agent vision analyze <image>` — full VLM analysis
- `cos agent vision route <image>` — preview which model an image will be routed to
- `cos agent media` — list TTS / STT / image-gen providers + output dir
- **TTS**: Edge TTS (keyless WebSocket), system fallbacks (`afplay` / `paplay` / `PlaySoundW`)
- **STT**: local Whisper via ONNX Runtime

### 4.8 Safety

- `cos agent redact <text>` — secret scrubber (Bearer, API keys, …)
- `cos agent file-safety <path>` — should I write this file? (config-driven policy)
- `cos agent osv <path>` — dependency-vuln check via osv.dev
- `cos agent binary-ext <file>` — extension-only binary classifier
- `cos agent guardrails` / `cos agent approval` — config introspection

### 4.9 Operations & introspection

| Command | Purpose |
|---|---|
| `cos agent doctor` | Single-shot holistic self-check |
| `cos agent providers` | Probe LLM provider credentials |
| `cos agent llm models / cost` | Browse model metadata + pricing |
| `cos agent sessions top / stats / purge / set-title` | Session ops |
| `cos agent audit cache-stats` | Cache hit rate + USD savings |
| `cos agent run-log` | Per-LLM-call run trail |
| `cos agent tokens / compress / retry` | Prompt size + retry policy preview |
| `cos agent prompt show` | Inspect the assembled system prompt |
| `cos agent honcho` | Smoke test the dialectic client |

---

## 5. Local inference (`cos model` · `cos engine`)

Run AI models on-device without external APIs.

### 5.1 Engines

Three native runtimes supported, each treated as an independently upgradable system component (multiple versions installable side-by-side):

- **ONNX Runtime** (`ort`) — Whisper STT, Piper / KittenTTS, embeddings
- **ONNX Runtime GenAI** (`ort-genai`) — generative text via ONNX
- **llama.cpp** — GGUF text models

### 5.2 Engine package management (`cos engine`)

- Install / list / pin / remove specific engine versions
- Multi-version coexistence under `<data_dir>/engines/<engine>/<version>/`
- Per-engine version selection at model registration time

### 5.3 Model registry (`cos model`)

- `cos model list` — registered models on this host
- `cos model import` — register a user-provided ONNX or GGUF file
- `cos model rm` — remove a model
- `cos model speak` — quick voice synthesis smoke test
- Ships with Qwen3-Embedding-0.6B for the agent's semantic memory

### 5.4 The model-runtime daemon

A long-running `model-runtime` service (started via `cos service start model-runtime`) keeps engines warm so call latency reflects only the model, not the loader.

---

## 6. The web engine (`cos-browser`)

A vendored Rust + V8 headless browser (forked from [Obscura](https://github.com/), Apache-2.0). One self-contained binary — no Chromium dependencies, no remote update channel.

### What you can do

| Command | Result |
|---|---|
| `cos app web read <url>` | URL → Markdown |
| `cos app web links <url>` | URL → list of links |
| `cos app web json <url>` | URL → structured JSON (schema.org, OpenGraph) |
| `cos app web screenshot <url>` | URL → PNG |
| `cos browser start` | Expose CDP at `ws://localhost:9222` — use any Puppeteer / Playwright / CDP client |

By default the CDP server is **opt-in** — `cos app web read` runs the browser as a subprocess only when needed.

---

## 7. The desktop environment (`desktop/`)

A full desktop environment lives in this repo for shipping Claw OS as a bootable OS, not just a container. Forked from [COSMIC](https://github.com/pop-os/cosmic-epoch) (System76, GPLv3) for unified iteration alongside `cos` and `cos-browser`.

- **Compositor**, **session manager**, **greeter** (login)
- **Panel** (dock + taskbar), **launcher** (Spotlight-style)
- Apps: **files**, **term**, **edit**, **text**, **store**, **settings**, **player**, **screenshot**, **theme-editor**
- **Background**, **idle**, **OSD**, **notifications**
- **xdg-desktop-portal** integration
- **Initial setup** flow for first-boot configuration

Status: forked, build-able, **not yet rebranded** (binaries still use `cosmic-*` names). See [`desktop/README.md`](../desktop/README.md), [`desktop/PROVENANCE.md`](../desktop/PROVENANCE.md), and [`desktop/TRADEMARK.md`](../desktop/TRADEMARK.md) before shipping anything that uses the COSMIC name or logo.

---

## 8. Skills (`skills/`)

Markdown-defined recipes the agent loads on demand. See §4.5 for the runtime side; the directory holds the actual content.

- `skills/claw-os/` — the OS's own skill, a `SKILL.md` manifest plus 13 topic files

Skills are versioned alongside the rest of the OS, but they can also be installed at runtime via the hub.

---

## 9. Plugins & clients

- `plugins/openclaw/` — the **OpenClaw** agent runs on top of Claw OS via this plugin. Claw OS is what OpenClaw lives on; OpenClaw is what makes Claw OS useful end-to-end.
- `clients/bridge/` — a thin bridge for external (non-`cos`) clients that need to call into the OS.

---

## 10. Build & distribution

Ship Claw OS as your medium of choice. Everything builds from one source tree.

| Target (under `targets/`) | What it produces |
|---|---|
| `docker` | Container image (`ghcr.io/xiaoyu-work/claw-os`) |
| `iso-live` | Bootable Live ISO (hybrid BIOS + UEFI) |
| `iso-installer` | Installable ISO with a Calamares kiosk installer |
| `vm` | qcow2 / vmdk / vhdx disk image (BIOS + UEFI) |
| `wsl` | Importable WSL2 rootfs tarball |

Plus:

- A **`.deb` package** for `cos` itself
- A **GitHub Pages-hosted apt repo** — `apt install cos` on any Debian/Ubuntu host
- Driven by `build.sh` at the repo root

The rootfs is assembled from composable **features** under `rootfs/features/` (`base`, `cos-core`, `browser`, `desktop`, `systemd`, `vm`, `live`, `installer`, `kernel`, `apt-source`, `grub-disk`). Each target picks the feature set it needs — `iso-live` does not pull `installer`, `wsl` skips `kernel`, etc.

---

## 11. Internationalization

Every user-facing string in the kernel, built-in apps, and third-party apps flows through `core/src/i18n/`. A single locale switch redrives the whole UI — error messages, capability labels, audit-log descriptions, and CLI help text all change together.

---

## Quick-start tours

### For an end user
1. `docker run -it ghcr.io/xiaoyu-work/claw-os`
2. `cos` → list primitives · `cos app` → list apps
3. `cos sys info` · `cos app web read https://example.com` · `cos checkpoint create "clean"`
4. Boot from the Live ISO (`targets/iso-live`) for the full desktop experience.

### For an agent developer
1. Read this page top to bottom for the capability map.
2. [docs/02-cos-commands.md](02-cos-commands.md) — full primitive syntax.
3. [docs/03-builtin-apps.md](03-builtin-apps.md) — full app syntax.
4. Try the agent loop: `cos agent ask "summarise this directory"`.
5. Hook your favourite LLM via `cos credential store ANTHROPIC_API_KEY ...` and `cos agent providers`.

### For a contributor
1. [docs/04-architecture.md](04-architecture.md) — how the pieces fit.
2. [docs/01-agent-first-design.md](01-agent-first-design.md) — design principles.
3. [CONTRIBUTING.md](../CONTRIBUTING.md) — build from source.
4. `rootfs/features/README.md` — modular rootfs system.
5. [desktop/README.md](../desktop/README.md) — desktop fork status.

---

## What's not yet documented (honest gaps)

Features that ship in code but don't yet have detailed reference docs:

- `cos agent` runtime — covered at feature level above; per-subcommand reference doc TBD
- `cos model` / `cos engine` — feature level only
- Capability model (`cos perms`) — needs its own reference doc; doc 01's legacy tier section will eventually be replaced
- Outbound gateways — per-channel configuration docs
- Skill format specification — beyond the examples in `skills/claw-os/`
- Build target deep-dives — beyond `rootfs/features/README.md`
- i18n contribution workflow

These are honest gaps, not bugs in the description above. Contributions welcome.
