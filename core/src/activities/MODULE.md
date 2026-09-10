# Activities Module

## Purpose

This is the single, desktop-independent backend for persistent user goals.
Terminal-only, Web, and native desktop installations share `ActivityService`
and the owner-scoped broker contract; only their presentations differ.

## Responsibilities

- Define bounded planning metadata, inert resource references, and explicit
  lifecycle transitions.
- Persist records through one SQLite provider, partitioned by authenticated
  owner UID. UID 0 has no cross-owner exemption.
- Keep goal completion separate from execution outcomes. Jobs and sessions own
  their associations and execution state; the broker builds related views.
- Reject unreadable, invalid, unsupported-schema, and poisoned-lock storage
  explicitly rather than resetting the database or returning empty defaults.

## Key Files

| Path | Role |
| --- | --- |
| `mod.rs` | Domain types, validation, and the `ActivityService` definition |
| `sqlite.rs` | Transactional SQLite provider |
| `../clawd/activities.rs` | Owner-scoped broker consumer and execution projections |
| `../activity.rs` | Terminal presentation |
| `../../test/unit/activities/` | Domain and provider unit tests |

## Persistence and Authority

The root daemon opens `crate::paths::data_dir().join("activities.db")`.
Direct clients never open this file. The provider has no desktop, agent-loop,
worker, or model-provider dependency, and Activity metadata grants no new
capabilities or approvals.

`ActivityDraft::validate`, `ActivityPatch::validate`, and `validate_id` validate
inputs without opening storage. UUID lookups return a canonical `Activity.id`;
job/session associations should store that returned value.

On Unix, new private directories use `0700`; an existing non-listable,
non-writable shared daemon root may retain its traversal bits (`0711`) so
isolated workers can reach their own state partitions. Database and SQLite
sidecar files use `0600`. Symlink database files, sidecars, and immediate
parent directories are rejected.

Disk connections require WAL journaling, `synchronous=FULL`, and a five-second
busy timeout. Creation, partial updates, and transitions use immediate
transactions. A new empty database receives schema version 1 transactionally;
an existing unversioned schema, a future version, missing columns, or failed
integrity checks cause an error. Orphaned SQLite journals are preserved rather
than initializing a replacement database over them. Reads validate stored
metadata rather than silently repairing it. The in-memory provider is for
tests and uses SQLite's in-memory journal instead of WAL.

## Bounds and Lifecycle

All text limits apply to final trimmed UTF-8 bytes, not character counts.
Titles and goals must be nonempty. Criteria and boundaries may be empty.

| Field or operation | Limit |
| --- | --- |
| Title / resource label | 240 bytes each |
| Goal | 16 KiB |
| Completion criteria / boundaries / confirmation note | 8 KiB each |
| Resources | 32 display references, each at most 4096 bytes |
| Owner records | 1000, including completed and cancelled records |
| List | Default 50, maximum 100, newest update first with an ID tie-breaker |

The provider interprets list limit zero as the default and caps larger limits;
the broker may reject out-of-range client requests earlier. NUL and unsupported
control characters are rejected, including controls in titles, labels, and
references. Goal, criteria, boundaries, and confirmation text support normal
newlines, carriage returns, and tabs. Resource references are never read,
opened, copied, or executed; typed App objects belong to a later phase.

Activities start `active`. Active and paused records can pause/resume or
explicitly complete/cancel. Terminal records can only reopen to `active`;
reopening clears the previous completion note. No-op transitions are rejected.
Every completion requires a nonempty user confirmation note, even after a
successful Job; other transitions reject a supplied completion note.

Only active records allow future work. Pausing or cancelling does not cancel
workers or undo effects. Planning fields can be edited while active or paused;
terminal records must first be explicitly reopened. Empty patches are invalid,
and omitted fields are preserved.

See [the Activity contract](../../../docs/activities.md) for presentations and
[the system architecture](../../../ARCHITECTURE.md) for authority boundaries.

## Tests

From the repository root on Linux or WSL:

```bash
cargo test -p cos --lib activities:: -- --test-threads=1
```
