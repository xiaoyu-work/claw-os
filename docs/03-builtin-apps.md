# Part 3: Built-in Apps

Claw OS ships 10 Python apps that extend the Rust core with higher-level functionality. Each app is a self-contained directory under `/usr/lib/cos/apps/` with:

- `app.json` — manifest (name, version, commands, dependencies)
- `main.py` — entry point implementing `run(command, args) → dict`
- `test_main.py` — unit tests

Apps are invoked via `cos <app> <command> [args...]`. The Rust bridge spawns a Python subprocess, passes the command and args, and returns the JSON result. Policy checks are enforced before the subprocess starts.

---

## fs — File Operations

Full-featured file management with metadata tracking and content search.

### fs ls

List directory contents with metadata.

```bash
cos fs ls /den [--all] [--long]
```
```json
{
  "path": "/den",
  "entries": [
    {"name": "main.py", "type": "file", "size": 1024, "modified": "2026-03-23T10:00:00Z"},
    {"name": "src", "type": "directory", "modified": "2026-03-23T09:00:00Z"}
  ],
  "count": 2
}
```

### fs read

Read file contents. Supports partial reads for large files.

```bash
cos fs read /den/main.py [--offset N] [--limit N] [--start-line N] [--end-line N]
```
```json
{
  "path": "/den/main.py",
  "content": "import sys\n...",
  "size": 1024,
  "lines": 42,
  "encoding": "utf-8"
}
```

Maximum read size: 1MB per call.

### fs write

Write content to a file.

```bash
cos fs write /den/output.txt --content "Hello, world!"
```
```json
{"path": "/den/output.txt", "bytes_written": 13}
```

### fs rm

Remove a file or directory.

```bash
cos fs rm /den/temp.txt
cos fs rm /den/temp_dir --recursive
```

### fs mkdir

Create a directory (including parents).

```bash
cos fs mkdir /den/src/components
```

### fs stat

Get detailed file metadata.

```bash
cos fs stat /den/main.py
```
```json
{
  "path": "/den/main.py",
  "type": "file",
  "size": 1024,
  "created": "2026-03-23T08:00:00Z",
  "modified": "2026-03-23T10:00:00Z",
  "permissions": "rw-r--r--",
  "tags": ["entrypoint", "python"]
}
```

### fs search

Search file contents (powered by ripgrep) and filenames.

```bash
cos fs search "def main" --path /den/src [--type py] [--max-results 20]
```
```json
{
  "matches": [
    {"file": "/den/src/main.py", "line": 15, "content": "def main():"},
    {"file": "/den/src/utils.py", "line": 8, "content": "def main_helper():"}
  ],
  "count": 2
}
```

### fs tag

Add semantic tags to files. Tags are stored in `.cos-meta.json` sidecar files.

```bash
cos fs tag /den/main.py --add entrypoint --add python
cos fs tag /den/main.py --remove python
```

### fs recent

List recently modified files.

```bash
cos fs recent /den --limit 10
```

---

## exec — Command Execution

Run shell commands and inline scripts with timeout control.

### exec run

Execute a shell command.

```bash
cos exec run "ls -la /den" [--shell bash] [--timeout 300]
```
```json
{
  "exit_code": 0,
  "stdout": "total 16\ndrwxr-xr-x ...",
  "stderr": "",
  "duration_ms": 12
}
```

Maximum output: 1MB per call.

### exec script

Run an inline script or script file with automatic language detection.

```bash
cos exec script --lang python --content "print(2 + 2)" [--timeout 60]
cos exec script --file /den/train.py --lang python
```

Language detection: `.py` → python3, `.sh` → bash, `.js` → node.

### exec which

Check if a command exists in the PATH.

```bash
cos exec which ripgrep
```
```json
{"command": "ripgrep", "found": true, "path": "/usr/bin/rg"}
```

### exec start / stop / ps

Legacy background process management (prefer `cos proc spawn` for new use).

```bash
cos exec start "python server.py"
cos exec ps
cos exec stop <pid>
```

---

## web — Browser & HTTP

Fetch web pages with full JavaScript rendering. Powered by the built-in Chromium browser engine.

### web read

Convert a URL to clean Markdown.

```bash
cos web read "https://example.com" [--timeout 30]
```
```json
{
  "url": "https://example.com",
  "title": "Example Domain",
  "content": "# Example Domain\n\nThis domain is for use in illustrative examples...",
  "content_length": 256
}
```

The browser engine renders JavaScript, so SPAs and dynamically-loaded content work correctly.

### web screenshot

Capture a screenshot of a web page.

```bash
cos web screenshot "https://example.com" --output /den/screenshot.png
```

### web submit

Submit form data to a URL.

```bash
cos web submit "https://example.com/form" --data '{"field": "value"}'
```

**Dependency:** Requires the browser service to be running (`cos browser start`).

---

## db — SQLite Database

Direct SQLite database access for agents.

### db query

Run a SELECT query.

```bash
cos db query "SELECT * FROM users LIMIT 10" --database /den/app.db
```
```json
{
  "columns": ["id", "name", "email"],
  "rows": [
    [1, "Alice", "alice@example.com"],
    [2, "Bob", "bob@example.com"]
  ],
  "count": 2
}
```

