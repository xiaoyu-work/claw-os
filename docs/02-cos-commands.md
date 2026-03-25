# Part 2: cos Command Reference

`cos` is the Claw OS supervisor — a single static Rust binary at `/usr/local/bin/cos`. All built-in commands are compiled into this binary. No external daemons required.

**Usage pattern:**
```
cos <app> <command> [args...]
cos                            # list all available apps
cos <app>                      # show app help and available commands
```

All commands return JSON to stdout. Errors return JSON with an `"error"` key and optional `"recovery"` hints.

---

## sys — System Information

Structured access to hardware, OS, environment, and kernel state. Replaces reading `/proc/*` and `/sys/*` text files.

### sys info

Get OS identity and runtime information.

```bash
cos sys info
```
```json
{
  "name": "claw-os",
  "version": "0.3.0",
  "platform": "linux",
  "arch": "x86_64",
  "hostname": "claw-container",
  "pid": 1
}
```

### sys env [pattern]

List environment variables, optionally filtered by substring.

```bash
cos sys env COS
```
```json
{
  "env": {
    "COS_VERSION": "0.3.0",
    "COS_DATA_DIR": "/var/lib/cos",
    "COS_DEN": "/den"
  },
  "count": 3
}
```

### sys resources

Show disk, memory, and CPU usage.

```bash
cos sys resources
```
```json
{
  "disk": {"path": "/den", "total_mb": 50000, "used_mb": 1200, "free_mb": 48800},
  "memory": {"total_mb": 8192, "used_mb": 2048, "available_mb": 6144}
}
```

### sys uptime

Show system uptime.

```bash
cos sys uptime
```
```json
{"uptime_seconds": 86400, "formatted": "1d 0h 0m"}
```

### sys proc

List all running processes with structured resource information. Equivalent to reading every `/proc/*/stat`.

```bash
cos sys proc
```
```json
{
  "processes": [
    {"pid": 1, "name": "cos-init", "state": "sleeping", "cpu_ticks": 150, "cpu_ms": 1500, "virtual_bytes": 8388608, "rss_bytes": 4096000},
    {"pid": 42, "name": "python3", "state": "running", "cpu_ticks": 98000, "cpu_ms": 980000, "virtual_bytes": 134217728, "rss_bytes": 67108864}
  ],
  "count": 2
}
```

### sys mounts

List all mount points. Equivalent to `/proc/mounts`.

```bash
cos sys mounts
```
```json
{
  "mounts": [
    {"device": "overlay", "mount_point": "/den", "filesystem": "overlay", "options": "lowerdir=...,upperdir=..."},
    {"device": "/dev/sda1", "mount_point": "/", "filesystem": "ext4", "options": "rw,relatime"}
  ],
  "count": 2
}
```

### sys net

Show network interfaces and TCP connections. Equivalent to `/proc/net/dev` + `/proc/net/tcp`.

```bash
cos sys net
```
```json
{
  "interfaces": [
    {"name": "eth0", "rx_bytes": 1048576, "rx_packets": 1024, "tx_bytes": 524288, "tx_packets": 512}
  ],
  "tcp_connections": [
    {"local": "0100007F:0BB8", "remote": "00000000:0000", "state": "LISTEN"}
  ],
  "tcp_count": 1
}
```

### sys cgroup

Show cgroup v2 resource limits and usage. Equivalent to `/sys/fs/cgroup/`.

```bash
cos sys cgroup
```
```json
{
  "memory": {"current_bytes": 134217728, "max_bytes": 536870912, "current_mb": 128, "max_mb": 512},
  "cpu": {"usage_usec": 5000000, "system_usec": 1000000},
  "pids": {"current": 15, "max": 100}
}
```

---

## proc — Process Session Manager

Spawn, track, control, and monitor processes by named session. Every process runs in a tracked session with buffered output.

### proc spawn

Start a process in a tracked session.

```bash
cos proc spawn [options] -- <command> [args...]
```

**Options:**
| Flag | Description |
|------|-------------|
| `--session <id>` | Custom session ID (default: auto-generated) |
| `--group <name>` | Add to a named group (for bulk operations) |
| `--parent <id>` | Declare parent session (enforces tier/scope inheritance) |
| `--workdir <path>` | Working directory |
| `--workspace isolated` | Create isolated workspace directory |
| `--tier <0-3>` | Permission tier (0=ROOT, 3=OBSERVE) |
| `--scope <path>` | Restrict operations to this path |
| `--priority <level>` | Process priority: `low`, `normal`, `high`, `realtime` |

