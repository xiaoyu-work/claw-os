# Agent Memory Module

## Purpose

`memory/` persists agent conversations and searchable knowledge, builds recall
results, and curates durable notes from completed work.

## Responsibilities

- Store sessions, messages, prompt injections, searchable text, and in-flight
  tool invocation state.
- Atomically bind rows recorded by durable Agent tasks to their canonical task
  and outer user-message identities.
- Retain canonical rows while applying checked, whole-task replay exclusions.
- Freeze content-addressed canonical system prompts per session.
- Provide FTS and semantic recall behind stable interfaces.
- Provide paged model reads with stable source IDs; the model chooses which
  sources and queries are relevant.
- Curate notes with crash-safe run bracketing, excluding injected/compacted data.
- Redact sensitive model-visible memory where required.
- Preserve schema, transaction, and recovery behavior.
- Reset owner-learned notes, App memory and derived semantic rows only through
  the owner-scoped broker route; conversation rows, task bindings, compaction
  summaries, Jobs and audit evidence are retained.

## Key Files

| Path | Role |
| --- | --- |
| `sqlite_fts.rs` | SQLite/WAL/FTS persistence plus pending/completed tool invocations |
| `conversation_bindings.rs` | Durable task/message membership recorded with each task-owned row |
| `semantic.rs` | Vector/semantic recall integration |
| `curator.rs` | Automatic memory curation |
| `notes.rs` | Durable notes, pinned profile entries and model-invoked literal search |
| `history.rs` | Conversation history queries and versioned UTF-8-safe read windows |
| `conversations.rs` | Bounded owner-facing history plus atomic, provenance-preserving conversation snapshots |
| `app_memory.rs` | App-scoped memory definition |
| `inspection.rs` | Bounded owner-facing notes/App/interaction/index inspection and checked learned-memory edits; original evidence stays immutable |
| `access.rs` | Content-free references recovered only from successful built-in memory reads |

## Dependencies

Runtime records through memory interfaces; tools query those interfaces rather
than opening concrete databases. Model-visible memory must be traced and
redacted consistently. Schema and recovery changes require migration/regression
coverage.

Task bindings are canonical execution evidence, not presentation metadata.
Their rows intentionally survive ordinary message purge so missing transcript
rows cannot later be mistaken for a complete retained task. Revert records only
exclude rows from active replay; raw rows, bindings, Jobs and audit evidence
remain available to diagnostic and recovery paths. Model-visible exact and
semantic recall honor replay exclusions, while canonical diagnostic reads and
FTS projections retain the underlying evidence.

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
Owner memory management uses the same stores under the authenticated owner's
filesystem identity. Note replacement/deletion carries its exact revision;
App forgetting also removes its derived index entry and reports failures.
Conversation evidence and derived-index inspection are read-only. Corrections
to reported/derived data are separate owner notes, not forged App provenance.
UI mutations refuse active Agent tasks. Model workers still use guarded memory
tools; the human management routes confer no model authority.
Legacy `memory.history/sessions` reads share the broker's owner-identity,
read-only opener with canonical conversations. A missing database is an empty
initial state; unreadable, wrong-owner or invalid existing files remain
explicit errors. Reads never create a database, run migrations, transfer
ownership or discard conversation data.
Injected packet data is recorded without the ordinary message-preview
truncation and is excluded from conversation replay and curation.

## Tests

```bash
cargo test -p cos agent::memory:: -- --test-threads=1
```