### db exec

Run DDL/DML statements (CREATE, INSERT, UPDATE, DELETE).

```bash
cos db exec "INSERT INTO users (name, email) VALUES ('Charlie', 'charlie@example.com')" --database /den/app.db
```

### db tables

List all tables in a database.

```bash
cos db tables --database /den/app.db
```

### db schema

Show table schema.

```bash
cos db schema users --database /den/app.db
```

### db databases

List all available database files.

```bash
cos db databases
```

Default database location: `/var/lib/cos/databases/`.

---

## doc — Document Reader

Read PDF, DOCX, XLSX, and CSV files as structured text.

### doc read

Extract text content from a document.

```bash
cos doc read /den/report.pdf
cos doc read /den/data.xlsx
cos doc read /den/document.docx
```
```json
{
  "path": "/den/report.pdf",
  "format": "pdf",
  "pages": 12,
  "content": "Chapter 1: Introduction\n..."
}
```

**Supported formats:**
| Format | Library |
|--------|---------|
| PDF | PyMuPDF (fitz) |
| DOCX | python-docx |
| XLSX | openpyxl |
| CSV | Python csv module |

### doc info

Get document metadata without reading full content.

```bash
cos doc info /den/report.pdf
```
```json
{
  "path": "/den/report.pdf",
  "format": "pdf",
  "pages": 12,
  "size": 524288,
  "title": "Annual Report 2025"
}
```

---

## net — HTTP Client

Make HTTP requests and download files.

### net fetch

Send an HTTP request.

```bash
cos net fetch "https://api.example.com/data" [--method POST] [--data '{"key":"val"}'] [--headers '{"Authorization":"Bearer ..."}'] [--timeout 30]
```
```json
{
  "status": 200,
  "headers": {"content-type": "application/json"},
  "body": {"data": [1, 2, 3]},
  "duration_ms": 245
}
```

### net download

Download a file to disk.

```bash
cos net download "https://example.com/file.zip" --output /den/file.zip
```
```json
{"path": "/den/file.zip", "size": 1048576, "duration_ms": 1200}
```

---

## kv — Key-Value Store

Simple persistent key-value storage for agent state and memory.

### kv set / get

```bash
cos kv set "last_checkpoint" "003"
cos kv get "last_checkpoint"
```
```json
{"key": "last_checkpoint", "value": "003"}
```

### kv list

List keys matching a pattern (supports `*` wildcard).

```bash
cos kv list "task_*"
```
```json
{
  "keys": ["task_1_status", "task_2_status", "task_3_status"],
  "count": 3
}
```

### kv del

Delete a key.

```bash
cos kv del "last_checkpoint"
```

Storage: JSON files in `/var/lib/cos/kv/`.

---

## log — Audit Log Search

Query the automatic audit trail.

### log search

Search audit entries by field.

```bash
cos log search "exec" [--app exec] [--status error] [--limit 50]
```
```json
{
  "entries": [
    {"timestamp": "2026-03-23T10:15:31Z", "app": "exec", "command": "run", "status": "error", "error": "command not found: foobar", "duration_ms": 5}
  ],
  "count": 1
}
```

### log tail

Show the most recent audit entries.

```bash
cos log tail 20
```

### log read / write

```bash
cos log read [--limit 100]
cos log write "custom log message"
```

Audit log location: `/var/lib/cos/logs/audit.jsonl`.

---

## notify — Notifications

Send notifications (platform-dependent output).

### notify send

```bash
cos notify send "Build complete" [--channel slack] [--priority high]
```

### notify list

List recent notifications.

```bash
cos notify list
```

---

## pkg — Package Management

Declarative package management for ensuring tool availability.

### pkg need

Install a package if not already present.

```bash
cos pkg need ripgrep
cos pkg need nodejs
```
```json
{"package": "ripgrep", "status": "already_installed", "version": "13.0.0"}
```

Uses `apt` on Debian-based systems.

### pkg has

Check if a package is installed.

```bash
cos pkg has ffmpeg
```
```json
{"package": "ffmpeg", "installed": false}
```

### pkg list

List all installed packages.

```bash
cos pkg list [--filter "python*"]
```

---

## App Development

To create a new app:

1. Create a directory under `apps/`:
   ```
   apps/myapp/
   ├── app.json
   ├── main.py
   └── test_main.py
   ```

2. Define the manifest (`app.json`):
   ```json
   {
     "name": "myapp",
     "version": "0.1.0",
     "description": "My custom app",
     "commands": {
       "hello": "Say hello",
       "compute": "Run computation"
     },
     "dependencies": {
       "python": ["numpy"],
       "system": ["curl"]
     }
   }
   ```

3. Implement the entry point (`main.py`):
   ```python
   def run(command, args):
       if command == "hello":
           return {"message": "Hello from myapp!"}
       elif command == "compute":
           return {"result": 42}
       else:
           return {"error": f"unknown command: {command}"}
   ```

4. The app is automatically discovered and available as `cos myapp hello`.

Policy enforcement is automatic — the Rust bridge infers the operation type from the command name and checks the session's tier before spawning the Python subprocess.