```bash
cos proc spawn --session build-1 --group ci --tier 1 --priority high -- cargo build --release
```
```json
{
  "session_id": "build-1",
  "pid": 12345,
  "command": ["cargo", "build", "--release"],
  "started_at": "2026-03-23T10:00:00Z",
  "group": "ci",
  "tier": 1,
  "priority": "high"
}
```

**Guardrails included:**
- Rapid respawn detection (5+ identical spawns in 60 seconds triggers warning)
- Destructive command detection (`rm -rf /`, `mkfs`, `dd if=`) triggers warning

### proc status

Check if a session's process is still running.

```bash
cos proc status <session-id>
```
```json
{"session_id": "build-1", "pid": 12345, "status": "running", "started_at": "2026-03-23T10:00:00Z"}
```

### proc output

Read buffered stdout/stderr from a session.

```bash
cos proc output <session-id> [--tail N] [--stream stdout|stderr|both] [--follow] [--since-offset BYTES]
```

- `--tail N` — Last N lines only
- `--follow` — Block until process exits, then return all output
- `--since-offset BYTES` — Incremental read from byte offset (for polling)

### proc kill

Terminate a session or an entire group.

```bash
cos proc kill <session-id>
cos proc kill --group <name>        # kill all sessions in group
```

### proc list

List all sessions.

```bash
cos proc list [--group <name>]
```
```json
{
  "sessions": [
    {"session_id": "build-1", "pid": 12345, "status": "running", "group": "ci"},
    {"session_id": "test-1", "pid": 12346, "status": "exited", "group": "ci"}
  ],
  "count": 2
}
```

### proc wait

Block until a session or group exits.

```bash
cos proc wait <session-id> [--timeout N]
cos proc wait --group <name> [--timeout N]
```

Returns exit status and output tail for each session.

### proc signal

Send a Unix signal to a session's process.

```bash
cos proc signal <session-id> <TERM|KILL|HUP|USR1|USR2|STOP|CONT>
```

### proc result

Get a comprehensive exit report with heuristic success detection.

```bash
cos proc result <session-id>
```
```json
{
  "session_id": "build-1",
  "status": "exited",
  "started_at": "2026-03-23T10:00:00Z",
  "ended_at": "2026-03-23T10:05:30Z",
  "duration_secs": 330,
  "stdout_bytes": 15234,
  "stderr_bytes": 0,
  "likely_success": true,
  "stdout_tail": "Build complete. 0 warnings.",
  "stderr_tail": ""
}
```

### proc stats

Get resource usage stats from `/proc/<pid>/`.

```bash
cos proc stats <session-id>
```
```json
{
  "session_id": "build-1",
  "pid": 12345,
  "alive": true,
  "cpu": {"user_ms": 12500, "system_ms": 3200, "total_ms": 15700},
  "memory": {"virtual_bytes": 268435456, "virtual_mb": 256, "rss_bytes": 134217728, "rss_mb": 128},
  "io": {"stdout_bytes": 15234, "stderr_bytes": 0, "read_bytes": 52428800, "write_bytes": 10485760},
  "threads": 8
}
```

### proc renice

Change a running process's priority.

```bash
cos proc renice <session-id> --priority <low|normal|high|realtime>
```

---

## checkpoint — OverlayFS Snapshot System

Instant workspace snapshot and rollback using OverlayFS.

### checkpoint create

Freeze current workspace changes into a named checkpoint.

```bash
cos checkpoint create "before dependency upgrade"
```
```json
{
  "id": "003",
  "description": "before dependency upgrade",
  "created_at": "2026-03-23T10:00:00Z",
  "files_changed": 42,
  "checkpoint_dir": "003-before-dependency-upgrade"
}
```

### checkpoint diff

Show what changed since the last checkpoint (or base).

```bash
cos checkpoint diff
```
```json
{
  "created": ["src/new_module.py", "tests/test_new.py"],
  "modified": ["requirements.txt", "src/main.py"],
  "deleted": ["src/old_module.py"],
  "total_changes": 5
}
```

### checkpoint rollback

Restore workspace to a checkpoint or base state.

```bash
cos checkpoint rollback          # reset to base (empty upper)
cos checkpoint rollback 002      # restore checkpoint 002
```

### checkpoint list

List all saved checkpoints.

```bash
cos checkpoint list
```

### checkpoint status

Show overlay mount state, pending changes, and disk usage.

```bash
cos checkpoint status
```
```json
{
  "overlay_mounted": true,
  "pending_changes": 12,
  "checkpoint_count": 3,
  "disk_usage": {"upper_mb": 45, "checkpoints_mb": 120, "total_mb": 165}
}
```

### checkpoint quota-set

Set filesystem quota for the upper layer.

