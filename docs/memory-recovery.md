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
rebuilds FTS from `messages`, and removes only orphaned title or prompt
projection rows after checking authoritative references in the same
transaction.

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
