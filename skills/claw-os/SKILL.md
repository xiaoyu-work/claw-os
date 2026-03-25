---
name: claw-os
description: "Native Claw OS system commands. Use when: you need sandboxed execution, process management, IPC (messages, locks, streaming pipes), checkpoints, service lifecycle management, credential management, job scheduling, file watching, web search, email, calendar, or system information. You are running on Claw OS — use cos commands for structured JSON output. OS primitives: cos <name>. Apps: cos app <name>."
metadata: { "openclaw": { "emoji": "🦀", "requires": { "bins": ["cos"] } } }
---

# Claw OS

You are running on Claw OS. All `cos` commands return JSON.

## Command Structure

OS primitives and apps are in separate namespaces:

```bash
cos <primitive> <command>         # OS primitives (sys, proc, checkpoint, etc.)
cos app <name> <command>          # Apps (fs, web, search, email, etc.)
cos                               # List OS primitives
cos app                           # List available apps
```

## Checkpoints (Undo / Rollback)

The workspace is mounted with OverlayFS. Every file change is captured automatically — regardless of how it's made. You can snapshot, diff, and rollback at any time:

```bash
cos checkpoint create "before refactoring"
cos checkpoint diff
cos checkpoint rollback
cos checkpoint rollback 001
cos checkpoint list
cos checkpoint status
```

`cos checkpoint diff` shows all files created, modified, or deleted since the last checkpoint — without scanning or comparing files manually. `cos checkpoint rollback` reverts the entire workspace instantly.

## Sandboxed Execution

Run untrusted code in an isolated environment with resource limits:

```bash
cos sandbox exec --mem 512M --cpu 50 --timeout 300 --no-network -- python3 script.py
cos sandbox exec --timeout 60 -- node app.js
cos sandbox exec --mem 1G --pids 100 -- bash -c "make && ./test"
```

Flags: `--mem` (memory limit), `--cpu` (percent), `--pids` (max processes), `--timeout` (seconds), `--no-network` (disable network).

## Process Sessions

Spawn background processes tracked by session ID. Output is buffered and queryable:

```bash
cos proc spawn --session build-1 -- npm run build
cos proc status build-1
cos proc output build-1 --tail 50
cos proc output build-1 --stream stderr
cos proc kill build-1
cos proc list
```

### Process Groups and Hierarchy

Organize related processes into groups. Child processes inherit parent context:

```bash
cos proc spawn --group research --session search-1 -- search.py "topic A"
cos proc spawn --group research --session search-2 -- search.py "topic B"
cos proc spawn --parent lead --session sub-1 -- worker.py
cos proc list --group research
cos proc kill --group research
```

### Wait, Signal, and Result

Wait for processes to finish, send signals, and get one-call result summaries:

```bash
cos proc wait build-1 --timeout 300
cos proc wait --group research
cos proc signal build-1 TERM
cos proc result build-1
```

`cos proc result` returns a comprehensive summary: status, duration, output tails, output sizes, and a `likely_success` heuristic — everything an agent needs in one call.

### Output Streaming

Read output incrementally without re-reading old content:

```bash
cos proc output build-1 --follow
cos proc output build-1 --since-offset 4096
```

### Isolated Workspaces

Give each process its own private workspace directory:

```bash
cos proc spawn --workspace isolated --session task-1 -- agent.py
```

## Permission Tiers

Control what a process can do. Tier 0 is highest privilege, tier 3 is read-only:

| Tier | Name    | Allowed Operations                    |
|------|---------|---------------------------------------|
| 0    | ROOT    | Read, Write, Delete, Exec, Net, System |
| 1    | OPERATE | Read, Write, Delete, Exec             |
| 2    | CREATE  | Read, Write                           |
| 3    | OBSERVE | Read                                  |

```bash
cos proc spawn --tier 3 --session reader-1 -- analyze.py
cos proc spawn --tier 1 --scope /den/project --session builder-1 -- build.py
cos proc spawn --tier 2 --scope /den/output --parent lead --session writer-1 -- report.py
```

Child processes cannot escalate beyond parent's tier or widen parent's scope.

## Credential Store

Secure AES-256-GCM encrypted storage for API keys, tokens, and secrets:

