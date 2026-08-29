# Agent Memory Recovery

Agent conversation history lives in SQLite at the path reported by:

```bash
cos agent sessions health
```

The focused health command and the `memory` section of `cos agent doctor`
report these dimensions separately:

- SQLite `integrity_check`;
- WAL mode and sidecar structure;
- authoritative tables and indexes;
- FTS contents and maintenance triggers;
- session-to-prompt references;
- SHA-256 verification of content-addressed prompt blobs;
- durable compaction lifecycle, source digests, protected boundaries, and
  content-addressed summary blobs;
- session-title ownership; and
- interrupted or failed repair lifecycle records.

Diagnosis takes the lifecycle lock long enough to copy the complete
database/WAL/SHM family into a private snapshot, then opens only that snapshot.
The live files remain byte-for-byte unchanged, including when SQLite would
otherwise create or rewrite `-shm`. WAL validation covers the format version,
header checksum, page size, every frame's page/commit fields, salts, and rolling
checksum. Diagnosis may therefore wait for an active Claw memory handle to
close. The FTS check builds a temporary in-memory index from authoritative
`messages` rows and compares token instances.

Runtime classification is explicit:

- a transient append failure is degraded operation and is logged while the
  current turn continues;
- missing or incompatible schema prevents `MemoryDb` startup until explicit
  repair; and
- a dangling or hash-invalid frozen prompt is fatal for that model request, so
  damaged instructions never reach the provider.

## Durable compaction

Conversation compaction never rewrites or deletes authoritative `messages`
rows. A completed projection records its session and generation, exact
source row IDs and inclusive range, source-content digest, algorithm/version,
protected tail and real-user anchor, provider/model, the frozen prompt
hash/version used at the time, and metadata that points recovery tooling back
to the searchable raw rows. Summary text is stored by SHA-256 and verified
before it can be replayed.

Continuation loading selects the newest valid completed projection and appends
all raw rows after its source boundary. Invalid newer projections are ignored
in favor of an older valid generation. A per-session advisory lock spans
summary generation. If the lock can be reacquired while a `started` record
still exists, the prior process did not complete it; the record is marked
`failed` and a later attempt may retry safely.

`cos agent replay <session-id>` continues to export the original message rows
and now includes the compaction lifecycle metadata and raw-row recovery range.
It does not substitute the summary for the audit source.

Before requesting an LLM summary, the compressor deterministically replaces
oversized tool results outside the protected tail with size-and-digest stubs.
If that projection is already below the trigger, it is persisted without an
extra model call. Tool call/result pairs are kept together, and the protected
tail always includes at least one genuine user message.

## Repair

Preview the exact plan without changing `memory.db`:

```bash
cos agent sessions repair --dry-run
```

Apply an in-place repair:

```bash
cos agent sessions repair --yes
```

Force a healthy FTS projection to be rebuilt:

```bash
cos agent sessions repair --rebuild-fts --yes
```

In-place repair checkpoints the WAL, restores schema objects and FTS triggers,
rebuilds FTS from `messages`, closes interrupted compactions, removes invalid
compaction summaries, and removes only orphaned title or prompt projection rows
after checking authoritative references in the same transaction.

If health reports `requires_quarantine`, preserve the damaged database and
initialize a replacement with:

```bash
cos agent sessions repair --quarantine --yes
```

The main database and any WAL/SHM sidecars are renamed on the same filesystem
to a unique `memory.db.quarantine-<id>` family. They are never deleted
automatically and are restricted to the owning account. When SQLite integrity
and WAL health permit safe reads, authoritative messages, valid titles, and
hash-verified prompts are copied into the replacement; damaged prompt blobs
are not trusted. If the WAL is malformed, repair first quarantines the complete
family, then validates and salvages a separate main-database-only copy so
already-checkpointed rows are retained without interpreting the suspect WAL.
If a valid WAL cannot be fully checkpointed because an uncoordinated SQLite
reader or writer is active, repair aborts before renaming any live file.

Standalone salvage scans `messages NOT INDEXED` and commits those authoritative
rows before rebuilding indexes and FTS. Titles, prompt references, and valid
content-addressed compaction projections are then copied in separate
transactions. A compaction is recovered only when its raw source range/digest,
summary hash, and referenced prompt blob all verify. Corruption in an optional
projection can therefore omit that projection with an explicit warning, but
cannot roll back readable conversation messages. A failure while scanning or
committing readable messages aborts recovery instead of installing an empty
replacement. Operational failures while copying, opening, configuring, or
inspecting the standalone source likewise fail the repair and leave quarantine
intact. An empty replacement is allowed only when SQLite conclusively rejects
that standalone main database or its authoritative `messages` schema is absent
or incompatible.

Every mutating attempt writes metadata-only `started` and
`completed`/`failed` records to `memory.db.repair.jsonl`. The log contains
actions, counts, paths, and errors, but no message or prompt bodies. Normal
memory handles keep a shared lifecycle lock, while repair requires the
exclusive lock, so replacement cannot race an active Claw writer. An
incomplete quarantine attempt blocks normal database startup. Replacement
files use an attempt-specific staging name and carry an internal completion
marker bound to the attempt and quarantined source. The marker and recovered
row counts must be visible in the standalone staged main database after a
checkpoint before installation, so a retry never accepts an unrelated empty
database or deletes sidecars that hold the only copy of recovered rows.

## Recovery Limit

Claw does not attempt page-level or byte-level salvage. For a malformed WAL it
recovers only rows already present in a separately validated copy of the main
database; it does not replay suspect WAL frames. If that standalone main copy
also fails SQLite integrity/schema validation, repair creates an empty, healthy
replacement and reports the recovery warning. Operators can retain or inspect
the quarantined evidence with external forensic tooling; Claw never deletes it
automatically.
