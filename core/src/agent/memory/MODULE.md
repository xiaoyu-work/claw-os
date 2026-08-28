# Agent Memory Module

## Purpose

`memory/` persists agent conversations and searchable knowledge, builds recall
results, and curates durable notes from completed work.

## Responsibilities

- Store sessions, messages, prompt injections, and searchable text.
- Freeze content-addressed canonical system prompts per session.
- Provide FTS and semantic recall behind stable interfaces.
- Curate notes with crash-safe run bracketing.
- Redact sensitive model-visible memory where required.
- Preserve schema, transaction, and recovery behavior.
- Diagnose corruption and serialize explicit repair against active database
  handles without deleting the damaged evidence.

## Key Files

| Path | Role |
| --- | --- |
| `sqlite_fts.rs` | SQLite/WAL/FTS persistence |
| `recovery.rs` | Snapshot health checks, full WAL validation, FTS rebuild, repair logging, quarantine, and attempt-bound replacement |
| `semantic.rs` | Vector/semantic recall integration |
| `curator.rs` | Automatic memory curation |
| `notes.rs` | Durable note storage |
| `history.rs` | Conversation history queries |
| `app_memory.rs` | App-scoped memory definition |

## Dependencies

Runtime records through memory interfaces; tools query those interfaces rather
than opening concrete databases. Model-visible memory must be traced and
redacted consistently. Schema and recovery changes require migration/regression
coverage.

## Tests

```bash
cargo test -p cos agent::memory::recovery::tests -- --test-threads=1
cargo test -p cos agent::memory:: -- --test-threads=1
```

Operator commands and recovery limits are documented in
[`../../../../docs/memory-recovery.md`](../../../../docs/memory-recovery.md).
