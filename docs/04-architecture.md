# Part 4: Architecture, Deployment & Integration

## System Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│  Agent (LLM / Framework)                                        │
│  e.g., OpenClaw, Claude Code, custom agent                      │
└────────────────────────┬────────────────────────────────────────┘
                         │ cos <primitive> <command> | cos app <name> <command>
                         ▼
┌─────────────────────────────────────────────────────────────────┐
│  cos binary (Rust, static musl, ~6KB)                           │
│                                                                 │
│  ┌──────────┐  ┌────────┐  ┌───────┐  ┌────────────────────┐   │
│  │ router   │→ │ audit  │  │config │  │ recovery hints     │   │
│  └────┬─────┘  └────────┘  └───────┘  └────────────────────┘   │
│       │                                                         │
│       ├── Built-in (Rust)                                       │
│       │   ├── sys        (system info, /proc, cgroup)           │
│       │   ├── proc       (sessions, stats, priority)            │
│       │   ├── checkpoint  (overlayfs, quota, namespaces)        │
│       │   ├── sandbox    (namespaces, cgroups, seccomp)         │
│       │   ├── ipc        (messages, locks, barriers, streaming pipes) │
│       │   ├── watch      (inotify, multi-source, event history)      │
│       │   ├── service    (lifecycle hooks, graceful shutdown)         │
│       │   ├── browser    (chromium engine)                      │
│       │   ├── credential (AES-256-GCM, namespaces, TTL, bundles)     │
│       │   ├── cron       (agent-native job scheduler)                │
│       │   ├── netfilter  (outbound network policy)                    │
│       │   └── policy     (tiers, elevation)                     │
│       │                                                         │
│       └── Python Apps (via bridge subprocess)                   │
│           ├── fs    (files)      ├── net   (HTTP)               │
│           ├── exec  (commands)   ├── kv    (key-value)          │
│           ├── web   (browser)    ├── log   (audit search)       │
│           ├── db    (SQLite)     ├── notify (notifications)     │
│           ├── doc   (PDF/DOCX)   ├── pkg   (packages)           │
│           ├── search (web search)  ├── email (SMTP/Gmail/Outlook)  │
│           └── calendar (local/Google/Outlook)                       │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────────────────┐
│  Linux Kernel                                                   │
│  OverlayFS · Namespaces · cgroups v2 · seccomp · /proc · /sys  │
└─────────────────────────────────────────────────────────────────┘
```

### Stateless Design

`cos` has **no long-running daemon**. Every invocation is a standalone process:

1. Read command-line args
2. Load state from disk (registry.json, rules.json, etc.)
3. Execute operation
4. Write state back to disk
5. Print JSON result
6. Exit

This means:
- `cos` restarts don't lose state (everything is on disk)
- No socket connections to maintain
- No crash recovery needed (JSON files are the source of truth)
- Multiple `cos` invocations can run concurrently (file-level atomicity)

### Rust Core vs Python Apps

**Why two languages?**

| Aspect | Rust Core | Python Apps |
|--------|-----------|-------------|
| **Role** | OS primitives (processes, filesystems, security) | Higher-level tools (HTTP, documents, databases) |
| **Performance** | Critical path — fast, no runtime | Acceptable latency — rich ecosystem |
| **Stability** | Changes rarely | Changes often (new features, formats) |
| **Dependencies** | Minimal (serde, chrono, libc) | Rich (pymupdf, openpyxl, requests) |
| **Update** | Requires recompile | Just replace main.py |

The bridge pattern (`core/src/bridge.rs`) spawns a Python subprocess for each app command. This provides:
- **Isolation**: A crashing Python app doesn't affect the core
- **Policy enforcement**: Tier/scope checked before subprocess starts
- **Environment control**: Agent-native env vars injected automatically

---

## Docker Deployment

### Image Variants

Claw OS publishes multiple Docker images:

| Image | Tag | Contents | Entrypoint |
|-------|-----|----------|------------|
| Base OS | `claw-os:latest` | Pure Claw OS | `bash --login` |
| OpenClaw | `claw-os:openclaw` | OS + OpenClaw agent framework | `openclaw gateway` |

### Base Image: FROM scratch

The base image starts from `scratch` (no base image):

```dockerfile
FROM scratch
COPY build/claw-os-rootfs /
ENV COS_VERSION=0.3.0 NODE_MAJOR=24 DEN=/den
WORKDIR /den
ENTRYPOINT ["/usr/local/bin/cos-init"]
CMD ["/bin/bash", "--login"]
```

This means the image contains **exactly** what's in the rootfs — nothing more. Total image size is determined by the bootstrapped Debian + installed packages.

### Running Claw OS

```bash
# Interactive shell
docker run -it --privileged ghcr.io/xiaoyu-work/claw-os:latest

