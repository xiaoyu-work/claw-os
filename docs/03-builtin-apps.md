# Part 3: Built-in Apps

Claw OS ships 13 Python apps that extend the Rust core with higher-level functionality. Each app is a self-contained directory under `/usr/lib/cos/apps/` with:

- `app.json` — manifest (name, version, commands, dependencies)
- `main.py` — entry point implementing `run(command, args) → dict`
- `test_main.py` — unit tests

Apps are invoked via `cos app <name> <command> [args...]`. The Rust bridge spawns a Python subprocess, passes the command and args, and returns the JSON result. Policy checks are enforced before the subprocess starts.

---

## fs — File Operations

Full-featured file management with metadata tracking and content search.

### fs ls

List directory contents with metadata.

```bash
cos app fs ls /den [--all] [--long]
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
cos app fs read /den/main.py [--offset N] [--limit N] [--start-line N] [--end-line N]
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
cos app fs write /den/output.txt --content "Hello, world!"
```
```json
{"path": "/den/output.txt", "bytes_written": 13}
```

### fs rm

Remove a file or directory.

```bash
cos app fs rm /den/temp.txt
cos app fs rm /den/temp_dir --recursive
```

### fs mkdir

Create a directory (including parents).

```bash
cos app fs mkdir /den/src/components
```

### fs stat

Get detailed file metadata.

```bash
cos app fs stat /den/main.py
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
cos app fs search "def main" --path /den/src [--type py] [--max-results 20]
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
cos app fs tag /den/main.py --add entrypoint --add python
cos app fs tag /den/main.py --remove python
```

### fs recent

List recently modified files.

```bash
cos app fs recent /den --limit 10
```

---

## exec — Command Execution

Run shell commands and inline scripts with timeout control.

### exec run

Execute a shell command.

```bash
cos app exec run "ls -la /den" [--shell bash] [--timeout 300]
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
cos app exec script --lang python --content "print(2 + 2)" [--timeout 60]
cos app exec script --file /den/train.py --lang python
```

Language detection: `.py` → python3, `.sh` → bash, `.js` → node.

### exec which

Check if a command exists in the PATH.

```bash
cos app exec which ripgrep
```
```json
{"command": "ripgrep", "found": true, "path": "/usr/bin/rg"}
```

### exec start / stop / ps

Legacy background process management (prefer `cos proc spawn` for new use).

```bash
cos app exec start "python server.py"
cos app exec ps
cos app exec stop <pid>
```

---

## web — Browser & HTTP

Fetch web pages with full JavaScript rendering. Powered by the built-in Chromium browser engine.

### web read

Convert a URL to clean Markdown.

```bash
cos app web read "https://example.com" [--timeout 30]
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
cos app web screenshot "https://example.com" --output /den/screenshot.png
```

### web submit

Submit form data to a URL.

```bash
cos app web submit "https://example.com/form" --data '{"field": "value"}'
```

**Dependency:** Requires the browser service to be running (`cos browser start`).

---

## db — SQLite Database

Direct SQLite database access for agents.

### db query

Run a SELECT query.

```bash
cos app db query "SELECT * FROM users LIMIT 10" --database /den/app.db
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
cos app db exec "INSERT INTO users (name, email) VALUES ('Charlie', 'charlie@example.com')" --database /den/app.db
```

### db tables

List all tables in a database.

```bash
cos app db tables --database /den/app.db
```

### db schema

Show table schema.

```bash
cos app db schema users --database /den/app.db
```

### db databases

List all available database files.

```bash
cos app db databases
```

Default database location: `/var/lib/cos/databases/`.

---

## doc — Document Reader

Read PDF, DOCX, XLSX, and CSV files as structured text.

### doc read

Extract text content from a document.

```bash
cos app doc read /den/report.pdf
cos app doc read /den/data.xlsx
cos app doc read /den/document.docx
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
cos app doc info /den/report.pdf
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
cos app net fetch "https://api.example.com/data" [--method POST] [--data '{"key":"val"}'] [--headers '{"Authorization":"Bearer ..."}'] [--timeout 30]
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
cos app net download "https://example.com/file.zip" --output /den/file.zip
```
```json
{"path": "/den/file.zip", "size": 1048576, "duration_ms": 1200}
```

---

## kv — Key-Value Store

Simple persistent key-value storage for agent state and memory.

### kv set / get

```bash
cos app kv set "last_checkpoint" "003"
cos app kv get "last_checkpoint"
```
```json
{"key": "last_checkpoint", "value": "003"}
```

### kv list

List keys matching a pattern (supports `*` wildcard).

```bash
cos app kv list "task_*"
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
cos app kv del "last_checkpoint"
```

Storage: JSON files in `/var/lib/cos/kv/`.

---

## log — Audit Log Search

Query the automatic audit trail.

### log search

Search audit entries by field.

```bash
cos app log search "exec" [--app exec] [--status error] [--limit 50]
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
cos app log tail 20
```

### log read / write

```bash
cos app log read [--limit 100]
cos app log write "custom log message"
```

Audit log location: `/var/lib/cos/logs/audit.jsonl`.

---

## notify — Notifications

Send notifications (platform-dependent output).

### notify send

```bash
cos app notify send "Build complete" [--channel slack] [--priority high]
```

### notify list

List recent notifications.

```bash
cos app notify list
```

---

## pkg — Package Management

Declarative package management for ensuring tool availability.

### pkg need

Install a package if not already present.

```bash
cos app pkg need ripgrep
cos app pkg need nodejs
```
```json
{"package": "ripgrep", "status": "already_installed", "version": "13.0.0"}
```

Uses `apt` on Debian-based systems.

### pkg has

