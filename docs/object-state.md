# Activity object state

Object state is a bounded history of **annotations about App-owned references**.
It adds statements, linked result reports, planning relationships and corrections
to an Activity. It does not copy an App's database, fetch an object, grant
permissions, schedule work or establish semantic truth.

Terminal, Agent Web and native desktop use the same owner-scoped Activity
service and `activities.db`. There is no presentation-owned object-state store.

## What a record means

| Content | Meaning |
| --- | --- |
| `user_statement` | An owner-submitted statement, with that reported classification |
| `agent_inference` | An inference, not an observed or verified fact |
| `app_report` | A link to an existing immutable Activity receipt, not new caller-authored App output |
| `relation` | A `related_to`, `depends_on` or `derived_from` planning link |
| `retracted` | An explicit reason to withdraw an earlier entry while retaining its history |

The broker fixes `source` to `caller_reported` and records the authenticated
owner and storage time. Content classifications do not authenticate human or
model authorship. An App report must reference a receipt belonging to the same
owner, Activity and App as the subject reference. Its existing result/error is
shown rather than accepting replacement text. The receipt remains
caller-reported: matching a package declaration does not authenticate execution,
and the caller's association does not prove that the result concerns this object.

References use the [public App object contract](app-objects.md). The subject,
and a relationship's target, must already be attached to the Activity. Removing
a resource later does not erase its recorded history. Reattach it before
recording a correction or retraction.

Relationships do not create execution dependencies. Cycles in planning links
do not cause execution or start a workflow engine.

## Terminal workflow

Inside an installed Linux or WSL environment, use an existing Activity and
attached App reference:

```bash
activity_id="00000000-0000-4000-8000-000000000001"
reference="app://kv/entry?id=release.status"

cos activity observe "$activity_id" --reference "$reference" \
  --text "The release is waiting for review"

cos activity observe "$activity_id" --reference "$reference" \
  --source agent_inference --text "The remaining blocker may be the review"

cos activity object-state "$activity_id" --reference "$reference" --limit 100
```

The source flag is a reported classification, not proof that an Agent authored
the text. Link an existing same-Activity receipt without supplying replacement
App output:

```bash
cos activity observe "$activity_id" --reference "$reference" \
  --receipt 00000000-0000-4000-8000-000000000002

cos activity relate "$activity_id" --reference "$reference" \
  --target "app://kv/entry?id=review.status" --relation depends_on \
  --note "Planning link only"
```

The target reference must also be attached. Neither command resolves an App
object or invokes a model.

## Reported time windows

Observation windows require both `--observed-at` and `--valid-until`, in
RFC3339 form, with the end strictly after the start. Omit both when validity
is unknown. Relations and retractions have no observation window.

The backend projects `unknown`, `not_yet_applicable`,
`within_reported_window` or `expired` using its current clock. These are
reported-window states, not verified freshness, availability or truth.
`recorded_at` is the broker's recording time, not a claim about when an external
event happened.

## Corrections and history

Every write creates an immutable entry with a UUID. Correct an entry by
submitting a fresh entry on the same exact reference with `--supersedes ID`.
Only the latest unsuperseded entry may be corrected. A racing or delayed
correction conflicts instead of silently replacing another writer's update.

```bash
cos activity observe "$activity_id" --reference "$reference" \
  --supersedes 00000000-0000-4000-8000-000000000003 \
  --text "Review is now complete; publishing still requires confirmation"

cos activity retract-object-state "$activity_id" --reference "$reference" \
  --supersedes 00000000-0000-4000-8000-000000000004 \
  --reason "This statement was not supported"
```

Retraction preserves all earlier records and their source classifications.
An already retracted entry cannot be corrected again; a fresh independent
entry may be recorded instead. Recording metadata is allowed after an
Activity pauses, completes or is cancelled, but never changes that lifecycle
or its completion confirmation.

`--id UUID` selects a retry key. Reuse the same ID and canonical draft when a
submission's result is unknown. An exact retry returns the original immutable
entry; changed content or another Activity under that owner's ID conflicts.
Current validity and supersession are read-time projections and may change.
`cos activity record-object-state ID --stdin` accepts the same draft used by
graphical clients, up to 16 KiB, for a recording-only retry.

## Shared presentation and Agent context

Web and native Activity detail show source classifications, bounded statements
or receipt results, reported windows, relations and superseded history. Forms
submit the same broker contract and never infer new authority from a record.
Late responses cannot replace another Activity's view or unsaved form.

When the broker claims an associated job, it adds bounded excerpts of at most
six unsuperseded entries to the existing **untrusted Activity context**. This
includes expiry and retraction caveats. The exact context is recorded through
the existing task stream, even with conversation memory disabled. A retry
refreshes the snapshot. Old or omitted annotations are not current facts, and
further source reads still need ordinary authorized tools. This does not add a
model call, rewrite a system prompt, or automatically create an observation.

## Bounds and compatibility

Statements and retraction reasons are at most 4096 UTF-8 bytes; relationship
notes are at most 2048. References retain the SDK's canonical 4096-byte bound.
There are at most 1000 entries per Activity, including history. Lists default
to 50 and return at most 100 recent entries, with optional exact-reference
filtering. These bounded views are not a complete global knowledge graph.

Database schema 3 adds the object-state ledger while preserving schema-1
Activities and schema-2 receipts. Activity and receipt wire formats remain
version 1. Unsupported versions, invalid stored records and failed migrations
are explicit errors, never an empty replacement store. Older cores must not
open schema-3 state; see [updating](updating.md).

The broker routes are `activity.object_state.list` and
`activity.object_state.record`. Neither request can choose an owner or a
trusted source. All models, terminal and graphical presentations remain
consumers of the same backend.