# With mounted workspace
docker run -it --privileged -v ./project:/den ghcr.io/xiaoyu-work/claw-os:latest

# Run a specific command
docker run --privileged ghcr.io/xiaoyu-work/claw-os:latest cos sys info

# Run with OpenClaw
docker run -it --privileged -e ANTHROPIC_API_KEY=sk-... ghcr.io/xiaoyu-work/claw-os:openclaw
```

**Note:** `--privileged` is needed for OverlayFS mounting and namespace isolation. In production, use specific capabilities instead:
```bash
docker run --cap-add SYS_ADMIN --cap-add NET_ADMIN --security-opt apparmor=unconfined ...
```

### CI/CD Pipeline

The GitHub Actions workflow (`.github/workflows/build.yml`):

1. Build `cos` binary with musl target (fully static)
2. Bootstrap rootfs via `debootstrap`
3. Build and push Docker images to GitHub Container Registry
4. Tagged builds create versioned images

---

## OpenClaw Integration

Claw OS integrates with [OpenClaw](https://github.com/xiaoyu-work/openclaw) (a multi-channel AI agent framework) through a plugin + skill system.

### Plugin (`plugins/openclaw/`)

The plugin registers 6 tools that replace OpenClaw's built-in equivalents:

| Claw OS Tool | Replaces | Advantage |
|-------------|----------|-----------|
| `cos_web_read` | Playwright browser | Lighter, built-in Chromium, returns Markdown |
| `cos_exec` | bash-tools | Session tracking, tier enforcement, audit |
| `cos_fs` | fs-bridge | Metadata, tags, search, structured output |
| `cos_net_fetch` | web-fetch/undici | Audit logging, netfilter integration |
| `cos_doc_read` | pdfjs-dist | PDF, DOCX, XLSX, CSV support |
| `cos_checkpoint` | (no equivalent) | Unique to Claw OS — snapshot/rollback |

Each tool is a TypeScript file that shells out to `cos`:

```typescript
// plugins/openclaw/src/tools/exec.ts
async function cosExec(command: string, timeout?: number) {
  const result = await cos.run("app", "exec", "run", ["--shell", "bash", command]);
  return JSON.parse(result);
}
```

### Skill (`skills/claw-os/SKILL.md`)

A comprehensive Markdown document that teaches agents how to use Claw OS. When an agent framework loads this skill, the agent "knows" the full command vocabulary:

- How to use checkpoints for safe experimentation
- How to manage processes with sessions and groups
- How to use sandbox isolation for untrusted code
- How to query system resources
- How to communicate between processes via IPC

---

## Security Model

### Defense in Depth

Claw OS security is **layered** — no single mechanism is sufficient alone:

```
Layer 1: Tier + Scope (policy.rs)
  ↓ What operations can this session perform?
Layer 2: Sandbox Isolation (sandbox.rs)
  ↓ Is this process isolated from the host?
Layer 3: Seccomp Profiles (sandbox.rs)
  ↓ Which syscalls can this process make?
Layer 4: Network Policy (netfilter.rs)
  ↓ Which domains is this process allowed to access? (policy declaration)
Layer 5: Credential Access Control (credential.rs)
  ↓ Which secrets can this session read?
Layer 6: Audit Trail (audit.rs)
  ↓ What did this session actually do?
