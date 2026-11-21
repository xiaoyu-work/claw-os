# Apps

The system Agent discovers permitted App MCP tools through `cos_tool_search`,
checks their live schema with `cos_tool_describe`, and invokes them with
`cos_tool_call`. The catalogue is authoritative. Human CLI examples below use
`yu app <name> <command>`; a CLI example does not make that command model-callable.
Apps never invoke other Apps or orchestrate Agent tasks themselves.

## Browser (Web Reading)

Fetch web pages as clean Markdown with JavaScript rendered. No Selenium needed:

```bash
yu app web read https://example.com
yu app web screenshot https://example.com
yu app web submit https://example.com/form --data '{"q": "search term"}'
```

## Web Search

Search the web through an explicitly selected Google or Brave provider:

```bash
yu app search web --provider brave "Rust async runtime" --max-results 5
yu app search image --provider brave "architecture diagram" --max-results 5
```

Requires credentials:
```bash
yu credential store BRAVE_SEARCH_API_KEY "BSA..." --tier 1
# Or configure Google:
yu credential store GOOGLE_SEARCH_API_KEY "AIza..." --tier 1
yu credential store GOOGLE_SEARCH_ENGINE_ID "a1b2c3..." --tier 1
```

## Email

Read, search, draft and send through the owner's existing Thunderbird accounts.
Discover accounts first and explicitly select the returned account ID:

```bash
yu app email accounts
yu app email folders --account ACCOUNT_ID
yu app email search --account ACCOUNT_ID --query "urgent" --max-results 10
yu app email list --account ACCOUNT_ID --unread --max-results 5
yu app email read --account ACCOUNT_ID --id MESSAGE_REFERENCE_ID
yu app email draft_save --account SENDER_ACCOUNT_ID \
  --to user@example.com --subject "Report" --body "Review the report"
```

There is no Gmail/Outlook API, SMTP or credential-detection fallback. The OS
starts the owner's Thunderbird through the user-session service manager when
needed; no Agent UI or model request is required. Unavailable session/backend
services fail explicitly. Mailbox read/draft and sender rights are separately
account-scoped, not borrowed from OAuth grants or a product name.

Message and draft results carry `thunderbird-message` / `thunderbird-draft`
references. Copy returned IDs: they include profile/account/folder/RFC
Message-ID, not Thunderbird's numeric IDs that change on restart.
`native_message` / `native_draft` resolve them using existing bounded mailbox
rights. Historical Gmail/Outlook objects are not remapped to native messages.

`attachment_download` requires an explicit mailbox account/message/MIME-part ID and a
canonical new output path whose parent already exists. Provider filenames never
select destinations; downloads are private, bounded and non-overwriting.
`draft_save` persists a native draft without sending. Replacing requires its
explicit `source-account`. `native_draft --id ...` returns a complete review,
including Bcc and attachment byte fingerprints. `draft_send` requires the
draft's mailbox `account`, an explicit `sender-account`, its current `review`
and `confirm=true`. These accounts can differ for Local Folders; their grants
remain independent. It sends the captured reviewed contents and removes only
an unchanged source draft; it does not prove recipient delivery.
`email.draft` is still an AI text helper only.

Keep one UUID `request-id` for each reviewed `send`, `draft_save` or `draft_send`
action, retain it in the Activity handoff, and retry only the same arguments.
Different/corrupt/in-flight request records never automatically resubmit.
An event-bookkeeping error does not justify repeating the Mail effect.

## Calendar

Select `local`, `google` or `outlook` explicitly. Local CRUD needs no remote
account, but is not a meeting-delivery or account-synchronization engine:

```bash
yu app calendar create --provider local --title "Standup" \
  --start "2026-11-08T09:00:00Z" --end "2026-11-08T09:30:00Z"
yu app calendar today --provider local
yu app calendar list --provider local --from "2026-11-08" --to "2026-11-09"
yu app calendar update --provider local --id EVENT_ID --title "New title"
yu app calendar delete --provider local --id EVENT_ID
```