```bash
cos checkpoint quota-set 2G
```

### checkpoint quota-status

Show current quota usage.

```bash
cos checkpoint quota-status
```
```json
{
  "quota_enabled": true,
  "limit_bytes": 2147483648,
  "limit_human": "2.0G",
  "used_bytes": 156000000,
  "used_human": "148.8M",
  "available_human": "1.9G",
  "percent_used": 7,
  "exceeded": false
}
```

### checkpoint namespaces

Manage independent overlay namespaces.

```bash
cos checkpoint namespaces                          # list all
cos checkpoint namespaces --create project-a       # create new namespace
cos checkpoint namespaces --status project-a       # show namespace details
cos checkpoint namespaces --destroy project-a      # remove namespace
```

---

## sandbox — Process Isolation

Lightweight namespace + cgroup isolation for untrusted code execution.

### sandbox exec

Run a command in an isolated environment.

```bash
cos sandbox exec [options] -- <command> [args...]
```

**Options:**
| Flag | Description |
|------|-------------|
| `--no-network` | Disable network access (network namespace isolation) |
| `--ro` | Read-only filesystem |
| `--workspace <dir>` | Working directory inside sandbox |
| `--mem <limit>` | Memory limit (e.g., `512M`, `1G`) |
| `--cpu <percent>` | CPU quota (e.g., `50` = 50%) |
| `--pids <max>` | Maximum number of processes |
| `--timeout <secs>` | Kill after N seconds |
| `--seccomp-profile <p>` | Syscall filter: `minimal`, `network`, `full` |

```bash
cos sandbox exec --no-network --mem 256M --timeout 30 --seccomp-profile minimal -- python untrusted.py
```
```json
{
  "exit_code": 0,
  "stdout": "result: 42\n",
  "stderr": "",
  "isolated": true,
  "network": false,
  "cgroup": true,
  "seccomp_profile": "minimal",
  "limits": {"memory": "256M", "timeout_secs": 30}
}
```

**Seccomp profiles:**
- `minimal` — blocks clock, debug, module, mount, raw-io, reboot, swap, privileged syscalls
- `network` — like minimal but allows networking syscalls
- `full` — no syscall restrictions

### sandbox create / destroy / list

Manage persistent sandbox configurations.

```bash
cos sandbox create --no-network --mode ro
cos sandbox list
cos sandbox destroy <id>
```

---

## ipc — Inter-Process Communication

File-based message queues, mutex locks, and synchronization barriers. No daemon required.

### ipc send / recv

Queue-based messaging between sessions.

```bash
cos ipc send <target-session> "task complete" --from orchestrator
cos ipc recv <session-id> [--timeout 30] [--peek]
```

### ipc list / clear

Manage a session's message queue.

```bash
cos ipc list <session-id>
cos ipc clear <session-id>
```

### ipc lock / unlock / locks

Named mutex locks with stale detection.

```bash
cos ipc lock shared-resource --holder agent-1 --timeout 10
cos ipc unlock shared-resource --holder agent-1
cos ipc locks       # list all active locks
```

If the holder's PID is dead, the lock is automatically reclaimed by the next caller.

### ipc barrier

Synchronization barrier — block until N sessions arrive.

```bash
cos ipc barrier sync-point --expect 3 --session agent-1 --timeout 60
```

### ipc pipe — Streaming Named Pipes

Agent-native evolution of Unix named pipes: structured NDJSON messages, multi-producer/consumer, replay, backpressure.

```bash
# Create a named channel
cos ipc pipe create my-events --buffer-size 500

# Publish structured messages
cos ipc pipe publish my-events '{"type":"progress","value":42}' --from worker-1
cos ipc pipe publish my-events "plain text message"

# Subscribe to messages (replay from history)
cos ipc pipe subscribe my-events --since 000003 --limit 10

# Follow mode (block until new messages arrive, like tail -f)
cos ipc pipe subscribe my-events --follow --timeout 30

# List all channels
cos ipc pipe list

# Remove a channel
cos ipc pipe destroy my-events
```

Features:
- **Structured**: Each message stored as JSON with ID, sender, timestamp
- **Multi-producer/consumer**: Any session can publish or subscribe
- **Replay**: `--since <id>` to catch up on history
- **Backpressure**: Configurable buffer size, oldest messages dropped when full
- **Discoverable**: `pipe list` shows all channels with metadata

---

## watch — Event Watcher

Event-driven file and process watching with multi-source aggregation and event history. Uses inotify on Linux for efficient notification (polling fallback on other platforms).

### watch file / dir / proc

Watch for file, directory, or process changes. Uses inotify on Linux for instant notification.

