# Agent Memory Module

## Purpose

`memory/` persists agent conversations and searchable knowledge, builds recall
results, and curates durable notes from completed work.

## Responsibilities

- Store sessions, messages, prompt injections, and searchable text.
- Provide FTS and semantic recall behind stable interfaces.
- Curate notes with crash-safe run bracketing.
- Redact sensitive model-visible memory where required.
- Preserve schema, transaction, and recovery behavior.

## Key Files

| Path | Role |
| --- | --- |
| `sqlite_fts.rs` | SQLite/WAL/FTS persistence |
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
cargo test -p cos agent::memory:: -- --test-threads=1
```