`local_event`, `google_event` and `outlook_event` are independent ID-only object
resolvers. `invite` and `respond` use only Google/Outlook and require explicit
confirmation. Invites require distinct attendee addresses and offset-aware start/
end times. Local CRUD never pretends to notify guests; provider request success
is not guest delivery proof.

## Cross-App Activities

An Activity is the persistent owner goal; a Job is one attempt, and a Session
is its conversation/authority context. Use the existing Activity service and
ordinary authorized App Mesh, not a workflow DSL, App intercall or new queue.
[Activity lifecycle](../../docs/activities.md) and
[finite execution limits](../../docs/activity-execution-limits.md) are shared
between terminal, Web and native presentation.

| Goal | Authorized system-Agent sequence |
| --- | --- |
| Process an invoice | Read a Mail message and attachment list; download the selected attachment to an explicit new path; read it through Doc/Files; write an extraction/report through Files; save a Mail draft for review. |
| Prepare a meeting | Read the Mail request and relevant Calendar events; prepare the agenda in Files; request a confirmed provider invitation; save/read/review/send the Mail follow-up with a retained request ID. |
| Research and report | Search/read web sources; retain source attribution; write a Files report or staged change proposal; review its actual diff; save a provider Mail draft rather than silently sending AI text. |
| Reconcile tabular data | Read a document through Doc; submit scoped statements to DB or explicit KV records; export a Files report; notify only after normal outbound authority and review. KV data is not Agent memory. |
| Diagnose and repair | Inspect Diagnostics/Security/Events evidence; prepare an exact maintenance/package/container action; settle its normal OS approvals; retain results and uncertainty before notifying. |
| Respond from a channel | Consume a provider-verified owner-bound channel event; execute the selected bounded Activity steps; call that connector's confirmed receipt-bound `reply`, not an arbitrary recipient or App-originated Agent call. |

Attach only returned canonical references as artifacts. Root captures declared
`object`, `reference` and `artifacts` result fields from successful App execution
and records caller reports separately from authenticated declaration snapshots.
Attachment does not resolve/read the object or grant permission. Effect previews
are non-executing App declarations; unresolved targets and unknown recovery must
stay visible.

When explicitly enabled Activity work exposes `cos_activity_checkpoint`, call
it alone at the segment end with accurate summary, completed steps, returned
artifact references and a concrete next action. `continue` means different
useful work remains; `wait` means an external condition must change; `review`
requests owner verification; `blocked` requests human input. Never repeat an
uncertain side effect, silently reset progress, or infer Activity completion
from a successful Job. Only the owner explicitly completes the goal.

Mail `sync` reads one bounded inbox history/delta page without a model. Its first
pass is an inert baseline; later new-message references enter `app:email`.
Calendar and Files publish their own reported events after successful changes.
An owner-created, normally approved Activity trigger consumes these sources.
Publication itself neither delegates work nor proves an external effect.

## Selected message channels

Only Discord, Telegram, Slack and WhatsApp are selected. Their IDs remain
`gateway-discord`, `gateway-telegram`, `gateway-slack` and `gateway-whatsapp`.
Existing outbound `send` operations remain independent.

A human configures exact provider account, channel, sender and owned Activity
trigger through `yu channels configure PLATFORM` with the closed JSON request
on stdin. The trigger must match `channel:gateway-PLATFORM` and
`message.received`, with active finite limits and its approved generation.
The model and Apps cannot supply owner identities, credentials or trigger
capability snapshots. See the [channel contract](../../claw-os-sdk/wire/v1/channel-ingress.md).

Managed `start`, `stop` and `status` use `clawd`, independent of an active model.
Discord/Telegram use authenticated provider polling with a non-replaying initial
baseline; Slack/WhatsApp require signed HTTP requests and operator-owned public
HTTPS forwarding to the loopback receiver. Discord local `configure` preferences
and old App PID/cursor/session files do not become Root admission.

