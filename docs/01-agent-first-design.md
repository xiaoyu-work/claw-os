# Part 1: Agent-First Design & Linux Internals

## Why a New OS?

Linux, macOS, and Windows were built for humans. Their interfaces — terminal text, GUI windows, exit codes — are optimized for human reading speed and pattern recognition. When an AI agent runs `ls -la`, it receives a block of whitespace-aligned text that it must **parse back into structured data**. This is wasteful and error-prone.

Claw OS starts from a different question: **what would an OS look like if its only users were autonomous AI agents?**

The answer is a system where every interface is machine-native: structured JSON output, declarative permission tiers instead of uid/rwx, instant checkpoint/rollback instead of "hope nothing breaks", and automatic audit trails instead of optional logging.

Claw OS is **not a new kernel**. It is a purpose-built Linux userspace — a minimal Debian rootfs with a Rust supervisor (`cos`) that wraps and extends standard Linux primitives, re-exposing them as agent-friendly JSON interfaces.

---

## Core Design Principles

### 1. Structured I/O Everywhere

Every command in Claw OS returns JSON. There are zero exceptions.

**Traditional Linux:**
```
$ ls -la /den
total 16
drwxr-xr-x 3 root root 4096 Mar 23 10:00 .
drwxr-xr-x 1 root root 4096 Mar 23 09:30 ..
-rw-r--r-- 1 root root  256 Mar 23 10:00 main.py
```

**Claw OS:**
```json
{
  "path": "/den",
  "entries": [
    {"name": "main.py", "type": "file", "size": 256, "modified": "2026-03-23T10:00:00Z", "permissions": "rw-r--r--"}
  ],
  "count": 1
}
```

An agent can directly access `result.entries[0].size` — no regex, no line splitting, no column guessing.

### 2. Actionable Error Recovery

When something fails, a human debugs by intuition. An agent needs explicit guidance.

**Traditional Linux:**
```
$ rm /etc/passwd
rm: cannot remove '/etc/passwd': Permission denied
```

**Claw OS:**
```json
{
  "error": "Permission denied on /etc/passwd",
  "recovery": {
    "hint": "Permission denied. Check file permissions.",
    "try": ["cos app exec run 'ls -la /etc/passwd'", "cos app exec run 'chmod +rw /etc/passwd'"]
  }
}
```

The `recovery.try` array contains literal commands the agent can execute to resolve the issue. This turns error handling from a guessing game into a structured decision tree.

### 3. No Interactive Prompts

A human can answer "Do you want to continue? [Y/n]". An agent cannot. Claw OS suppresses all interactive behavior at the environment level:

```bash
# /etc/cos/profile.sh
export DEBIAN_FRONTEND=noninteractive
export GIT_TERMINAL_PROMPT=0
export CI=true
export PAGER=cat
export GIT_PAGER=cat
export PIP_NO_INPUT=1
export NPM_CONFIG_YES=true
export PYTHONDONTWRITEBYTECODE=1
```

Every process spawned by `cos proc spawn` inherits these variables. No tool ever blocks waiting for human input.

### 4. Trust Tiers, Not Unix Permissions

Unix permissions (uid/gid/rwx) answer "which **human** can access this file?" Claw OS permissions answer "what **operations** can this agent perform?"

| Tier | Name | Allowed Operations |
|------|------|--------------------|
| 0 | ROOT | Read, Write, Delete, Exec, Net, System |
| 1 | OPERATE | Read, Write, Delete, Exec |
| 2 | CREATE | Read, Write |
| 3 | OBSERVE | Read |

Combined with **path-based scopes**, this creates a fine-grained capability model:

```bash
# Agent spawned with tier 2, scoped to /den/project-a
cos proc spawn --tier 2 --scope /den/project-a --session worker-1 -- python task.py

# worker-1 CAN: read and write files under /den/project-a
# worker-1 CANNOT: delete files, execute commands, access /den/project-b, or make network requests
```

**Inheritance rules:**
- Child processes inherit the parent's tier (cannot escalate)
- Child scopes must be equal to or narrower than the parent's scope
- These constraints are enforced at spawn time, not at runtime (fail-fast)

### 5. Sessions, Not Just PIDs

Linux PIDs are opaque integers that get recycled. An agent that spawns `pid 42358` has no meaningful handle for later interaction.

Claw OS wraps every process in a **session** — a named, persistent, queryable entity:

```bash
cos proc spawn --session build-1 --group ci --parent orchestrator -- cargo build
```

Sessions have:
- **Stable names** (survive `cos` restarts via on-disk registry)
- **Group membership** (kill an entire group at once)
- **Parent-child relationships** (with tier/scope inheritance)
- **Buffered output** (queryable without attaching to the process)
- **Resource stats** (CPU time, memory, I/O from `/proc/<pid>/`)
- **Priority control** (nice values via `--priority low|normal|high|realtime`)

### 6. Reversible Workspace

Traditional development: if an agent makes a mistake, you run `git checkout .` or rebuild from scratch. Both are slow and lossy.

Claw OS provides **instant, OS-level checkpoint/rollback** via OverlayFS:

```bash
cos checkpoint create "before risky refactor"
# ... agent modifies files ...
cos checkpoint diff      # see what changed
cos checkpoint rollback  # instant undo — all changes gone
```

This happens at the filesystem layer. No git commands, no file copies, no external tools. Sub-second for any number of files.

### 7. Audit Everything, Automatically

Every `cos` command is logged to a JSONL audit trail. The agent doesn't need to opt in.

```json
{"timestamp":"2026-03-23T10:15:30Z","app":"fs","command":"write","args":["--content","..."],"duration_ms":12,"status":"ok"}
{"timestamp":"2026-03-23T10:15:31Z","app":"exec","command":"run","args":["python","train.py"],"duration_ms":45200,"status":"ok"}
{"timestamp":"2026-03-23T10:15:32Z","app":"net","command":"fetch","args":["--url","https://api.example.com","Authorization: ***REDACTED***"],"duration_ms":340,"status":"ok"}
```

Sensitive values (Bearer tokens, API keys, Authorization headers) are automatically redacted before logging. The agent can later query this log:

```bash
cos app log search --app exec --status error  # find all failed command executions
cos app log tail 20                            # last 20 entries
```

---

## Linux Internals: What Changed

Claw OS is built on a standard Debian Bookworm rootfs. Here's what's modified and why.

### Rootfs Construction

The rootfs is bootstrapped via `rootfs/build.sh`:

1. **debootstrap** a minimal Debian Bookworm into `/build/claw-os-rootfs`
2. **Node.js 24** installed (for Chromium browser engine)
3. **System packages** installed from `rootfs/packages.txt`:
   - Core: `bash`, `coreutils`, `git`, `ripgrep`, `python3`, `python3-pip`, `curl`, `sqlite3`, `jq`
   - Browser: `chromium` + ~15 dependency packages (libnss3, libxss1, libasound2, etc.)
4. **Python packages**: `pymupdf`, `python-docx`, `openpyxl`, `pyyaml`
5. **Overlay applied**: config files, cos-init script, service definitions
6. **`cos` binary** (Rust, musl-static) copied to `/usr/local/bin/cos`
7. **Python apps** copied to `/usr/lib/cos/apps/`
8. **Browser engine** (Puppeteer-based) installed in `/opt/cos-browser-engine/`

### OverlayFS: The Checkpoint Foundation

Claw OS mounts the agent's workspace (`/den`) as an OverlayFS:

```
/den (merged view — what the agent sees)
  ├── lower = /var/lib/cos/overlay/base       (read-only original state)
  ├── upper = /var/lib/cos/overlay/upper       (copy-on-write modifications)
  └── work  = /var/lib/cos/overlay/work        (overlayfs internal)
```

**How it works:**
- When the agent **reads** a file, OverlayFS checks upper first, then lower
- When the agent **writes** a file, OverlayFS copies it to upper on first write (copy-on-write)
- When the agent **deletes** a file, OverlayFS creates a whiteout marker (character device 0,0) in upper

**Checkpoint = freeze the upper layer:**
1. Unmount overlay
2. Move `upper/` to `checkpoints/NNN-description/layer/`
3. Create fresh empty `upper/`
4. Remount overlay

**Rollback = restore a frozen layer:**
1. Unmount overlay
2. Delete current `upper/`
3. Copy checkpoint's `layer/` back as `upper/`
4. Remount overlay

**Diff = walk the upper layer:**
- Regular file in upper AND in base → **modified**
- Regular file in upper but NOT in base → **created**
- Character device (0,0) in upper → **deleted** (whiteout)

**Multi-namespace support:**
Each namespace is an independent overlay stack under `/var/lib/cos/overlay-namespaces/<name>/`, with its own base, upper, work, and checkpoints. This allows multiple isolated filesystems (e.g., for parallel sandbox runs).

**Quota enforcement:**
A configurable byte limit on the upper layer prevents unbounded disk growth:
```bash
cos checkpoint quota-set 2G
cos checkpoint quota-status  # → {"used_bytes": 156000000, "limit_bytes": 2147483648, "exceeded": false}
```

### Linux Namespaces: Sandbox Isolation

`cos sandbox exec` uses Linux namespace isolation:

```bash
cos sandbox exec --no-network --mem 512M --timeout 60 -- python untrusted.py
```

Under the hood:
1. **`unshare --pid --fork --mount-proc --mount [--net]`** creates isolated PID, mount, and optionally network namespaces
2. **`systemd-run --scope`** (when resource limits are requested) creates a transient cgroup v2 scope with:
   - `MemoryMax=512M` + `MemorySwapMax=0`
   - `CPUQuota=50%`
   - `TasksMax=100`
   - `RuntimeMaxSec=60`
3. **seccomp-bpf profiles** (via `systemd-run -p SystemCallFilter=`) restrict syscalls:
   - `minimal`: blocks `@clock @debug @module @mount @obsolete @raw-io @reboot @swap @privileged`
   - `network`: blocks the above except allows networking syscalls
   - `full`: no restrictions

Exit code 137 is detected as OOM kill; exit code 124 as timeout.

