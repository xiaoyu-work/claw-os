# Agent Skills Module

## Purpose

`skills/` discovers, validates, syncs, and exposes instruction packages that
extend the agent without adding privileged code.

## Responsibilities

- Parse skill manifests and instruction files.
- Resolve installed/user skill roots.
- Sync trusted skill sources and track usage.
- Select skill content for prompt construction.

## Key Files

| Path | Role |
| --- | --- |
| `manifest.rs` | Skill manifest validation |
| `sync.rs` | Source synchronization |
| `usage.rs` | Skill usage records/statistics |
| `mod.rs` | Discovery and selection |

## Dependencies

Skills contribute instructions, not authority. Tool access still comes from the
guarded registry and capabilities. Synced content is external/untrusted input
until validated and wrapped for prompt use.

## Tests

```bash
cargo test -p cos agent::skills:: -- --test-threads=1
```
