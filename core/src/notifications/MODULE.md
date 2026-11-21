# Notifications Module

## Purpose

`core/src/notifications/` owns the durable, owner-scoped user-attention
service. Producers publish bounded notification facts; delivery adapters and
user interfaces consume them without relying on model behavior.

## Responsibilities

- Define the versioned notification, preference, change, and delivery models.
- Persist notifications and per-channel delivery state in SQLite.
- Enforce owner isolation, deduplication, retention, acknowledgement, DND, and
  retry leases.
- Provide transactional owner/task projections with full retained and unread
  counts before bounded detail limits.
- Provide channel-neutral delivery claims and the ntfy adapter.
- Mark all retained, unexpired unread notifications for one authenticated owner
  in a single transaction, including records beyond presentation limits.
  Per-record read changes remain observable; acknowledgement, dismissal,
  delivery state, Jobs and approval authority are unchanged.

Authenticated Apps submit capability-gated intent through
`clawd/app_notifications.rs`. Post/close/send require `ui.notify`; source-scoped
list independently requires `data.inbox.read`. Every publication, close and list
binds to the App ID in the sealed grant, not a caller-supplied source or display
name. Ordinary IDs retain `app:<id>`; IDs too long for the existing 128-byte
source bound use a deterministic `app:sha256:<digest>` encoding.
Existing `app:notify` and `app:cosmic-notifications` records are unchanged.
The service stores display-only sender, theme
icon, popup timeout and transient metadata, never caller-provided authority.
The additive SQLite v1 → v2 upgrade preserves every existing record and
preference; the wire notification schema remains backwards-compatible v1.
Owner and source are immutable and jointly partition deduplication.
Acknowledgement/dismissal suppress pending deliveries; a stale receipt cannot
requeue terminal records. Delivery success never implies user acknowledgement.
The `Activity` delivery policy is durable-only and never queues Web, desktop,
or ntfy interruption. Activity-linked Agent lifecycle updates use one
owner/source/Activity dedupe key during the normal deduplication window, so
`occurrences` exposes each bounded batch while the latest linked Job and
session remain inspectable. Queue/start/resume/recovery progress stays in task
streams and audit rather than creating notifications. Background terminal
outcomes retain their existing delivery policy; outcomes of a currently viewed
conversation are presented inline instead. Genuine approval and critical
alerts remain separate.

The broker holds bounded owner/viewer conversation leases in memory. Web sends
focus/visibility updates and five-second renewals; leases expire after fifteen
seconds. Revisions prevent late updates from restoring an abandoned view, and
multiple viewers remain independent. These are notification hints only, never
the kernel-attended `SessionPresence` proof or permission/consent authority.

The Root-held `clawd/notifications/gui.rs` resource is the sole automatic
desktop delivery consumer. An explicitly declared and granted
`ipc.subscribe:notifications.presentation` admits bounded leased display data;
the independent `ipc.invoke` right admits exact presentation controls.
Neither `ui.notify`, `data.inbox.read`, `ui.panel`, a signature nor an App name
selects an all-source presenter. Only the authenticated private GUI endpoint
can install this resource's process/session binding. A second presenter or
public desktop claimant cannot take custody before checked retirement.
Its numeric handles and random per-delivery tickets are not durable IDs or
an inbox/authority store. Renderer acceptance marks delivery, never user
acknowledgement; expiry, eviction and retirement leave unread records unread.
Display receipts additionally compare the retained publication occurrence and
delivery attempt transactionally. A late acceptance/action cannot deliver or
acknowledge a newer deduplicated publication, change a terminal record or
settle another retry. Replacements receive fresh handles while the original
UI swaps the displayed card without a transient duplicate or blank surface.

The OS-owned `claw-notification-panel` terminates the existing shell descriptor
and relays the public bounded rendering/preferences contract. It owns neither
App configuration nor a user bus. The native App keeps its original popup UI,
private preference file and image preparation. Popup mute is distinct from
Root DND. Late preference acknowledgements are observable after a timeout,
without replaying the write. An authenticated descriptor peer's EOF retires
the helper normally and stops its child connections; failed authentication,
protocol errors and unrelated I/O errors remain failures. This is separate
from checked GUI/lease retirement and never acknowledges notification data.
The Agent bridge no longer claims desktop leases.
The distinct `notify` send/list service intents use source `app:notify`, not
the native producer's authority. `list_source` returns one transactional
owner/source snapshot with newest-publication-first rows and a full retained,
unexpired total, including read/acknowledged/dismissed records. Urgent sends
use warning severity and never the critical DND bypass.
Historical `notify` JSON is deliberately not read, imported, replayed or moved;
the worker's automatic legacy-state move no longer selects it.

## Key Files

| Path | Role |
| --- | --- |
| `mod.rs` | Stable service definition and domain model |
| `sqlite.rs` | Durable SQLite provider |
| `ntfy.rs` | Opt-in ntfy delivery adapter |
| `visibility.rs` | Bounded, expiring, owner-scoped conversation-view hints; no durable state or authority |
| `../clawd/notifications/gui.rs` | Exact signed-GUI resource, one owner presenter, leased display tickets and checked lifetime |
| `../bin/claw-notification-panel.rs` | OS shell descriptor and public rendering/preferences proxy; no App code/configuration |
| `../../test/unit/notifications/` | Unit and adapter tests |

## Dependencies

Producers and `clawd` depend on the `NotificationService` definition. They do
not reach into SQLite tables or delivery adapters directly. The desktop and Web
surfaces consume owner-scoped broker routes. Activity attention filters by
authenticated owner and associated task IDs; notification acknowledgement is
presentation state, never an approval decision.

## Tests

```bash
cargo test -p yu notifications:: -- --test-threads=1
```
