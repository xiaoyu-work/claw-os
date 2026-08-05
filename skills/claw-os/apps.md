# Apps

All apps are accessed via `cos app <name> <command>`.

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
cos app fs ls /home/cos
cos app fs read /home/cos/file.txt
cos app fs write /home/cos/output.txt    # reads content from stdin
cos app fs stat /home/cos/file.txt
cos app fs search "pattern" /home/cos
cos app fs rm /home/cos/tmp
cos app fs mkdir /home/cos/new-dir
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
cos app net download https://example.com/file.zip --output /home/cos/file.zip
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

## Network Doctor

```bash
cos app netdiag interfaces
cos app netdiag routes
cos app netdiag dns example.com
cos app netdiag tcp example.com:443 --attempts 3
cos app netdiag diagnose example.com:443
```

`diagnose` checks the local link, IPv4 default route, one-time DNS resolution,
and TCP reachability in order. TCP probes connect to the exact addresses
returned by that resolution, so a second DNS lookup cannot redirect the
connection.

## NetworkManager Control

```bash
cos app network-manager status
cos app network-manager wifi-list
cos app network-manager wifi-connect "Cafe WiFi" default/cafe_wifi_psk
cos app network-manager wifi-toggle off
cos app network-manager vpn-list
cos app network-manager vpn-up work-vpn
cos app network-manager airplane on
```

The optional Wi-Fi credential uses `namespace/name` form and is loaded only
after exact `secret.read` and credential-tier checks. Its plaintext is written
only to a root-only temporary nmcli password file and never placed in argv.
Network mutation grants are category-scoped as `net.manage:wifi`,
`net.manage:vpn`, or `net.manage:airplane`; user-controlled SSIDs and profile
names are never interpreted as glob capability patterns.

## Package Management

```bash
cos app pkg need python3-pymupdf
cos app pkg has ripgrep
cos app pkg list
cos app pkg update
cos app pkg upgrade curl
cos app pkg install-version curl 8.0.1-1
cos app pkg hold curl
cos app pkg remove curl
```

Single-package install, upgrade, remove, and hold changes record the previous
installed version and hold state on a durable parent task. Purge, global index
refresh, and full-system upgrade require a system snapshot for complete
rollback.

## Native System Services

Use the `systemd` app for native systemd units. This is distinct from the
`cos_service` tool, which manages Claw-defined service manifests.

```bash
cos app systemd status ssh.service
cos app systemd restart ssh.service
cos app systemd enable ssh.service
cos app systemd disable ssh.service
```

Status requires `sys.observe:<unit>`. Lifecycle changes require the exact
`sys.service:<unit>` grant and are executed by the root clawd broker. Start,
stop, enable, and disable record the previous state on a durable parent task
for rollback.

## System Recovery Points

```bash
cos app system-snapshot status
cos app system-snapshot create "before distribution upgrade"
cos app system-snapshot list
cos app system-snapshot rollback snap_<id> --confirm
```

The broker prefers a configured Snapper root profile, otherwise uses direct
read-only Btrfs snapshots or LVM snapshots. Snapper and LVM rollback require a
reboot. Direct Btrfs snapshots can be created and deleted, but live-root
rollback is refused until a bootloader-aware restore path is configured.
