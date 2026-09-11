# Agent Memory Module

## Purpose

`memory/` persists agent conversations and searchable knowledge, builds recall
results, and curates durable notes from completed work.

## Responsibilities

- Store sessions, messages, prompt injections, searchable text, and in-flight
  tool invocation state.
- Freeze content-addressed canonical system prompts per session.
- Provide FTS and semantic recall behind stable interfaces.
- Provide paged model reads with stable source IDs; the model chooses which
  sources and queries are relevant.
- Curate notes with crash-safe run bracketing, excluding injected/compacted data.
- Redact sensitive model-visible memory where required.
- Preserve schema, transaction, and recovery behavior.

## Key Files

| Path | Role |
| --- | --- |
| `sqlite_fts.rs` | SQLite/WAL/FTS persistence plus pending/completed tool invocations |
| `semantic.rs` | Vector/semantic recall integration |
| `curator.rs` | Automatic memory curation |
| `notes.rs` | Durable notes, pinned profile entries and model-invoked literal search |
| `history.rs` | Conversation history queries and versioned UTF-8-safe read windows |
| `app_memory.rs` | App-scoped memory definition |

## Dependencies

Runtime records through memory interfaces; tools query those interfaces rather
than opening concrete databases. Model-visible memory must be traced and
redacted consistently. Schema and recovery changes require migration/regression
coverage.

`USER.md` and `[always]` entries enter the bounded request profile, not the
frozen system prompt. Other notes are read deliberately through `cos_memory`.
`cos_memory search` performs the model's literal query across all notes or one
explicit name. Results disclose the requested/scanned names, bounded excerpts
and whether more matches remain. Empty results do not establish that no related
knowledge exists under another wording or source.

`cos_memory read`, `cos_recall show`, `cos_app_memory show` and
`cos_recall_semantic read` accept `offset`/`max_chars` (characters, default 4096,
maximum 16384). Nonzero offsets also require the returned `revision`; changed
content rejects the read and requires a restart. `next_offset` continues the
same source. `source_complete` is true only for a full first-page read, not a
suffix ending at EOF. Search/list snippets are bounded collectively as well as
per entry, and carry follow-up handles. Semantic timestamps describe indexing,
not event time; its original-source handle is a location, not proof.

The system prompt and tool descriptions require continued retrieval/refinement
when memory is incomplete, stale, off-topic or conflicting. The main model
selects those reads, including current authoritative App/OS observations.
Neither a search score nor a remembered command authorizes an action.
Injected packet data is recorded without the ordinary message-preview
truncation and is excluded from conversation replay and curation.

## Tests

```bash
cargo test -p cos agent::memory:: -- --test-threads=1
```
