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
| `cos-browser/`, `cos-mcp-serve/`, `cos-cli/` | First-party binaries/tools |
| `obscura-*/` | Vendored browser-engine internals |
| `../Cargo.toml` | Workspace membership and shared dependencies |

## Dependencies

Core and `claw-semantic` depend on `claw-embed`; the primitives crate does not
import daemon or core orchestration. Core depends on crate APIs; focused crates
do not import core orchestration.
`claw-embed::SemanticStore::get` reads a namespace/key without an embedding call.
Its model-backed search checks model identity, dimensions and vector integrity
before returning similarity-ranked candidates; raw precomputed-vector queries
retain the caller-managed vector-space contract.
Desktop broker consumers depend on `clawd-client`; the client contains no
desktop UI or privileged broker implementation.
Its typed command inventory must be updated with every desktop-consumed broker
route, including Activity object-state list/record; core route additions alone
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
