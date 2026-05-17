# Semantic search: design

ClawOS ships two complementary search layers. Both are local-only,
both run as `systemd --user` daemons enabled by default, and both
back the single `apps/docs` AI surface.

| Layer       | Daemon                       | Storage                          | Strength                           | Weakness                                |
| ----------- | ---------------------------- | -------------------------------- | ---------------------------------- | --------------------------------------- |
| Keyword     | `claw-recoll-index.service`  | Xapian TF-IDF (`~/.recoll/`)     | Exact terms, names, numbers, code  | Doesn't understand intent or synonyms   |
| Semantic    | `claw-semantic.service`      | Vector store (`~/.local/state/`) | "Find my Sequoia pitch"            | Slower, heavier, fuzzy on exact strings |

Each can answer questions the other can't. `apps/docs.search`
queries both, merges with Reciprocal Rank Fusion (RRF), and returns
a single ranked list to the AI agent.

## Components — semantic side

```
                ┌────────────────────────────────────┐
                │ claw-semantic-daemon (Rust, sysd)  │
                │                                    │
  ~/Documents ──┤ Watcher (notify/inotify)           │
  ~/Desktop ────┤   │                                │
  ~/Downloads ──┤   ▼                                │
                │ Extractor: bytes → UTF-8 text      │
                │   (Phase 1: txt/md/source code)    │
                │   (Phase 2: + pdf/docx/html/rtf)   │
                │   │                                │
                │   ▼                                │
                │ Chunker: ~1024-char windows w/ 128 │
                │   char overlap, grapheme-safe      │
                │   │                                │
                │   ▼                                │
                │ Embedder                           │
                │   (Phase 1: stub SHA-256 → 384d)   │
                │   (Phase 2: fastembed-rs BGE-small)│
                │   │                                │
                │   ▼                                │
                │ Vector store                       │
                │   (Phase 1: JSON file in-mem +     │
                │     persist on every upsert)       │
                │   (Phase 2: LanceDB embedded)      │
                └────────────────────────────────────┘
                              │
                  /usr/bin/claw-semantic (CLI)
                              │
                              ▼
                   apps/docs/main.py
                   ├── docs.search ──► claw-semantic + recollq
                   ├── docs.status ──► claw-semantic + recoll DB stat
                   ├── docs.index ───► both (advisory; daemons run anyway)
                   └── docs.configure
```

## Why two crates, not one

* Recoll is a 22-year-old C++ project we don't want to fork; we just
  invoke `recollindex -m` and `recollq` as subprocesses.
* The semantic side is a single Rust binary because the embedding
  model and vector store are tightly coupled and we want the entire
  pipeline (watcher → embedder → store) to run in one process for
  efficiency.
* They live in separate systemd units so a failure on one side
  (e.g. recoll crash, or fastembed OOM) doesn't blast the other.

## Pluggable trait stack

The Rust daemon is built around three traits in `crates/claw-semantic`:

| Trait          | Phase 1               | Phase 2 target                                                 |
| -------------- | --------------------- | -------------------------------------------------------------- |
| `Extractor`    | `TextExtractor`       | `PdfExtractor`, `DocxExtractor`, `HtmlExtractor`, ...          |
| `Embedder`     | `StubEmbedder` (sha)  | `FastEmbedEmbedder` (BGE-small-en-v1.5, 384-dim, ~30 MB ONNX)  |
| `VectorStore`  | `MemoryStore` (JSON)  | `LanceStore` (LanceDB, on-disk, scales to >1M chunks)          |

Phase boundaries are commit boundaries: each phase swap is a single
file change in the daemon's `main` (`Arc::new(StubEmbedder)` →
`Arc::new(FastEmbedEmbedder::load("BGE-small-en-v1.5")?)`).

## Why not ollama?

`fastembed-rs` runs the embedding model inside the daemon process via
ONNX Runtime. No separate ollama daemon, no HTTP roundtrip per chunk,
no Python. The trade-off is we're committed to a specific runtime;
the upside is one fewer service to manage on every desktop.

## RRF fusion (Phase 4)

`apps/docs.search` will issue both queries in parallel, then merge
with the standard Reciprocal Rank Fusion formula:

```
score(doc) = sum over each source S of:
                1 / (k + rank_S(doc))    with k=60
```

`k=60` is the value used in the original Cormack/Clarke paper and is
robust across corpora. We don't normalise the raw scores (BM25 vs
cosine are on different scales) — RRF works directly on ranks.

## Status

* Phase 1 — scaffold (this commit). Daemon runs, walks topdirs,
  watches inotify, but uses a stub embedder so search hits are not
  yet meaningful. apps/docs is **not** wired to the daemon yet.
* Phase 2 — real embedder + LanceDB. Daemon produces real semantic
  results via `claw-semantic search QUERY`.
* Phase 3 — extractor expansion (pdf, docx, html, rtf).
* Phase 4 — `apps/docs.search` fuses recoll + claw-semantic with RRF
  and becomes the single AI surface.