Check if a package is installed.

```bash
cos app pkg has ffmpeg
```
```json
{"package": "ffmpeg", "installed": false}
```

### pkg list

List all installed packages.

```bash
cos app pkg list [--filter "python*"]
```

---

## search — Web & Image Search

Search the web using Google Custom Search or Brave Search. Auto-fallback: if Google fails, retries with Brave.

**Credentials** (store one or both):
```bash
cos credential store GOOGLE_SEARCH_API_KEY "AIza..." --tier 1
cos credential store GOOGLE_SEARCH_ENGINE_ID "a1b2c3..." --tier 1
# Or:
cos credential store BRAVE_SEARCH_API_KEY "BSA..." --tier 1
```

### search web

Search the web for information.

```bash
cos app search web "Rust async runtime" [--max-results 5] [--provider google|brave]
```
```json
{
  "query": "Rust async runtime",
  "provider": "google",
  "results": [
    {"title": "Tokio", "url": "https://tokio.rs", "snippet": "An asynchronous runtime for Rust..."}
  ],
  "count": 5,
  "total_results": 1250000
}
```

### search image

Search for images.

```bash
cos app search image "architecture diagram" [--max-results 5]
```
```json
{
  "query": "architecture diagram",
  "provider": "google",
  "results": [
    {"title": "System architecture", "url": "https://example.com/arch.png", "thumbnail": "https://...", "width": 1920, "height": 1080, "source": "example.com"}
  ],
  "count": 5
}
```

---

## email — Email Management

Send, search, and read email. Supports three providers:

| Provider | Use Case | Credentials Needed |
|---|---|---|
| **SMTP** (default) | Send-only, works with any mail server | `SMTP_HOST`, `SMTP_PORT`, `SMTP_USER`, `SMTP_PASSWORD` |
| **Gmail** | Full features (send, search, list, read) | `GMAIL_ACCESS_TOKEN` or `GOOGLE_OAUTH_TOKEN` |
| **Outlook** | Full features (send, search, list, read) | `MICROSOFT_ACCESS_TOKEN` or `MICROSOFT_OAUTH_TOKEN` |

Provider is auto-detected from available credentials. Override with `--provider`.

### email send

```bash
cos app email send --to recipient@example.com --subject "Hello" --body "Message body" [--cc other@example.com] [--provider smtp|gmail|outlook]
```
```json
{"sent": true, "to": "recipient@example.com", "subject": "Hello", "provider": "gmail"}
```

### email search

Search emails by query (Gmail/Outlook only).

```bash
cos app email search --query "from:boss subject:urgent" [--max-results 10] [--provider gmail|outlook]
```
```json
{
  "query": "from:boss",
  "provider": "gmail",
  "emails": [
    {"id": "msg123", "from": "boss@company.com", "subject": "Urgent", "snippet": "Please review...", "date": "2026-03-25T10:00:00Z", "unread": true}
  ],
  "count": 3
}
```

### email list

List recent emails.

```bash
cos app email list [--max-results 10] [--unread] [--provider gmail|outlook]
```

### email read

Read a specific email by ID.

```bash
cos app email read --id <message-id> [--provider gmail|outlook]
```
```json
{
  "id": "msg123",
  "from": "boss@company.com",
  "to": ["you@company.com"],
  "subject": "Urgent",
  "body": "Please review the attached document...",
  "date": "2026-03-25T10:00:00Z",
  "attachments": [{"name": "doc.pdf", "size": 52400}]
}
```

---

## calendar — Events & Scheduling

Manage calendar events. **Local-first**: works out of the box with a SQLite database, no API keys needed. Optionally sync with Google Calendar or Outlook.

| Provider | Storage | Credentials Needed |
|---|---|---|
| **local** (default) | SQLite at `$COS_DATA_DIR/calendar/events.db` | None |
| **Google** | Google Calendar API v3 | `GOOGLE_CALENDAR_TOKEN` or `GOOGLE_OAUTH_TOKEN` |
| **Outlook** | Microsoft Graph API | `MICROSOFT_ACCESS_TOKEN` or `MICROSOFT_OAUTH_TOKEN` |

### calendar create

```bash
cos app calendar create --title "Team standup" --start "2026-03-25T09:00:00Z" [--end "2026-03-25T09:30:00Z"] [--description "Daily sync"] [--location "Room 3"] [--provider local|google|outlook]
```
```json
{
  "created": true,
  "provider": "local",
  "event": {"id": "evt-1234-abc", "title": "Team standup", "start": "2026-03-25T09:00:00Z", "end": "2026-03-25T09:30:00Z"}
}
```

If `--end` is omitted, defaults to 1 hour after start.

### calendar list

```bash
cos app calendar list --from "2026-03-25T00:00:00Z" --to "2026-03-26T00:00:00Z" [--provider local|google|outlook]
```
```json
{
  "provider": "local",
  "events": [
    {"id": "evt-1234-abc", "title": "Team standup", "start": "2026-03-25T09:00:00Z", "end": "2026-03-25T09:30:00Z", "description": "Daily sync"}
  ],
  "count": 1
}
```

### calendar today

Shortcut to list today's events.

```bash
cos app calendar today [--provider local|google|outlook]
```

### calendar update

```bash
cos app calendar update --id evt-1234-abc --title "New title" [--start "..."] [--end "..."] [--provider local|google|outlook]
```

### calendar delete

```bash
cos app calendar delete --id evt-1234-abc [--provider local|google|outlook]
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

4. The app is automatically discovered and available as `cos app myapp hello`.

Policy enforcement is automatic — the Rust bridge infers the operation type from the command name and checks the session's tier before spawning the Python subprocess.