Use `reply` with the original `channel-event-...` ID, explicit text and
`confirm=true`. Root resolves the original owned account/recipient/thread;
WhatsApp BSUIDs stay opaque and do not require a phone number. Identical completed
replies return retained results; uncertain submissions never automatically resend.
Stopping admission does not undo requests or cancel existing Jobs. Unavailable
receivers, credentials, capacity or corrupt progress are explicit holds.

## File System

```bash
yu app fs ls /home/cos
yu app fs read /home/cos/file.txt
yu app fs write /home/cos/output.txt --content='hello'
yu app fs stat /home/cos/file.txt
yu app fs search "pattern" /home/cos
yu app fs rm /home/cos/tmp
yu app fs mkdir /home/cos/new-dir
```

Filesystem commands use the MCP-only contract. Writes require explicit
`--content` (strict base64 for `write_bytes`); `--stdin` is not supported.

## Documents

Read PDFs, DOCX, XLSX, PPTX, CSV, and other formats as structured text:

```bash
yu app doc read document.pdf
yu app doc read spreadsheet.xlsx
yu app doc read presentation.pptx
yu app doc info document.pdf
```

## Database (SQLite)

```bash
yu app db exec mydb "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)"
yu app db exec mydb "INSERT INTO users (name) VALUES ('Alice')"
yu app db query mydb "SELECT * FROM users"
yu app db tables mydb
yu app db schema mydb users
yu app db databases
```

## HTTP Client

```bash
yu app net fetch https://api.example.com/data
yu app net fetch https://api.example.com/data --method POST --data '{"key": "value"}'
yu app net download https://example.com/file.zip --output /home/cos/file.zip
```

## Key-Value Store

Persistent App-owned key-value storage for explicitly selected records, not
Agent memory, another product's private data or credentials:

```bash
yu app kv set project:status "building"
yu app kv get project:status
yu app kv list "project:*"
yu app kv del project:status
```

## System Info

```bash
yu sys info
yu sys env
yu sys resources
yu sys uptime
```

## Browser

Use the `web` App to read or render public pages in isolated Helium. Use
`browser-attached` only for the user's authorized, signed-in Helium tabs.
The two modes never share a browser profile.

## Network Doctor

```bash
yu app netdiag interfaces
yu app netdiag routes
yu app netdiag dns example.com
yu app netdiag tcp example.com:443 --attempts 3
yu app netdiag diagnose example.com:443
```

`diagnose` checks the local link, IPv4 default route, one-time DNS resolution,
and TCP reachability in order. TCP probes connect to the exact addresses
returned by that resolution, so a second DNS lookup cannot redirect the
connection.

## NetworkManager Control

```bash
yu app network-manager status
yu app network-manager wifi-list
yu app network-manager wifi-connect "Cafe WiFi" default/cafe_wifi_psk
yu app network-manager wifi-toggle off
yu app network-manager vpn-list
yu app network-manager vpn-up work-vpn
yu app network-manager airplane on
```

The optional Wi-Fi credential uses `namespace/name` form and is loaded only
after exact `secret.read` and credential-tier checks. Its plaintext is written
only to a root-only temporary nmcli password file and never placed in argv.
Network mutation grants are category-scoped as `net.manage:wifi`,
`net.manage:vpn`, or `net.manage:airplane`; user-controlled SSIDs and profile
names are never interpreted as glob capability patterns.

## Crash Doctor

