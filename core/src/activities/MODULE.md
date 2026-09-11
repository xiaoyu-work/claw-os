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
- Append immutable caller-reported execution receipts without upgrading reports
  or App-declared effects into OS-confirmed changes or authority.
- Keep bounded object observations, receipt links, planning relations and
  correction/retraction history in the same owner-scoped Activity database.
- Reject unreadable, invalid, unsupported-schema, and poisoned-lock storage
  explicitly rather than resetting the database or returning empty defaults.

## Key Files

| Path | Role |
| --- | --- |
| `mod.rs` | Domain types, validation, and the `ActivityService` definition |
| `sqlite.rs` | Transactional SQLite provider |
| `receipts.rs` | Receipt types, strict report validation, and declaration bounds |
| `object_state.rs` | Object-state domain, canonical references/windows, and strict draft validation |
| `sqlite/object_state.rs` | Immutable object-state ledger, reference/receipt binding, schema-3 migration and integrity checks |
| `../clawd/activities.rs` | Owner-scoped broker consumer and execution projections |
| `../activity.rs` | Terminal presentation |
| `../../test/unit/activities/` | Domain and provider unit tests |

## Persistence and Authority

The root daemon opens `crate::paths::data_dir().join("activities.db")`.
Direct clients never open this file. The provider has no desktop, agent-loop,
worker, or model-provider dependency, and Activity metadata grants no new
capabilities or approvals.

`ActivityDraft::validate`, `ActivityPatch::validate`,
`ObjectStateDraft::validate`, and `validate_id` validate inputs without opening
storage. UUID lookups return a canonical `Activity.id`; job/session associations
should store that returned value.

On Unix, new private directories use `0700`; an existing non-listable,
non-writable shared daemon root may retain its traversal bits (`0711`) so
isolated workers can reach their own state partitions. Database and SQLite
sidecar files use `0600`. Symlink database files, sidecars, and immediate
parent directories are rejected.

Disk connections require WAL journaling, `synchronous=FULL`, and a five-second
busy timeout. Creation, partial updates, transitions, receipt appends and
object-state appends use immediate transactions. A new empty database receives
schema version 3 through explicit sequential migrations. Schema 1 first adds
the schema-2 `activity_receipts` ledger; schema 2 then adds
`activity_object_state` and the receipt composite index needed for
owner/Activity-bound links. Neither migration rewrites Activity rows,
resources, lifecycle state, timestamps, completion confirmations or receipts.
The version advances only after schema/index/foreign-key definitions, SQLite
integrity, foreign-key relationships and persisted legacy metadata validate.
A failure rolls back both schema additions and the version. An existing
unversioned schema, unsupported old/future versions, missing columns, or failed
integrity checks cause an error. Orphaned SQLite journals are preserved rather
than initializing a replacement database over them. Reads validate stored
metadata rather than silently repairing it. The in-memory provider is for
tests and uses SQLite's in-memory journal instead of WAL.

`DATABASE_SCHEMA_VERSION = 3` controls SQLite `user_version` and migration.
The broker independently owns `clawd::activities::WIRE_SCHEMA_VERSION = 1`
for `activity.list` and `activity.get`; it does not emit the database version.
The existing public `activities::SCHEMA_VERSION = 1` is retained for wire
compatibility. Database upgrades do not change Activity response schemas or DTOs.

## Bounds and Lifecycle

Activity planning text limits apply to final trimmed UTF-8 bytes, not character counts.
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
opened, copied, or executed by the store. App object attachments use canonical
URIs in this same field and an atomic append/upsert operation; the broker's
separate metadata catalogue authenticates declarations without fetching data.
See [App-owned objects](../../../docs/app-objects.md).

Activities start `active`. Active and paused records can pause/resume or
explicitly complete/cancel. Terminal records can only reopen to `active`;
reopening clears the previous completion note. No-op transitions are rejected.
Every completion requires a nonempty user confirmation note, even after a
successful Job; other transitions reject a supplied completion note.

