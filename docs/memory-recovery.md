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

Diagnosis opens `memory.db` read-only. The FTS check builds a temporary
in-memory index from authoritative `messages` rows and compares token
instances; it does not rewrite the persisted index.

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
are not trusted.

Every mutating attempt writes metadata-only `started` and
`completed`/`failed` records to `memory.db.repair.jsonl`. The log contains
actions, counts, paths, and errors, but no message or prompt bodies. Normal
memory handles keep a shared lifecycle lock, while repair requires the
exclusive lock, so replacement cannot race an active Claw writer.

## Recovery Limit

Claw does not attempt page-level or byte-level salvage when SQLite cannot read
the database or when a WAL is malformed. In that case it preserves the entire
file family in quarantine and creates an empty, healthy replacement. Operators
can retain or inspect the quarantined evidence with external forensic tooling;
Claw never deletes it automatically.
