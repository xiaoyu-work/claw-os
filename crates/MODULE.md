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
| `claw-embed/`, `claw-semantic/` | First-party embedding/search services |
| `cos-browser/`, `cos-mcp-serve/`, `cos-cli/` | First-party binaries/tools |
| `obscura-*/` | Vendored browser-engine internals |
| `../Cargo.toml` | Workspace membership and shared dependencies |

## Dependencies

Core depends on crate APIs; focused crates do not import core orchestration.
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