```

### Threat Model

| Threat | Mitigation |
|--------|------------|
| Agent escalates privileges | Tier inheritance prevents escalation; child tier >= parent tier |
| Agent accesses unauthorized files | Scope enforcement limits filesystem access |
| Agent runs dangerous syscalls | Seccomp-bpf profiles block system-level syscalls |
| Agent exfiltrates data | Network policy (netfilter) declares allowed outbound access |
| Agent reads other agents' secrets | Credential store enforces tier-based access |
| Agent causes resource exhaustion | Sandbox cgroup limits (mem, CPU, PIDs, timeout) |
| Agent makes undetectable changes | Checkpoint diff shows all modifications |
| Agent denies its actions | Audit trail logs everything with timestamps |
| Agent gets stuck in infinite loop | Rapid respawn detection + timeout enforcement |
| Compromised agent attacks host | Namespace isolation (PID, mount, network) |

### Credential Security

- Stored encrypted with **AES-256-GCM** (key derived from machine-id via SHA-256)
- 12-byte random nonce per encryption (from `/dev/urandom`, timestamp fallback)
- File permissions set to `0600` (owner-only read/write)
- Access controlled by tier (a tier 2 session cannot read a tier 0 credential)
- **Namespace isolation** for multi-tenant/multi-agent environments
- **TTL support** — credentials can auto-expire
- **Bundles** — load groups of related credentials in a single call
- Values never appear in `cos credential list` output
- API keys in command args are automatically redacted in audit logs
- Backward compatible with legacy credentials (auto-detected and decrypted)

### Elevation Audit

Every privilege elevation is:
1. Stored on disk with reason and expiry time
2. Visible via `cos policy status`
3. Logged in the audit trail
4. Automatically expired (max 1 hour)

---

## Example Workflows

### Safe Code Refactoring

```bash
# 1. Snapshot current state
cos checkpoint create "before refactor"

# 2. Agent performs risky changes
cos app fs write /den/src/main.py --content "..."
cos app fs rm /den/src/legacy.py
cos app exec run "python -m pytest"

# 3. Check what changed
cos checkpoint diff
# → {"created": [...], "modified": ["src/main.py"], "deleted": ["src/legacy.py"]}

# 4a. Tests pass → keep changes
# 4b. Tests fail → instant rollback
cos checkpoint rollback
```

### Multi-Agent Parallel Work

```bash
# Orchestrator spawns workers with restricted permissions
cos proc spawn --session worker-1 --group research --tier 2 --scope /den/research -- python search_api.py "topic A"
cos proc spawn --session worker-2 --group research --tier 2 --scope /den/research -- python search_api.py "topic B"
cos proc spawn --session worker-3 --group research --tier 2 --scope /den/research -- python search_api.py "topic C"

# Wait for all to finish
cos proc wait --group research --timeout 300

# Collect results
cos proc result worker-1
cos proc result worker-2
cos proc result worker-3

# Clean up
cos proc kill --group research
```

### Sandboxed Untrusted Code

```bash
# Agent received code from external source — run it safely
cos sandbox exec \
  --no-network \
  --mem 256M \
  --timeout 30 \
  --pids 50 \
  --seccomp-profile minimal \
  --ro \
  -- python /den/untrusted_script.py

# Result includes exit code, stdout, stderr, and whether it was killed (OOM, timeout)
```

### Service-Oriented Architecture

```bash
# Register and start a custom service
cos service register --name api --command "python /den/api/server.py" --workdir /den/api --health-url http://localhost:8000/health
cos service start api

# Monitor it
cos watch on service.health-fail --name api --timeout 3600

# If health fails, the watch returns and the agent can investigate
cos service logs api --tail 50
cos service restart api
```

### Agent Service with Lifecycle Management

```bash
# Register a service with full lifecycle hooks
cos service register \
  --name onevalet \
  --command "python -m onevalet" \
  --workdir /den/onevalet \
  --health-url http://localhost:8000/health \
  --pre-start "python -m alembic upgrade head" \
  --pre-stop "python drain.py" \
  --checkpoint-cmd "python save_state.py" \
  --drain-timeout 10 \
  --stop-timeout 30

# Start with pre-start hook (runs migration first)
cos service start onevalet

# Schedule periodic health monitoring
cos cron add health-monitor \
  --schedule "*/5 * * * *" \
  --command "cos service health onevalet" \
  --overlap skip

# Watch for multiple failure signals simultaneously
cos watch multi \
  --service onevalet \
  --file /den/onevalet/config.yaml \
  --timeout 3600