```bash
yu app crash-doctor recent 60 20
yu app crash-doctor diagnose 60 20
yu app crash-doctor backtrace <boot-id>:<pid>:<timestamp-us>
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
yu app storage-manager status
yu app storage-manager health /dev/sdb
yu app storage-manager check /dev/sdb1
yu app storage-manager mount /dev/sdb1
yu app storage-manager unmount /dev/sdb1
yu app storage-manager eject /dev/sdb
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
yu app audio-manager status
yu app audio-manager output-volume 75
yu app audio-manager output-mute toggle
yu app audio-manager input-mute on
yu app audio-manager output-default 42
yu app audio-manager output-route 42 1
yu app audio-manager profile 30 2
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
yu app desktop-manager list
yu app desktop-manager focus <identifier>
yu app desktop-manager close <identifier>
yu app desktop-manager restart <identifier> <app-id>
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
yu app bluetooth-manager status
yu app bluetooth-manager power AA:BB:CC:DD:EE:FF on
yu app bluetooth-manager scan AA:BB:CC:DD:EE:FF 15
yu app bluetooth-manager pair AA:BB:CC:DD:EE:FF 11:22:33:44:55:66
yu app bluetooth-manager pair-respond <pairing-id> yes
yu app bluetooth-manager trust AA:BB:CC:DD:EE:FF 11:22:33:44:55:66
yu app bluetooth-manager connect AA:BB:CC:DD:EE:FF 11:22:33:44:55:66
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
yu app power-manager status
yu app power-manager suspend --confirm
yu app power-manager hibernate --confirm
yu app power-manager reboot --confirm
yu app power-manager poweroff --confirm
```

`status` reads UPower devices and logind `Can*` capabilities. Every state
change requires the critical unscoped `sys.power` capability plus an explicit
`--confirm`. The broker writes a durable power intent before sending a
no-reply logind request, so suspend/reboot/poweroff is recorded even if the
machine transitions before the normal clawd response audit completes.

## Hardware Center

```bash
yu app hardware-center summary
yu app hardware-center cpu
yu app hardware-center gpu
yu app hardware-center usb
yu app hardware-center memory
yu app hardware-center drivers
yu app hardware-center thermal
```

Hardware Center combines kernel sysfs/proc data with bounded lscpu, lspci,
lsblk, and dmidecode enrichment. It reports bound drivers, IOMMU groups,
firmware identity, DIMM metadata, USB authorization, CPU vulnerability state,
temperatures, and fans under the read-only `sys.observe:hardware` scope.
Individual provider failures remain explicit inside `summary` rather than
silently producing an empty healthy-looking section.

## Security Center

```bash
yu app security-center summary
yu app security-center auth
yu app security-center ssh
yu app security-center sudo
yu app security-center mac
yu app security-center ports
yu app security-center events
```

Security Center uses the high-risk read-only `sys.security:audit` scope because
its evidence contains login sources, sudo rules, effective SSH policy, process
listeners, and mandatory-access-control denials. It flags repeated auth
failures, direct root SSH, empty/password authentication, invalid or writable
sudoers files, broad NOPASSWD rules, missing enforcing MAC, sensitive wildcard
listeners, and recent kernel security events.

## Container Manager

```bash
yu app container-manager status
yu app container-manager list docker
yu app container-manager list podman
yu app container-manager list containerd k8s.io
yu app container-manager logs docker web 200
yu app container-manager namespaces podman web
yu app container-manager restart docker web
yu app container-manager remove docker old-job --confirm
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
yu app config-editor inspect /etc/hosts
yu app config-editor validate /etc/hosts /home/cos/hosts.new
yu app config-editor apply /etc/hosts /home/cos/hosts.new --confirm
yu app config-editor restore /etc/hosts <backup-token> --confirm
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
yu app event-center status
yu app event-center recent
yu app event-center recent security 50
yu app event-center recent storage 50
yu app event-center watch-pid 1234
```

Event Center starts with clawd and maintains restartable subscriptions to udev,
systemd D-Bus signals, and filtered journal follow output. Storage and security
events are classified into dedicated sources, while requested process watches
use pidfds and verify the original PID start time. Records are broadcast
in-memory and persisted to a bounded rotating JSONL log under
`sys.events:observe`; watcher failures remain visible in `status`.

## Backup Center

