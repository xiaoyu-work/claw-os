# Workspace Crates Module

## Purpose

`crates/` contains focused Rust packages consumed by the core, SDK, browser,
CLI, semantic, and embedding surfaces.

## Responsibilities

- Isolate reusable binaries/libraries from the core dispatcher.
- Maintain explicit Cargo package boundaries.
- Preserve vendored Obscura internals separately from first-party Claw crates.

## Key Files

| Path | Role |
| --- | --- |
| `claw-embed/` | Reusable embedding, extraction, chunking, walking, and storage contracts |
| `claw-semantic/` | Filesystem semantic daemon, config, service orchestration, and CLI |
| `clawd-client/` | Unprivileged typed broker discovery, framing, envelopes, deadlines, and errors |
| `cos-browser/`, `cos-cli/` | First-party binaries/tools |
| `obscura-*/` | Vendored browser-engine internals |
| `../Cargo.toml` | Workspace membership and shared dependencies |

## Dependencies

Core and `claw-semantic` depend on `claw-embed`; the primitives crate does not
import daemon or core orchestration. Core depends on crate APIs; focused crates
do not import core orchestration.
`claw-embed::SemanticStore::get` reads a namespace/key without an embedding call.
`SemanticStore::open_read_only` permits owner inspection without creating a
database, migrating schema or constructing an embedder.
Its model-backed search checks model identity, dimensions and vector integrity
before returning similarity-ranked candidates; raw precomputed-vector queries
retain the caller-managed vector-space contract.

Personal-file indexing uses `claw-semantic/src/indexer.rs` and the same SQLite
`SemanticStore`, not the historical `StubEmbedder` JSON corpus. The versioned
`claw-embed::local` pipe delegates fixed Qwen3 CPU inference to `cos`; these
leaf crates do not import core or select a cloud provider. The shared
`claw-embed::file_source` helpers pin every path component without symlinks.
Callers still supply owner identity and sharing policy.

Each complete file source is replaced atomically after all embeddings exist
and its held/live file identity is unchanged. Keys bind chunks to a SHA-256
file revision; indexed text remains a snapshot, not live-file evidence.
Scoped search filters namespace/key eligibility before inspecting private
rows, ranking or limits. Workspace readers additionally recheck current tags,
permissions and source bytes at the broker boundary.

The existing daemon watches configured personal directories and reconciles
deleted, excluded and removed-root sources. Protected credentials/OS state
cannot be enabled by removing user exclusion patterns. Only supported UTF-8
text is extracted. Effective file chunks are capped at 448 characters for the
2048-token local inference ceiling; status reports the requested and effective
sizes. Missing models, runtime, unsupported sources and watcher errors are
explicit; there is no cloud or fake-vector fallback.

Desktop broker consumers depend on `clawd-client`; the client contains no
desktop UI or privileged broker implementation.
System-review commands authenticate the connected Unix peer as root before
sending a request. A socket override or an approved-looking JSON response from
an ordinary user process cannot substitute for OS confirmation.
Its typed command inventory must be updated with every desktop-consumed broker
route, including Activity object-state and policy controls; core route additions alone
do not update this independent client.
Keep Obscura changes scoped and preserve upstream licensing/provenance. Add a
new crate only for a coherent reusable responsibility.

## Tests

Each Rust crate stores private-access unit-test bodies under its own
`test/unit/` tree, mirroring `src/`. Public integration tests may still use the
Cargo-standard `tests/` directory.

```bash
cargo test -p <package-name>
```

Use the package's own test suite before a workspace-wide run.