### Process Management: Beyond fork/exec

`cos proc` wraps standard process spawning with agent-aware tracking:

1. **`setsid()`** — each spawned process becomes a session leader (allows group signals)
2. **Stdout/stderr redirected to files** — buffered, capped at 2MB, queryable later
3. **Registry persisted to disk** (`/var/lib/cos/proc/registry.json`) — survives `cos` restarts
4. **`std::mem::forget(child)`** — process outlives the `cos` invocation (detached)
5. **Priority via `nice -n`** — low (10), normal (0), high (-5), realtime (-10)
6. **Resource stats from `/proc/<pid>/`**:
   - `/proc/<pid>/stat` → CPU ticks (user + system), virtual memory, RSS
   - `/proc/<pid>/io` → read/write bytes
   - `/proc/<pid>/status` → thread count

### Structured /proc Exposure

Linux's `/proc` and `/sys` filesystems contain valuable system state but in inconsistent, text-based formats. Claw OS re-exposes them as structured JSON:

| Command | Linux Source | What It Returns |
|---------|-------------|-----------------|
| `cos sys proc` | `/proc/*/stat` | All processes: pid, name, state, cpu_ticks, memory |
| `cos sys mounts` | `/proc/mounts` | All mount points: device, path, filesystem, options |
| `cos sys net` | `/proc/net/dev`, `/proc/net/tcp` | Interfaces (rx/tx bytes) + TCP connections (state) |
| `cos sys cgroup` | `/sys/fs/cgroup/` | Memory/CPU/PID limits and current usage |
| `cos sys resources` | `/proc/meminfo`, `statvfs()` | Disk and memory usage |
| `cos sys uptime` | `/proc/uptime` | System uptime (seconds + formatted) |

### Container Entrypoint: cos-init

`/usr/local/bin/cos-init` is the container's ENTRYPOINT:

```bash
#!/bin/bash
source /etc/cos/profile.sh

# Set up OverlayFS workspace (Linux only)
if [ "$(uname)" = "Linux" ]; then
    mkdir -p /var/lib/cos/overlay/{base,upper,work}
    cp -a /den/* /var/lib/cos/overlay/base/ 2>/dev/null || true
    mount -t overlay overlay \
        -o lowerdir=/var/lib/cos/overlay/base,upperdir=/var/lib/cos/overlay/upper,workdir=/var/lib/cos/overlay/work \
        /den
fi

# Start browser engine service
cos service start browser 2>/dev/null || true

# Execute the requested command (default: bash --login)
exec "$@"
```

This ensures:
1. Agent-native environment variables are set
2. OverlayFS is mounted on `/den` (enables checkpoint/rollback)
3. Browser service is available for web operations
4. The requested workload runs as PID 1 in the container

### Network Firewall

`cos netfilter` provides declarative outbound network control:

```bash
cos netfilter default deny-all                          # block everything by default
cos netfilter add --allow "api.openai.com" --port 443   # allow specific API
cos netfilter add --allow "*.github.com"                # wildcard support
cos netfilter add --deny "*.malware.com"                # explicit deny
cos netfilter check "api.openai.com"                    # → {"allowed": true}
```

Rules are stored in `/var/lib/cos/netfilter/rules.json` and checked with simple domain matching (exact match + wildcard prefix `*.example.com`).

### Credential Store

`cos credential` provides AES-256-GCM encrypted secret storage with agent-native features:

```bash
cos credential store OPENAI_KEY "sk-..." --tier 0            # AES-256-GCM encrypted, ROOT only
cos credential store DB_PASS "hunter2" --tier 1 --ttl 3600    # expires in 1 hour
cos credential store TENANT_KEY "abc" --namespace tenant-123   # namespace isolation
cos credential load OPENAI_KEY                                 # tier check + expiry enforced
cos credential bundle openai-config --keys OPENAI_KEY,OPENAI_ORG  # group credentials
cos credential load-bundle openai-config                       # load entire bundle at once
```

Credentials are encrypted with AES-256-GCM (key derived from machine-id via SHA-256), stored with restrictive file permissions (`0600`), and access-controlled by the tier system. Namespaces provide per-tenant/per-agent isolation. TTL enables automatic credential expiry. Bundles let agents load groups of related credentials in a single call.

### Temporary Privilege Elevation

`cos policy elevate` provides time-limited `sudo`:

```bash
cos policy elevate --to 1 --duration 300 --reason "deployment requires delete access"
# Session now has tier 1 for 5 minutes
# After 300 seconds, automatically drops back to original tier
```

Elevation grants are stored on disk and checked against expiry time. The audit log captures both the elevation and the reason.

---

## Summary

Claw OS takes standard Linux primitives — OverlayFS, namespaces, cgroups, `/proc`, `nice`, process groups — and re-exposes them through an agent-native interface. It doesn't replace the kernel; it replaces the **interaction paradigm**. Every output is structured, every error is actionable, every operation is audited, every change is reversible.

The result is an OS where agents can work autonomously, safely, and transparently — without ever needing a human to read terminal output, answer a prompt, or debug a cryptic error message.