# Graceful shutdown with state preservation
cos service stop onevalet
# → checkpoint → pre_stop (drain) → SIGTERM → wait → post_stop
```

---

## Configuration Reference

### /etc/cos/config.json

```json
{
  "version": "0.1.0",
  "den": "/den",
  "exec": {
    "timeout": 300,
    "shell": "/bin/bash"
  },
  "net": {
    "timeout": 30,
    "allow_outbound": true
  },
  "web": {
    "reader_url": "http://localhost:3000",
    "timeout": 30,
    "max_content_length": 50000
  }
}
```

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `COS_DATA_DIR` | `/var/lib/cos` | Runtime state directory |
| `COS_APPS_DIR` | `/usr/lib/cos/apps` | Python app directory |
| `COS_SERVICES_DIR` | `/usr/lib/cos/services` | Service definitions |
| `COS_CONFIG_PATH` | `/etc/cos/config.json` | Configuration file |
| `COS_VERSION` | (from Cargo.toml) | OS version |
| `COS_SESSION` | (unset) | Current session ID (set by proc spawn) |
| `COS_BROWSER_URL` | `http://localhost:3000` | Browser engine URL |
| `DEN` | `/den` | Agent workspace path |

---

## Design Decisions Worth Noting

### 1. No Background Daemon

Many systems (systemd, Docker, Kubernetes) rely on long-running daemons. Claw OS deliberately avoids this:
- Every `cos` invocation is **stateless** (reads from disk, modifies, writes back)
- No PID file management for the supervisor itself
- No socket connections to maintain or reconnect
- Crash recovery is trivial (just run the next command)

This trades some performance (disk I/O per call) for simplicity and reliability.

### 2. JSON Files as Database

All state (process registry, IPC queues, lock files, credentials, netfilter rules) is stored as JSON files. This is intentionally simple:
- No database to install, configure, or crash
- Human-readable and debuggable
- Git-compatible (can checkpoint/diff state)
- Atomic enough for single-agent use (file rename is atomic on most filesystems)

For multi-agent concurrent access, the IPC primitives (locks, barriers) provide coordination.

### 3. Python Apps as User-Space Extensions

The Rust/Python split follows the kernel/userspace boundary concept:
- **Rust core** = kernel (stable, fast, security-critical)
- **Python apps** = userspace (extensible, ecosystem-rich, replaceable)

Apps can be added, removed, or updated without recompiling the core binary. The bridge protocol is simple: spawn subprocess, pass JSON, read JSON.

### 4. OverlayFS Over Git for Snapshots

Git is powerful but slow for snapshot/rollback:
- `git stash` requires staging
- `git checkout .` touches every file
- `.git` directory grows with history

OverlayFS is instant:
- Checkpoint = rename a directory
- Rollback = delete a directory and rename another
- No history accumulation (checkpoints are independent)
- Works with binary files, symlinks, permissions — everything

### 5. inotify with Polling Fallback for Watch

`cos watch` uses Linux's inotify for efficient, event-driven file change detection on Linux. On non-Linux platforms, it falls back to stat-based polling (500ms interval).

The `watch multi` command aggregates events from multiple sources (files, directories, processes, services) in a single call — something neither inotify nor polling alone provides. This multi-source aggregation is the agent-native evolution: agents don't want to manage multiple watchers, they want a single call that returns when *anything* relevant changes.

Event history is automatically logged to `$COS_DATA_DIR/watch/history.jsonl`, enabling agents to review past events for debugging or pattern detection.

### 6. Profile.sh as Agent-Native Default

Instead of modifying individual tool configs, Claw OS sets environment variables that universally suppress interactive behavior:

```bash
DEBIAN_FRONTEND=noninteractive    # apt won't ask questions
GIT_TERMINAL_PROMPT=0             # git won't prompt for credentials
CI=true                           # many tools detect CI and suppress prompts
PAGER=cat                         # no interactive paging
PIP_NO_INPUT=1                    # pip won't ask questions
NPM_CONFIG_YES=true               # npm auto-confirms
```

This is a **single point of configuration** that makes every tool in the system agent-compatible.

### 7. Recovery Hints as First-Class Output

Error messages in Claw OS are not strings — they are structured data with actionable commands:

```json
{
  "error": "No space left on device",
  "recovery": {
    "hint": "Disk full. Free space before retrying.",
    "try": ["cos sys resources", "cos app exec run 'du -sh /den/* | sort -rh | head'"]
  }
}
```

Every `try` command starts with `cos ` — the agent can pipe them directly back into the system. This creates a closed-loop error recovery path that doesn't require human intervention.
