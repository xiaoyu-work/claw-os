# Part 2: cos Command Reference

`cos` is the Claw OS supervisor — a single static Rust binary at `/usr/local/bin/cos`. All built-in commands are compiled into this binary. No external daemons required.

**OS Primitives** — `cos <primitive> <command>`
Core system operations implemented in Rust. Always available.

**Apps** — `cos app <name> <command>`
Higher-level tools implemented in Python. Extensible and replaceable.

**Usage pattern:**
```
cos <primitive> <command> [args...]    # OS primitive
cos app <name> <command> [args...]     # Python app
cos                                    # list OS primitives
cos app                                # list available apps
cos <primitive>                        # show primitive help
cos app <name>                         # show app help
```

All commands return JSON to stdout. Errors return JSON with an `"error"` key and optional `"recovery"` hints.

**Agent-only primitives** (process sessions, IPC, watching, tracing, sandbox, netfilter) are not user commands. The agent calls them through dedicated tools (`cos_proc`, `cos_ipc`, `cos_watch`, `cos_trace`, `cos_sandbox`, `cos_netfilter`). See the `skills/claw-os/` directory.

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
    "COS_HOME": "/home/cos"
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
  "disk": {"path": "/home/cos", "total_mb": 50000, "used_mb": 1200, "free_mb": 48800},
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
    {"device": "overlay", "mount_point": "/home/cos", "filesystem": "overlay", "options": "lowerdir=...,upperdir=..."},
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
  --workdir /home/cos/api \
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

Manages the optional `cos-browser` CDP server (Rust headless browser, vendored from [Obscura](https://github.com/h4ckf0r0day/obscura)).

```bash
cos browser start [--port 9222] [--stealth] [--proxy URL]   # start CDP server
cos browser stop                                            # stop
cos browser restart
cos browser status                                          # running + healthy
cos browser health                                          # health check with auto-restart
```

The CDP server is **opt-in** — it exposes Chrome DevTools Protocol at `ws://localhost:9222` for external Puppeteer/Playwright clients. `cos app web read|scrape|screenshot` does **not** require it; those commands invoke `cos-browser` per call.

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

## cron — Agent-Native Job Scheduler

Schedule recurring jobs with execution context, structured result capture, and overlap protection. Unlike traditional crond, each job carries its own permission tier, scope, credentials, and timeout.

### cron add

```bash
cos cron add <id> --schedule "*/5 * * * *" --command "cos app exec run 'python check.py'" \
    [--description "Health check"] [--tier 2] [--scope /home/cos/project] \
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