```bash
cos watch file /den/config.json --timeout 30
cos watch dir /den/src/ --timeout 60
cos watch proc <session-id> --timeout 120
```

### watch multi — Multi-Source Aggregation

Watch multiple sources simultaneously — returns when ANY source fires.

```bash
cos watch multi --file /den/main.py --dir /den/output/ --proc worker-1 --service my-api --timeout 60
```
```json
{
  "status": "triggered",
  "source": "file",
  "path": "/den/main.py",
  "event": "modified",
  "watched": {"files": ["/den/main.py"], "dirs": ["/den/output/"], "procs": ["worker-1"], "services": ["my-api"]}
}
```

### watch history — Event Log

View past watch events from the persistent event log.

```bash
cos watch history --limit 20
cos watch history --since "2026-03-25T10:00:00Z" --source file
```
```json
{
  "events": [
    {"timestamp": "2026-03-25T10:00:01Z", "source": "file", "path": "/den/main.py", "event": "modified"},
    {"timestamp": "2026-03-25T10:00:05Z", "source": "proc", "session": "worker-1", "event": "exited"}
  ],
  "count": 2
}
```

### watch on

Subscribe to OS-level events.

```bash
cos watch on <event-type> [--timeout N] [event-specific args]
```

**Event types:**

| Event | Args | Triggers When |
|-------|------|---------------|
| `proc.exit` | `--session <id>` | Process exits |
| `fs.change` | `--path <dir>` | Any file in directory changes |
| `service.health-fail` | `--name <svc>` | Service health check fails |
| `checkpoint.created` | (none) | New checkpoint is created |
| `quota.exceeded` | (none) | Filesystem quota is exceeded |
| `ipc.message` | `--session <id>` | New IPC message arrives |
| `credential.expired` | `--name <name>` | Credential TTL expires |

```bash
cos watch on proc.exit --session build-1 --timeout 600
```
```json
{
  "event": "proc.exit",
  "triggered": true,
  "details": {"status": "exited", "sessions": [{"session_id": "build-1", "pid": 12345, "status": "exited"}]}
}
```

---

## service — Service Manager

Declarative service lifecycle management with agent-native hooks: graceful shutdown, drain period, checkpoint-on-stop, dependency-ordered teardown.

### service start / stop / restart

```bash
cos service start my-api
cos service stop my-api       # graceful: checkpoint → pre_stop → drain → SIGTERM → wait → SIGKILL → post_stop
cos service restart my-api
```

Graceful stop returns step-by-step results:
```json
{
  "name": "my-api",
  "status": "stopped",
  "pid": 12345,
  "steps": [
    {"step": "checkpoint", "status": "ok", "duration_ms": 150},
    {"step": "pre_stop", "status": "ok", "duration_ms": 200},
    {"step": "drain", "duration_ms": 5000},
    {"step": "sigterm", "status": "sent"},
    {"step": "wait_exit", "status": "exited", "duration_ms": 1200},
    {"step": "post_stop", "status": "ok", "duration_ms": 50}
  ]
}
```

### service stop-all

Stop all services in reverse dependency order with graceful shutdown for each.

```bash
cos service stop-all
```

### service status / health

```bash
cos service status my-api
cos service health my-api              # auto-restarts if unhealthy
cos service health my-api --no-restart # check only
```

### service list / logs

```bash
cos service list
cos service logs my-api --tail 50
```

### service register

Create a new service with lifecycle hooks.

```bash
cos service register \
  --name my-api \
  --command "python app.py" \
  --workdir /den/api \
  --health-url http://localhost:8000/health \
  --pre-start "python migrate.py" \
  --pre-stop "python drain.py" \
  --post-stop "rm -rf /tmp/api-cache" \
  --checkpoint-cmd "python save_state.py" \
  --drain-timeout 10 \
  --stop-timeout 30
```

---

## browser — Browser Service

Manages the built-in Chromium browser engine (Puppeteer-based).

```bash
cos browser start      # start browser service
cos browser stop       # stop browser service
cos browser restart    # restart
cos browser status     # check running + healthy
cos browser health     # health check with auto-restart
```

The browser service powers `cos web read` (URL to Markdown conversion with full JavaScript rendering).

---

## credential — Secure Secret Storage

OS-level AES-256-GCM encrypted credential store with tier-based access, namespaces, TTL, and bundles.

### credential store

```bash
cos credential store <name> <value> [--tier N] [--namespace NS] [--ttl SECS]
```

```bash
cos credential store OPENAI_KEY "sk-abc123" --tier 0              # ROOT-only, never expires
cos credential store DB_PASSWORD "hunter2" --tier 1 --ttl 3600     # OPERATE+, expires in 1 hour
cos credential store API_KEY "key123" --namespace tenant-42         # isolated namespace
```