```bash
cos credential store OPENAI_KEY "sk-..." --tier 0
cos credential store DB_URL "postgresql://..." --tier 1 --ttl 3600
cos credential store TENANT_KEY "abc" --namespace tenant-42
cos credential load OPENAI_KEY
cos credential list
cos credential list --namespace tenant-42
cos credential revoke OPENAI_KEY
```

### Bundles

Group related credentials for bulk loading:

```bash
cos credential bundle openai-config --keys OPENAI_KEY,OPENAI_ORG
cos credential load-bundle openai-config
```

Credentials are auto-injected into services registered with `--credentials`:
```bash
cos service register --name my-agent --command "python agent.py" --credentials OPENAI_KEY,DB_URL
cos service start my-agent   # OPENAI_KEY and DB_URL injected as env vars
```

## Inter-Process Communication

Message passing, locks, and barriers for agent coordination:

```bash
cos ipc send target-session "build complete" --from build-1
cos ipc recv my-session
cos ipc recv my-session --timeout 30
cos ipc recv my-session --peek
cos ipc list my-session
cos ipc clear my-session
```

### Locks

Mutual exclusion for shared resources. Stale locks from dead processes are auto-reclaimed:

```bash
cos ipc lock database-write --holder agent-1
cos ipc unlock database-write --holder agent-1
cos ipc locks
```

### Barriers

Wait until N agents reach a synchronization point:

```bash
cos ipc barrier merge-ready --expect 3 --session search-1 --timeout 60
```

### Streaming Pipes

Named channels for structured message streaming between processes. Unlike message queues (send/recv), pipes support replay, backpressure, and follow mode:

```bash
cos ipc pipe create my-events --buffer-size 500
cos ipc pipe publish my-events '{"type":"progress","value":42}' --from worker-1
cos ipc pipe subscribe my-events --since 000003 --limit 10
cos ipc pipe subscribe my-events --follow --timeout 30
cos ipc pipe list
cos ipc pipe destroy my-events
```

## Service Management

Manage long-running services with lifecycle hooks and graceful shutdown:

```bash
cos service list
cos service start my-api
cos service stop my-api          # graceful: checkpoint → pre_stop → drain → SIGTERM → wait → SIGKILL → post_stop
cos service stop-all             # stop all in reverse dependency order
cos service restart my-api
cos service status my-api
cos service health my-api
cos service logs my-api --tail 50
```

### Register with Lifecycle Hooks

```bash
cos service register \
  --name my-api \
  --command "python app.py" \
  --workdir /den/api \
  --health-url http://localhost:8000/health \
  --credentials OPENAI_KEY,DB_URL \
  --pre-start "python migrate.py" \
  --pre-stop "python drain.py" \
  --checkpoint-cmd "python save_state.py" \
  --drain-timeout 10 \
  --stop-timeout 30
```

## Job Scheduling (Cron)

Schedule recurring jobs with agent context (tier, scope, credentials) and overlap protection:

```bash
cos cron add health-check \
  --schedule "*/5 * * * *" \
  --command "cos service health my-api" \
  --overlap skip

cos cron add nightly-backup \
  --schedule "0 2 * * *" \
  --command "cos app exec run 'python backup.py'" \
  --tier 1 --scope /den/data \
  --credentials DB_URL \
  --timeout 3600

cos cron list
cos cron status health-check
cos cron logs health-check --limit 10
cos cron enable health-check
cos cron disable health-check
cos cron remove health-check
cos cron run health-check         # manual trigger
```

Overlap policies: `skip` (default — skip if previous still running), `queue`, `kill`, `allow`.

An external scheduler calls `cos cron tick` every minute to process due jobs.

## Network Firewall & Rate Limiting

Control outbound network access and enforce API quotas:

```bash
cos netfilter default deny-all
cos netfilter add --allow "api.openai.com" --port 443
cos netfilter add --allow "*.github.com"
cos netfilter check "api.openai.com"
```

### Rate Limiting

Prevent agents from exceeding API quotas:

```bash
cos netfilter rate-limit api.openai.com --rpm 60 --burst 10
cos netfilter rate-check api.openai.com
cos netfilter rate-limits
cos netfilter rate-limit-remove api.openai.com
```

