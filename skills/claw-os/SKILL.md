---
name: claw-os
description: "Native Claw OS system commands. Use when: you need sandboxed execution, process management, browser rendering, file operations, database queries, or system information. You are running on Claw OS — use cos commands instead of raw shell commands for better structured output."
metadata: { "openclaw": { "emoji": "🦀", "requires": { "bins": ["cos"] } } }
---

# Claw OS

You are running on Claw OS. All `cos` commands return JSON.

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

## Browser (Web Reading)

Fetch web pages as clean Markdown with JavaScript rendered. No Selenium needed:

```bash
cos web read https://example.com
cos web screenshot https://example.com
cos web submit https://example.com/form --data '{"q": "search term"}'
```

## File System

```bash
cos fs ls /workspace
cos fs read /workspace/file.txt
cos fs write /workspace/output.txt    # reads content from stdin
cos fs stat /workspace/file.txt
cos fs search "pattern" /workspace
cos fs rm /workspace/tmp
cos fs mkdir /workspace/new-dir
```

## Documents

Read PDFs, DOCX, XLSX, CSV, and other formats as structured text:

```bash
cos doc read document.pdf
cos doc read spreadsheet.xlsx
cos doc info document.pdf
```

## Database (SQLite)

```bash
cos db exec mydb "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)"
cos db exec mydb "INSERT INTO users (name) VALUES ('Alice')"
cos db query mydb "SELECT * FROM users"
cos db tables mydb
cos db schema mydb users
cos db databases
```

## HTTP Client

```bash
cos net fetch https://api.example.com/data
cos net fetch https://api.example.com/data --method POST --data '{"key": "value"}'
cos net download https://example.com/file.zip --output /workspace/file.zip
```

## Key-Value Store

Persistent key-value storage for state and memory:

```bash
cos kv set project:status "building"
cos kv get project:status
cos kv list "project:*"
cos kv del project:status
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
cos pkg need python3-pymupdf
cos pkg has ripgrep
cos pkg list
```