Only active records allow future work. Pausing or cancelling does not cancel
workers or undo effects. Planning fields can be edited while active or paused;
terminal records must first be explicitly reopened. Empty patches are invalid,
and omitted fields are preserved.

## Immutable Execution Reports

`ActivityService::record_receipt` appends to the ledger in the same
`activities.db`; `receipts` reads bounded recent reports. There are no receipt
update or delete APIs, and existing Activity/Resource JSON is unchanged.
Receipts do not execute or replay operations, grant permission, reopen an
Activity, or complete its goal. Recording is allowed in every Activity state:
an in-flight operation may report after pause, completion, or cancellation.

The provider sets the owner, received timestamp, and the sole source value
`caller_reported`. A user or model cannot supply a source or confirmed-effect
field in `ReceiptReport`. Root has no cross-owner exemption. Owner/Activity
existence and quotas are checked atomically, with a composite foreign key
preserving the ownership relation.

The caller creates a UUID idempotency key. Only its spelling is canonicalized;
report text is not trimmed or rewritten. An exact canonical report retry for
the same owner and Activity returns the original immutable record, including
the original timestamp and declaration/error. It does not refresh a stale
declaration or replace an earlier verification failure. Reusing that owner's
key with a changed report or another Activity is a conflict. Other owners may
independently use the same UUID. There are at most 1000 receipts per Activity;
exact retries still work at the limit. Lists default to 50, cap at 100, and use
insertion order rather than trusting caller times or wall-clock monotonicity.

`Returned` requires a result without an error; `ReportedError` requires both;
`Indeterminate` requires an error without a result. Errors are nonempty and at
most 2048 UTF-8 bytes. Result summaries contain a canonical lowercase
`sha256:` digest, at most 16 MiB of reported original output, and at most 2048
UTF-8 bytes of preview. Preview controls are limited to newline, carriage
return, and tab. JSON/text results require nonzero original bytes; empty
results require zero bytes, SHA256(empty), no preview, and no truncation.
The digest and byte count are caller reports about the original output, not
proof of execution. A redacted or truncated preview need not be valid JSON,
match the digest, or have the same length as the original output.

One-shot operation names retain their existing grammar. App-owned session
receipts use `session:<tool>`, where the tool is at most 128 ASCII bytes and
matches the manifest's `[a-z][a-z0-9._-]*` grammar. This is a qualified identifier
in the existing string field, not an execution route or another App category.
Session results summarize the rendered MCP output, including its explicit
error flag, rather than the raw protocol envelope.

The broker redacts previews/errors before storage and supplies exactly one
verified declaration snapshot or a bounded declaration error. A declaration
is only current signed-manifest metadata matched to the reported package
digest: App version (128 bytes), operation label (512 bytes), and at most 16
App-declared effects with labels bounded to 512 bytes. Effects retain only
kind, label, recovery guidance, and optional argument name, never requested or
final target values. An argument name, when present, must be nonempty, contain
no control characters, and fit 128 UTF-8 bytes. Declarations remain separate
from caller-reported results and do not prove actual effects or the existence
of an inverse.

`ReceiptDeclaration::validate` lets the broker check metadata bounds before
recording. Invalid metadata can be represented as `declaration_error` rather
than discarding the execution report; this validation does not authenticate an
App package or establish that its digest matches the report.

The provider does not discover Apps, read object data or credentials, or depend
on agent redaction/runtime modules. It validates stored receipt JSON, UUID
relationships, source, timestamp, and report/declaration invariants; corruption
is an explicit error, never a repaired or silently skipped record.

## Object-State Reports and History

`ActivityService::record_object_state` appends an `ObjectStateDraft`;
`object_state` reads recent `ObjectStateEntry` history, optionally filtered by
an exact canonical App reference. These are shared backend APIs, not another
App store or a general graph. The provider only calls the pure public SDK
reference parser/formatter re-exported by `crate::objects`; it never discovers,
describes, resolves, executes or reads an App.

