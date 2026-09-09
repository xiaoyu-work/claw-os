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
- Provide channel-neutral delivery claims and the ntfy adapter.

Native `cosmic-notifications` submits bounded intent through
`clawd/app_notifications.rs`. The service stores display-only sender, theme
icon, popup timeout and transient metadata, never caller-provided authority.
The additive SQLite v1 → v2 upgrade preserves every existing record and
preference; the wire notification schema remains backwards-compatible v1.
Owner and source are immutable and jointly partition deduplication.
Acknowledgement/dismissal suppress pending deliveries; a stale receipt cannot
requeue terminal records. Delivery success never implies user acknowledgement.

The existing Agent desktop bridge remains the sole desktop delivery consumer.
Its connection-local numeric handles are not durable IDs or an alias store.
Legacy `notify` JSON history remains untouched pending its explicit transition.

## Key Files

| Path | Role |
| --- | --- |
| `mod.rs` | Stable service definition and domain model |
| `sqlite.rs` | Durable SQLite provider |
| `ntfy.rs` | Opt-in ntfy delivery adapter |
| `../../test/unit/notifications/` | Unit and adapter tests |

## Dependencies

Producers and `clawd` depend on the `NotificationService` definition. They do
not reach into SQLite tables or delivery adapters directly. The desktop and Web
surfaces consume owner-scoped broker routes.

## Tests

```bash
cargo test -p cos notifications:: -- --test-threads=1
```
