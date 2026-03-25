---
name: claw-os
description: "Native Claw OS system commands. Use when: you need sandboxed execution, process management, inter-process communication, checkpoints, service management, file operations, database queries, or system information. You are running on Claw OS — use cos commands instead of raw shell commands for better structured output."
metadata: { "openclaw": { "emoji": "🦀", "requires": { "bins": ["cos"] } } }
---

# Claw OS

You are running on Claw OS. All `cos` commands return JSON.

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

## Service Management

Manage long-running services via declarative JSON definitions:

```bash
cos service list
cos service start browser
cos service stop browser
cos service restart browser
cos service status browser
cos service health browser
cos service logs browser --tail 50
cos service register --name my-service --command "node server.js" --health-url http://localhost:8080
```

## File Watching

Block until a file, directory, or process changes:

```bash
cos watch file /den/output.txt --timeout 30
cos watch dir /den/results --timeout 60
cos watch proc build-1 --timeout 300
```

## Browser (Web Reading)

Fetch web pages as clean Markdown with JavaScript rendered. No Selenium needed:

```bash
cos app web read https://example.com
cos app web screenshot https://example.com
cos app web submit https://example.com/form --data '{"q": "search term"}'
```

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

Read PDFs, DOCX, XLSX, CSV, and other formats as structured text:

```bash
cos app doc read document.pdf
cos app doc read spreadsheet.xlsx
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