New subjects and relationship targets must already be attached to the owned
Activity. Relations use only `related_to`, `depends_on` or `derived_from`, and
cannot point from a reference to itself. They are planning links, not
execution dependencies. Detaching a subject or target does not remove its
history, and exact retries of existing entries still work after detachment.
New entries and corrections must satisfy the current attachment checks.

The provider fixes `source` to `caller_reported` and supplies the owner,
Activity ID and recording time. Drafts reject caller-supplied origin,
authentication, owner, confirmation and derived-state fields. `user_statement`
and `agent_inference` classify the content, not proven human/model authorship.
Optional draft fields (`observed_at`, `valid_until`, `supersedes`) may be
omitted on input, but output includes explicit JSON `null` when absent.
Nullable entry fields (`receipt`, `superseded_by`) likewise serialize as `null`,
never as omitted keys.
`app_report` has only a receipt ID, never a text or output override. It links
an existing immutable receipt with matching owner, Activity and reference App,
including indeterminate reports. Its entry projects the already validated
`ReceiptReport`, not original App data or declaration authority. This is not
proof of execution or semantic binding between that report and the object.

Statement/inference text and retraction reasons require 1..=4096 trimmed UTF-8
bytes; relation notes allow 0..=2048. Only ordinary newline, carriage return
and tab controls are allowed. References use the exact shared canonical URI
format. UUID spellings for the entry, receipt and superseded entry are
canonicalized consistently with receipts.

`observed_at` and `valid_until` must both be absent or both be RFC3339
timestamps, with the end strictly after the start. Storage canonicalizes them
to nanosecond UTC, independently of the server's `recorded_at`. Relations and
retractions reject these windows. Reads derive `unknown`,
`not_yet_applicable`, `within_reported_window` or `expired` from UTC now. A
reported window includes its start and excludes its end; being within it
never establishes freshness, truth or verification.

Corrections append a new ID with `supersedes` naming an existing entry on the
exact same subject in the same owner/Activity. Only an unsuperseded,
non-retracted entry can be corrected. Relations may change their target or
kind while keeping that subject. Composite foreign keys, sequence ordering
and unique supersession prevent missing/foreign predecessors, cycles and
competing corrections. Retractions require a predecessor and preserve all
earlier classifications and content. A retracted entry cannot itself be
corrected; a fresh independent entry can be added instead.

There are at most 1000 entries per Activity, including all superseded and
retracted history. An exact canonical draft retry returns the original
immutable data with current derived validity and supersession, even at the
limit. A changed draft or Activity under the same owner's ID conflicts; other
owners may reuse that UUID. Ownership, attachment, receipt binding,
supersession and quota checks occur in the same immediate transaction. Late
metadata is allowed in active, paused, completed and cancelled Activities
without changing lifecycle, goal, timestamps or completion confirmation.

Reads default to 50 entries, cap at 100 and sort by insertion sequence, not
caller times or a monotonic wall clock. The provider validates the bounded
ledger before applying a view's filter or limit, including canonical JSON,
column consistency, fixed source, receipt binding and correction edges.
Corruption fails explicitly, never disappearing through a filter or being
repaired by retry.

See [the Activity contract](../../../docs/activities.md) for presentations and
[object-state annotations](../../../docs/object-state.md) for the cross-client
contract, and [the system architecture](../../../ARCHITECTURE.md) for authority
boundaries.

## Tests

From the repository root on Linux or WSL:

```bash
cargo test -p cos --lib activities:: -- --test-threads=1
```

Receipt unit coverage includes schema-1 preservation, failed migration
rollback, process exit before migration commit, owner/root isolation, retries
and conflicts, lifecycle independence, quotas, and corrupt storage.
Object-state coverage adds strict wire/canonicalization and clock boundaries,
attachment/receipt binding, concurrent corrections and retries, retractions,
detached history, immutable appends, schema-1/2 preservation and failed or
interrupted schema-3 migration rollback.