## File Watching

Event-driven watching with inotify (Linux) and multi-source aggregation:

```bash
cos watch file /den/output.txt --timeout 30
cos watch dir /den/results --timeout 60
cos watch proc build-1 --timeout 300
```

### Multi-Source Watching

Watch files, dirs, processes, and services simultaneously — returns on first event:

```bash
cos watch multi --file /den/main.py --dir /den/output/ --proc worker-1 --service my-api --timeout 60
```

### Event History

View past events from the persistent log:

```bash
cos watch history --limit 20
cos watch history --since "2026-03-25T10:00:00Z" --source file
```

### OS Events

```bash
cos watch on proc.exit --session build-1 --timeout 600
cos watch on service.health-fail --name my-api --timeout 3600
cos watch on ipc.message --session worker-1 --timeout 30
cos watch on credential.expired --name API_TOKEN --timeout 300
```

## Browser (Web Reading)

Fetch web pages as clean Markdown with JavaScript rendered. No Selenium needed:

```bash
cos app web read https://example.com
cos app web screenshot https://example.com
cos app web submit https://example.com/form --data '{"q": "search term"}'
```

## Web Search

Search the web via Google or Brave (auto-fallback):

```bash
cos app search web "Rust async runtime" --max-results 5
cos app search image "architecture diagram" --max-results 5
```

Requires credentials:
```bash
cos credential store GOOGLE_SEARCH_API_KEY "AIza..." --tier 1
cos credential store GOOGLE_SEARCH_ENGINE_ID "a1b2c3..." --tier 1
# Or: cos credential store BRAVE_SEARCH_API_KEY "BSA..." --tier 1
```

## Email

Send, search, and read email. SMTP for sending, Gmail/Outlook for full features:

```bash
cos app email send --to user@example.com --subject "Report" --body "See attached"
cos app email send --to user@example.com --subject "Hi" --body "Hello" --provider gmail
cos app email search --query "from:boss subject:urgent" --max-results 10
cos app email list --unread --max-results 5
cos app email read --id msg123
```

Providers: `smtp` (default, send-only), `gmail`, `outlook`. Auto-detected from credentials.

## Calendar

Manage events locally or sync with Google/Outlook. Works out of the box with no API keys:

```bash
cos app calendar create --title "Standup" --start "2026-03-25T09:00:00Z"
cos app calendar today
cos app calendar list --from "2026-03-25" --to "2026-03-26"
cos app calendar update --id evt-123 --title "New title"
cos app calendar delete --id evt-123
```

Local events stored in SQLite. Add `--provider google` or `--provider outlook` with OAuth tokens for cloud sync.

## File System

```bash
cos app fs ls /den
cos app fs read /den/file.txt
cos app fs write /den/output.txt    # reads content from stdin
cos app fs stat /den/file.txt
cos app fs search "pattern" /den
cos app fs rm /den/tmp
cos app fs mkdir /den/new-dir
```

## Documents

Read PDFs, DOCX, XLSX, PPTX, CSV, and other formats as structured text:

```bash
cos app doc read document.pdf
cos app doc read spreadsheet.xlsx
cos app doc read presentation.pptx
cos app doc info document.pdf
```

## Database (SQLite)

```bash
cos app db exec mydb "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)"
cos app db exec mydb "INSERT INTO users (name) VALUES ('Alice')"
cos app db query mydb "SELECT * FROM users"
cos app db tables mydb
cos app db schema mydb users
cos app db databases
```

## HTTP Client

```bash
cos app net fetch https://api.example.com/data
cos app net fetch https://api.example.com/data --method POST --data '{"key": "value"}'
cos app net download https://example.com/file.zip --output /den/file.zip
```

## Key-Value Store

Persistent key-value storage for state and memory:

```bash
cos app kv set project:status "building"
cos app kv get project:status
cos app kv list "project:*"
cos app kv del project:status
```

## System Info

```bash
cos sys info
cos sys env
cos sys resources
cos sys uptime
```

## Browser Service

Manage the built-in browser rendering engine:

```bash
cos browser status
cos browser health
cos browser restart
```

## Package Management

```bash
cos app pkg need python3-pymupdf
cos app pkg has ripgrep
cos app pkg list
```
