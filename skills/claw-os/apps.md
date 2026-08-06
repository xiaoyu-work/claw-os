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

## Crash Doctor

```bash
cos app crash-doctor recent 60 20
cos app crash-doctor diagnose 60 20
cos app crash-doctor backtrace <boot-id>:<pid>:<timestamp-us>
```

Crash Doctor correlates systemd coredumps with bounded kernel and service
journal evidence for OOM kills, segmentation faults, and repeated crashes.
System-wide crash metadata and backtraces require the explicit high-risk
`sys.crash:system` capability. Live GDB analysis is optional, output-bounded,
disables automatic script loading and debuginfod, and runs as the crashed
process UID/GID for non-root crashes. Root-owned dumps return the recorded
stack only and never launch GDB as root.

## Storage Manager

```bash
cos app storage-manager status
cos app storage-manager health /dev/sdb
cos app storage-manager check /dev/sdb1
cos app storage-manager mount /dev/sdb1
cos app storage-manager unmount /dev/sdb1
cos app storage-manager eject /dev/sdb
```

`health` combines SMART JSON, filesystem-specific metadata, and matching
kernel storage errors. `check` only runs no-repair checkers on an unmounted
filesystem. These deep reads require `sys.storage:diagnose`. Mount, unmount,
and eject require `sys.mount` for the exact canonical `/dev` path; symlink
aliases are rejected so capability and kernel targets cannot diverge.
Mounting calls UDisks2 as the requesting user inside a verified active local
logind session, while protected system mounts, active swap, and non-removable
eject targets are refused.

## Audio Manager

```bash
cos app audio-manager status
cos app audio-manager output-volume 75
cos app audio-manager output-mute toggle
cos app audio-manager input-mute on
cos app audio-manager output-default 42
cos app audio-manager output-route 42 1
cos app audio-manager profile 30 2
```

Audio Manager connects to the requesting user's PipeWire and WirePlumber
runtime even when the Agent itself is routed through clawd. Output changes use
`device.audio:output`; microphone changes use `device.microphone:input`.
Defaults, routes, and profiles use the high-risk
`device.media-route:pipewire` scope because numeric PipeWire IDs are global
and reusable across media classes. The broker filters non-audio objects and
revalidates object type and serial immediately before each graph mutation.

## Desktop Manager

```bash
cos app desktop-manager list
cos app desktop-manager focus <identifier>
cos app desktop-manager close <identifier>
cos app desktop-manager restart <identifier> <app-id>
```

Window identifiers come from `ext-foreign-toplevel-list-v1` and remain stable
for the lifetime of the window; local Wayland proxy IDs are never exposed.
Focus and close require `desktop.window:control`. Restart verifies that the
selected window still has the supplied AppID, closes every matching window,
and only then uses the exact `desktop.launch:<app-id>` grant to relaunch it.
If an application refuses to close, relaunch is skipped rather than creating a
duplicate instance.

## Bluetooth Manager

```bash
cos app bluetooth-manager status
cos app bluetooth-manager power AA:BB:CC:DD:EE:FF on
cos app bluetooth-manager scan AA:BB:CC:DD:EE:FF 15
cos app bluetooth-manager pair AA:BB:CC:DD:EE:FF 11:22:33:44:55:66
cos app bluetooth-manager pair-respond <pairing-id> yes
cos app bluetooth-manager trust AA:BB:CC:DD:EE:FF 11:22:33:44:55:66
cos app bluetooth-manager connect AA:BB:CC:DD:EE:FF 11:22:33:44:55:66
```

All mutations require the fixed high-risk `device.bluetooth:control` scope;
adapter and device addresses are normalized and never become glob scopes.
Pairing starts a dedicated bounded `KeyboardDisplay` BlueZ agent session.
`pair` returns a pairing ID and any confirmation/PIN/passkey prompt;
`pair-respond`, `pair-status`, and `pair-cancel` continue that same D-Bus
connection. Prompt responses are forwarded to the broker over stdin rather
than its command line. Discovery likewise runs in one bounded
`bluetoothctl` connection and is explicitly stopped before that client exits.

## Power Manager

```bash
cos app power-manager status
cos app power-manager suspend --confirm
cos app power-manager hibernate --confirm
cos app power-manager reboot --confirm
cos app power-manager poweroff --confirm
```

`status` reads UPower devices and logind `Can*` capabilities. Every state
change requires the critical unscoped `sys.power` capability plus an explicit
`--confirm`. The broker writes a durable power intent before sending a
no-reply logind request, so suspend/reboot/poweroff is recorded even if the
machine transitions before the normal clawd response audit completes.

## Hardware Center

```bash
cos app hardware-center summary
cos app hardware-center cpu
cos app hardware-center gpu
cos app hardware-center usb
cos app hardware-center memory
cos app hardware-center drivers
cos app hardware-center thermal
```

Hardware Center combines kernel sysfs/proc data with bounded lscpu, lspci,
lsblk, and dmidecode enrichment. It reports bound drivers, IOMMU groups,
firmware identity, DIMM metadata, USB authorization, CPU vulnerability state,
temperatures, and fans under the read-only `sys.observe:hardware` scope.
Individual provider failures remain explicit inside `summary` rather than
silently producing an empty healthy-looking section.

## Security Center