```bash
yu app backup-center init /media/cos/backup/repo default/restic
yu app backup-center backup /media/cos/backup/repo /home/cos/Documents default/restic
yu app backup-center snapshots /media/cos/backup/repo default/restic
yu app backup-center check /media/cos/backup/repo default/restic
yu app backup-center restore /media/cos/backup/repo latest /home/cos/restore default/restic --confirm
yu app backup-center retention /media/cos/backup/repo default/restic 7 4 12 --confirm
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
yu app firewall-manager status
yu app firewall-manager add allow input tcp 22 --remote 192.0.2.0/24 --interface eth0
yu app firewall-manager add deny input tcp 22 --remote 0.0.0.0/0
yu app firewall-manager delete <rule-id>
yu app firewall-manager clear --confirm
yu app firewall-manager restore <backup-token> --confirm
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
yu app user-manager status
yu app user-manager create-user alice --full-name "Alice" --groups sudo
yu app user-manager set-password alice default/alice-password
yu app user-manager add-to-group alice docker
yu app user-manager lock-user alice
yu app user-manager delete-user alice --confirm
yu app user-manager restore <backup-token> --confirm
```

User Manager only mutates regular UID/GID ranges and refuses root/system
accounts. Password plaintext is loaded through exact `secret.read` and sent to
`chpasswd` over stdin. Every command writes a private durable snapshot before
mutation and records the applied fingerprint afterward; restore refuses newer
changes. User deletion never removes the home directory and is refused while
the account owns live processes. Private primary groups, shadow aging, shells,
UID/GID, and supplementary memberships are included in rollback.

## Printer Manager

```bash
yu app printer-manager status
yu app printer-manager capabilities office
yu app printer-manager jobs office
yu app printer-manager print office /home/cos/document.pdf --copies 2 --sides two-sided-long-edge
yu app printer-manager cancel office-42 --confirm
```

Printer Manager talks to CUPS as the requesting user. The broker opens the
approved source with `O_NOFOLLOW` and passes that pinned file descriptor to
`lp` stdin, so path replacement cannot print a different file. Printing uses
`device.printer:print` plus exact `fs.read`; queue inspection and cancellation
use separate `observe`/`control` scopes. Cancellation is limited to jobs owned
by the requesting CUPS username.

## Clipboard Manager

```bash
yu app clipboard-manager status
yu app clipboard-manager types
yu app clipboard-manager read text/plain
yu app clipboard-manager write /home/cos/note.txt text/plain
yu app clipboard-manager clear --confirm
```

Clipboard contents are treated as sensitive: read/type operations require
`clipboard.read:selection`, while write/clear require
`clipboard.write:selection`. Source
files are opened with `O_NOFOLLOW` and passed to `wl-copy` by descriptor.
Binary reads return base64 and are capped at 4 MiB. The XDG Clipboard portal is
reported in status when present, but that portal is defined for RemoteDesktop
sessions; normal COSMIC desktop access uses the installed Wayland data-control
`wl-clipboard` tools.

## Camera Manager

```bash
yu app camera-manager status
yu app camera-manager capture 42 100 /home/cos/Pictures/capture.png png 1920 1080
```

Camera discovery reads only PipeWire `Video/Source` nodes. Capture requires
`device.camera:capture` plus exact `fs.write` for a new destination, rechecks
the node's object serial, and runs a one-frame GStreamer `pipewiresrc`
pipeline. Output is written to a user-owned temporary file, bounded to 64 MiB,
fsynced, and atomically persisted without overwriting. Status reports whether
the XDG Camera portal exists; that portal mediates sandboxed sessions, while
this host-level manager uses the approved PipeWire node directly.

## Accessibility Manager

```bash
yu app accessibility-manager status
yu app accessibility-manager screen-reader on
yu app accessibility-manager magnifier on
yu app accessibility-manager invert on
yu app accessibility-manager filter deuteranopia
```

Screen-reader control updates both `org.a11y.Status.IsEnabled` and
`ScreenReaderEnabled`, rolling the first property back if the second fails.
Magnifier, inversion, greyscale, and daltonization filters use the COSMIC
`cosmic_a11y_manager_v1` protocol and wait for compositor state events before
reporting success. Mutations require `ui.accessibility:control`.

## Display Manager

