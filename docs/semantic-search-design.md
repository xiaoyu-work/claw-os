# Semantic Search Architecture

Claw OS currently has three related but distinct local-search surfaces. They
do not yet form one unified document-search pipeline.

| Surface | Implementation | Storage | Current consumer | Status |
| --- | --- | --- | --- | --- |
| Document keyword search | Recoll / Xapian | `~/.recoll/` | `apps/docs` | Shipped |
| Filesystem semantic prototype | `crates/claw-semantic` | JSON-backed `MemoryStore` | `claw-semantic` CLI only | Experimental |
| Agent semantic memory | `crates/claw-embed` + core memory adapter | SQLite `SemanticStore` | Agent indexing and `cos_recall_semantic` | Shipped when embedding is configured |

The Recoll and `claw-semantic` user units are enabled under
`default.target`, so they can run in headless user sessions. Each unit uses
`ConditionPathIsExecutable` and skips cleanly when its optional binary is not
installed.

## Document Search: Recoll

`apps/docs` currently uses Recoll only:

1. `recollindex` builds the Xapian index over configured top directories.
2. `recollq` executes keyword queries.
3. The App converts Recoll results into its structured response.

Recoll is strong for exact terms, names, numbers, and code tokens. Config and
index data live under `~/.recoll/`.

The current `apps/docs.search` operation does **not** query either semantic
store and does not perform result fusion.

## Filesystem Semantic Prototype: `claw-semantic`

`crates/claw-semantic` remains a Phase-1 prototype:

```text
watched files
  -> TextExtractor
  -> grapheme-safe overlapping chunks
  -> StubEmbedder (384 dimensions)
  -> MemoryStore persisted as JSON
  -> claw-semantic CLI queries
```

The daemon watches configured directories with notify/inotify. Its
`StubEmbedder` deterministically hashes content, so the pipeline can be
exercised end to end, but the resulting similarity scores are not meaningful
semantic embeddings.

This prototype is not connected to `apps/docs`.

## Agent Semantic Memory: `claw-embed`

The production semantic-memory path lives in `crates/claw-embed` and
`core/src/agent/memory/semantic.rs`:

```text
agent message or memory item
  -> configured embedding task
  -> SemanticStore (SQLite)
  -> cosine search
  -> cos_recall_semantic
```

The default local embedding task uses the bundled Qwen3-Embedding-0.6B model
through ONNX Runtime GenAI and emits 1024-dimensional vectors. Embedding
provider `auto` prefers the configured local task and can use an
OpenAI-compatible provider when configured.

`SemanticStore` records the embedding model identity with stored vectors and
rejects incompatible model changes instead of silently mixing vector spaces.
The default database is the Agent semantic store below the Claw OS data
directory.

This path provides real semantic recall for Agent memory, but it does not
index the user's document directories for `apps/docs`.

## Why the Paths Are Separate

- Recoll is an external document-indexing system with broad format support.
- `claw-semantic` is a standalone filesystem-watching prototype.
- `claw-embed` is a reusable embedding/storage library integrated with Agent
  memory.

Keeping these boundaries explicit prevents the prototype daemon from being
mistaken for the production Agent semantic store.

## Planned Document Fusion

The intended `apps/docs.search` design is still:

1. query Recoll for keyword matches,
2. query a real filesystem semantic index,
3. merge the ranked lists with Reciprocal Rank Fusion.

```text
score(doc) = sum over each source S of:
                1 / (k + rank_S(doc))
```

Before implementing fusion, the project must choose one filesystem semantic
path:

- upgrade `claw-semantic` to use the production embedding task and a durable
  store, or
- reuse `claw-embed` and retire the duplicate prototype storage pipeline.

Only after that decision should `apps/docs` gain semantic queries and RRF
fusion.

## Current Status

- `apps/docs`: Recoll keyword search only.
- `claw-semantic`: optional filesystem prototype with stub embeddings.
- Agent semantic recall: real embeddings and SQLite storage when configured.
- Document semantic fusion: not implemented.
