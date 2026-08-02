# Semantic search: design

Claw OS has two complementary local search layers. Both run as
`systemd --user` daemons enabled by default, and both are meant to back
the single `apps/docs` AI surface.

| Layer    | Daemon                      | Storage                      | Strength                          | Weakness                              |
| -------- | --------------------------- | ---------------------------- | --------------------------------- | ------------------------------------- |
| Keyword  | `claw-recoll-index.service` | Xapian TF-IDF (`~/.recoll/`) | Exact terms, names, numbers, code | Doesn't understand intent or synonyms |
| Semantic | `claw-semantic.service`     | Vector store (`~/.local/state/`) | "Find my Sequoia pitch"       | Slower, heavier, fuzzy on exact strings |

Both are globally enabled systemd user units under `default.target`, not
`graphical-session.target`, so indexing also runs for SSH/headless user
sessions. Each unit skips cleanly when its optional binary is absent.

## Components — semantic side

```
                ┌────────────────────────────────────┐
                │ claw-semantic daemon (Rust, sysd)  │
                │                                    │
  ~/Documents ──┤ Watcher (notify/inotify)           │
  ~/Desktop ────┤   │                                │
  ~/Downloads ──┤   ▼                                │
                │ Extractor: bytes → UTF-8 text      │
                │   │                                │
                │   ▼                                │
                │ Chunker: ~1024-char windows w/ 128 │
                │   char overlap, grapheme-safe      │
                │   │                                │
                │   ▼                                │
                │ Embedder → Vector store            │
                └────────────────────────────────────┘
                              │
                  /usr/local/bin/claw-semantic (CLI)
```

The crate (`crates/claw-semantic`) is built around three traits so any
stage can be swapped independently:

| Trait         | Current             |
| ------------- | ------------------- |
| `Extractor`   | `TextExtractor` (txt/md/source code) |
| `Embedder`    | `StubEmbedder` (deterministic-from-hash, 384-dim) |
| `VectorStore` | `MemoryStore` (vectors in memory, persisted as JSON) |

The `Embedder` is a placeholder that maps content to a 384-dim vector by
hashing, so the pipeline (watcher → extract → chunk → embed → store)
runs end-to-end but vector hits are **not yet semantically meaningful**.
A real embedding backend and an on-disk vector store are the next steps.

## Components — keyword side

`apps/docs` shells out to Recoll: `recollindex` builds the Xapian index
over the configured topdirs and `recollq` answers queries. Recoll
handles PDF, LibreOffice / MS Office formats, `.eml` mail, and more.
Config + index live under `~/.recoll/`.

## Why two crates, not one

* Recoll is a 22-year-old C++ project we don't fork; we just invoke
  `recollindex` and `recollq` as subprocesses.
* The semantic side is a single Rust binary because the embedder and
  vector store are tightly coupled and we want the whole pipeline
  (watcher → embedder → store) in one process.
* They live in separate systemd units so a failure on one side doesn't
  take down the other.

## RRF fusion (planned)

`apps/docs.search` is intended to query both layers in parallel and
merge with Reciprocal Rank Fusion:

```
score(doc) = sum over each source S of:
                1 / (k + rank_S(doc))    with k=60
```

`k=60` is the value from the original Cormack/Clarke paper and is robust
across corpora. RRF works directly on ranks, so BM25 and cosine scores
don't need normalising.

## Status

* `apps/docs` currently serves **keyword** results via Recoll.
* The semantic daemon runs and indexes, but uses a stub embedder, so its
  results are not yet meaningful and it is not yet fused into
  `apps/docs.search`.
* Next: a real embedder, an on-disk vector store, broader extractors
  (pdf/docx/html/rtf), and RRF fusion in `apps/docs.search`.