```bash
yu app display-manager status
yu app display-manager scale eDP-1 1.25
yu app display-manager position HDMI-A-1 1920 0
yu app display-manager mode HDMI-A-1 2560 1440 --refresh 144 --transform normal
yu app display-manager mirror HDMI-A-1 eDP-1
yu app display-manager brightness intel_backlight 70
yu app display-manager apply-layout /home/cos/layout.kdl --confirm
yu app display-manager restore <backup-token> --confirm
```

Display Manager uses the installed cosmic-randr protocol client and parses its
KDL output into modes/layout state. It refuses to disable the last active
output and requires at least one enabled output in full-layout KDL. Every
layout change stores before-KDL plus an applied hash; brightness stores exact
backlight values. Restore refuses newer changes. Mutations require
`device.display:manage`; full KDL additionally requires exact `fs.read`.

## USB Guard

```bash
yu app usb-guard status
yu app usb-guard authorize 1-2 on
yu app usb-guard authorize 1-2 off --confirm
yu app usb-guard block 1-2 --confirm
yu app usb-guard unblock <rule-id> --confirm
yu app usb-guard eject 1-2 --confirm
yu app usb-guard restore <backup-token> --confirm
```

USB Guard lists physical sysfs devices and mapped block nodes. Current
authorization writes the exact device's `authorized` attribute. Persistent
blocking owns one dedicated udev rules file and requires an exact
vendor/product/serial fingerprint; hubs and unsafe serial strings are refused.
Deauthorization/block/eject reject devices backing protected mounts or active
swap. Safe eject uses UDisks2 power-off for whole disks. Policy mutations are
revisioned and rollback-capable under `device.usb:control`; rollback also
restores the affected connected device's prior authorization when its exact
fingerprint is still unique. clawd reconciles the dedicated udev policy from
durable state on startup and reports any policy drift in `status`.

Persistent blocking is a udev enumeration policy, not a kernel-wide
default-deny mode: a newly attached device can exist briefly before udev
deauthorizes it. Use hardware or kernel USB authorization defaults where a
zero-enumeration trust boundary is required.

## Location Manager

```bash
yu app location-manager locate
yu app location-manager locate exact
yu app location-manager timezone city
```

Location Manager requests a fix from the system GeoClue broker as the
requesting desktop user, never as root. Accuracy can be `country`, `city`,
`neighborhood`, `street`, or `exact`; every request requires the no-scope
`device.location` capability. It also compares the coordinates with
`zone1970.tab` offline and returns advisory IANA timezone candidates without
changing the system timezone; this timezone-matching step makes no network
request. GeoClue may use whichever location sources the system administrator
enabled. The image allows `com.yuos.Agent` through GeoClue because Claw's
session capability approval is the built-in authorization mechanism.

## Package Management

```bash
yu app pkg need python3-pymupdf
yu app pkg has ripgrep
yu app pkg list
yu app pkg update
yu app pkg upgrade curl
yu app pkg install-version curl 8.0.1-1
yu app pkg hold curl
yu app pkg remove curl
```

Single-package install, upgrade, remove, and hold changes record the previous
installed version and hold state on a durable parent task. Purge, global index
refresh, and full-system upgrade require a system snapshot for complete
rollback.

## Native System Services

Use the `systemd` app for native systemd units. This is distinct from the
`cos_service` tool, which manages Claw-defined service manifests.

```bash
yu app systemd status ssh.service
yu app systemd restart ssh.service
yu app systemd enable ssh.service
yu app systemd disable ssh.service
```

Status requires `sys.observe:<unit>`. Lifecycle changes require the exact
`sys.service:<unit>` grant and are executed by the root clawd broker. Start,
stop, enable, and disable record the previous state on a durable parent task
for rollback.

## System Recovery Points

```bash
yu app system-snapshot status
yu app system-snapshot create "before distribution upgrade"
yu app system-snapshot list
yu app system-snapshot rollback snap_<id> --confirm
```

The broker prefers a configured Snapper root profile, otherwise uses direct
read-only Btrfs snapshots or LVM snapshots. Snapper and LVM rollback require a
reboot. Direct Btrfs snapshots can be created and deleted, but live-root
rollback is refused until a bootloader-aware restore path is configured.