```bash
cos app security-center summary
cos app security-center auth
cos app security-center ssh
cos app security-center sudo
cos app security-center mac
cos app security-center ports
cos app security-center events
```

Security Center uses the high-risk read-only `sys.security:audit` scope because
its evidence contains login sources, sudo rules, effective SSH policy, process
listeners, and mandatory-access-control denials. It flags repeated auth
failures, direct root SSH, empty/password authentication, invalid or writable
sudoers files, broad NOPASSWD rules, missing enforcing MAC, sensitive wildcard
listeners, and recent kernel security events.

## Container Manager

```bash
cos app container-manager status
cos app container-manager list docker
cos app container-manager list podman
cos app container-manager list containerd k8s.io
cos app container-manager logs docker web 200
cos app container-manager namespaces podman web
cos app container-manager restart docker web
cos app container-manager remove docker old-job --confirm
```

Inventory, inspect, logs, process, stats, cgroup, and namespace evidence require
`sys.container:observe`; lifecycle changes require
`sys.container:control`. Rootless `podman` runs as the requesting user,
`podman-root` addresses root-owned containers, and Docker/containerd use the
system runtime. Containerd lifecycle/log operations use nerdctl and require an
explicit namespace; ctr remains a read-only inventory fallback. Removal never
passes volume-deletion or force flags and requires explicit confirmation.

## Safe Config Editor

```bash
cos app config-editor inspect /etc/hosts
cos app config-editor validate /etc/hosts /home/cos/hosts.new
cos app config-editor apply /etc/hosts /home/cos/hosts.new --confirm
cos app config-editor restore /etc/hosts <backup-token> --confirm
```

Replacement content is read from an approved source file, never placed in
argv. Targets must be canonical regular files below `/etc` and must match a
registered validator (JSON, sudoers, sshd, systemd units, fstab, sysctl,
shell, hosts, hostname, or resolv.conf). `sys.config:<exact-path>` gates the
target and `fs.read:<source>` gates staged content. Existing metadata/xattrs
are preserved with `cp --preserve=all`; a durable backup and applied hash are
written before atomic replacement. Failed post-validation restores
automatically, and manual restore refuses to overwrite newer edits.

## Event Center

```bash
cos app event-center status
cos app event-center recent
cos app event-center recent security 50
cos app event-center recent storage 50
cos app event-center watch-pid 1234
```

Event Center starts with clawd and maintains restartable subscriptions to udev,
systemd D-Bus signals, and filtered journal follow output. Storage and security
events are classified into dedicated sources, while requested process watches
use pidfds and verify the original PID start time. Records are broadcast
in-memory and persisted to a bounded rotating JSONL log under
`sys.events:observe`; watcher failures remain visible in `status`.

## Backup Center

```bash
cos app backup-center init /media/cos/backup/repo default/restic
cos app backup-center backup /media/cos/backup/repo /home/cos/Documents default/restic
cos app backup-center snapshots /media/cos/backup/repo default/restic
cos app backup-center check /media/cos/backup/repo default/restic
cos app backup-center restore /media/cos/backup/repo latest /home/cos/restore default/restic --confirm
cos app backup-center retention /media/cos/backup/repo default/restic 7 4 12 --confirm
```

Backup Center runs Restic as the requesting user. Repositories must live on a
currently mounted non-root filesystem, and the mount ID is checked before and
after every operation to detect missing or replaced backup media. The password
is loaded through exact `secret.read`, written only to a user-owned 0600
runtime file, and passed via `--password-file`. `data.backup:<path>` separately
authorizes repository, source-tree, and restore-destination roots. Restore only
targets an empty or new directory; forgetting and retention/prune require
explicit confirmation and never delete volumes.

## Firewall Manager

```bash
cos app firewall-manager status
cos app firewall-manager add allow input tcp 22 --remote 192.0.2.0/24 --interface eth0
cos app firewall-manager add deny input tcp 22 --remote 0.0.0.0/0
cos app firewall-manager delete <rule-id>
cos app firewall-manager clear --confirm
cos app firewall-manager restore <backup-token> --confirm
```

Firewall Manager owns only the dedicated `inet claw_agent` table and never
flushes distribution or administrator tables. Rules are generated from
validated direction/protocol/port/CIDR/interface fields, checked and applied as
one nft transaction, then verified by managed rule comments. Every mutation
persists a previous-state token; failed persistence rolls live rules back, and
manual restore requires the exact current revision. clawd reapplies persisted
managed rules after reboot. Mutations require `net.firewall:manage`.

## User Manager

```bash
cos app user-manager status
cos app user-manager create-user alice --full-name "Alice" --groups sudo
cos app user-manager set-password alice default/alice-password
cos app user-manager add-to-group alice docker
cos app user-manager lock-user alice
cos app user-manager delete-user alice --confirm
cos app user-manager restore <backup-token> --confirm
```

User Manager only mutates regular UID/GID ranges and refuses root/system
accounts. Password plaintext is loaded through exact `secret.read` and sent to
`chpasswd` over stdin. Every command writes a private durable snapshot before
mutation and records the applied fingerprint afterward; restore refuses newer
changes. User deletion never removes the home directory and is refused while
the account owns live processes. Private primary groups, shadow aging, shells,
UID/GID, and supplementary memberships are included in rollback.

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