### credential load

```bash
cos credential load <name> [--namespace NS]
```
```json
{"name": "OPENAI_KEY", "namespace": "default", "value": "sk-abc123", "min_tier": 0}
```

Enforces tier check and TTL expiry. Returns error with `"expired": true` if credential has passed its TTL.

### credential revoke / list

```bash
cos credential revoke OPENAI_KEY [--namespace NS]
cos credential list                        # all namespaces with counts
cos credential list --namespace tenant-42  # credentials in specific namespace
```

### credential bundle / load-bundle

Group related credentials for bulk loading:

```bash
cos credential bundle openai-config --keys OPENAI_KEY,OPENAI_ORG [--namespace NS]
cos credential load-bundle openai-config [--namespace NS]
```
```json
{
  "bundle": "openai-config",
  "namespace": "default",
  "credentials": {"OPENAI_KEY": "sk-abc123", "OPENAI_ORG": "org-xyz"}
}
```

Missing or expired credentials in a bundle return partial results with an `"errors"` field.

---

## netfilter — Outbound Network Firewall

Domain-based allow/deny rules for outbound network access.

### netfilter add / remove

```bash
cos netfilter add --allow "api.openai.com" --port 443
cos netfilter add --allow "*.github.com"
cos netfilter add --deny "*.malware.com"
cos netfilter remove "*.malware.com"
```

### netfilter default / check / list / reset

```bash
cos netfilter default deny-all     # block all by default
cos netfilter check "api.openai.com"   # → {"allowed": true}
cos netfilter list                 # show all rules
cos netfilter reset                # clear all rules, revert to allow-all
```

---

## policy — Permission System

Manage trust tiers, check permissions, and temporarily elevate privileges.

### policy status

Show current session's permission state.

```bash
cos policy status
```
```json
{
  "session": "worker-1",
  "base_tier": 2,
  "effective_tier": 2,
  "tier_name": "CREATE",
  "allowed_operations": ["Read", "Write"]
}
```

### policy check

Test if a specific operation is allowed.

```bash
cos policy check exec
```
```json
{"operation": "exec", "allowed": false, "details": {"error": "permission denied", "tier": 2, "hint": "..."}}
```

### policy elevate

Temporarily escalate privileges (requires tier 0).

```bash
cos policy elevate --to 1 --duration 300 --reason "deployment needs delete access"
```
```json
{
  "elevated": true,
  "from_tier": 2,
  "to_tier": 1,
  "duration_secs": 300,
  "expires_at": "2026-03-23T10:05:00Z",
  "reason": "deployment needs delete access"
}
```

Maximum duration: 3600 seconds (1 hour). Automatically expires.

### policy drop

Revoke an active elevation.

```bash
cos policy drop
```

---

## cron — Agent-Native Job Scheduler

Schedule recurring jobs with execution context, structured result capture, and overlap protection. Unlike traditional crond, each job carries its own permission tier, scope, credentials, and timeout.

### cron add

```bash
cos cron add <id> --schedule "*/5 * * * *" --command "cos exec run 'python check.py'" \
    [--description "Health check"] [--tier 2] [--scope /den/project] \
    [--credentials OPENAI_KEY,DB_PASS] [--overlap skip|queue|kill|allow] \
    [--timeout 300]
```

**Overlap policies:**
| Policy | Behavior |
|--------|----------|
| `skip` (default) | Skip this run if previous is still running |
| `queue` | Wait for previous to finish, then run |
| `kill` | Kill previous run, start new one |
| `allow` | Run in parallel (traditional cron) |

### cron remove / enable / disable

```bash
cos cron remove my-job
cos cron enable my-job
cos cron disable my-job
```

### cron list / status

```bash
cos cron list
cos cron status my-job
```
```json
{
  "id": "my-job",
  "schedule": "*/5 * * * *",
  "enabled": true,
  "next_run": "2026-03-25T10:05:00Z",
  "last_run": {"status": "success", "duration_ms": 1200, "exit_code": 0}
}
```

### cron logs

```bash
cos cron logs my-job --limit 10
```

Shows execution history with stdout/stderr tails, exit codes, and durations.

### cron run

Manually trigger a job immediately (respects overlap policy).

```bash
cos cron run my-job
```

### cron tick

Process all due jobs. Called by an external scheduler (e.g., systemd timer) every minute:

```bash
cos cron tick
```
```json
{
  "processed": 5,
  "executed": [{"id": "health-check", "status": "success"}],
  "skipped": [{"id": "backup", "reason": "previous still running"}]
}
```
