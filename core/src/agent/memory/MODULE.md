# Agent Memory Module

## Purpose

`memory/` persists agent conversations and searchable knowledge, builds recall
results, and curates durable notes from completed work.

## Responsibilities

- Store sessions, messages, prompt injections, and searchable text.
- Freeze content-addressed canonical system prompts per session.
- Provide FTS and semantic recall behind stable interfaces.
- Curate canonical, append-only facts with provenance and crash-safe run
  bracketing.
- Serialize the curator's final reread, dedupe, and append so concurrent
  writers cannot discard intervening note changes.
- Classify durable knowledge, expiring observations, session state, and
  procedure/Skill candidates before persistence.
- Project only current non-expired, non-conflicting fact tails into prompts
  while preserving the complete human-readable `MEMORY.md` history.
- Redact sensitive model-visible memory where required.
- Preserve schema, transaction, and recovery behavior.

## Key Files

| Path | Role |
| --- | --- |
| `sqlite_fts.rs` | SQLite/WAL/FTS persistence |
| `semantic.rs` | Vector/semantic recall integration |
| `curator.rs` | Automatic memory curation |
| `ontology.rs` | Canonical aliases, lifetime policy, and TTL bounds |
| `notes.rs` | Durable note storage and current-fact prompt projection |
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
