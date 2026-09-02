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

Every skill package is authenticated by `crate::provenance` before it is
loaded: install requires a valid signature from a trusted, non-revoked
publisher key, and there is no environment variable that relaxes this.
Layered shadowing compares the verified publisher key id, not directory
precedence. The catalog, the disclosed `SKILL.md` body and every child
resource are read from the verified snapshot and re-checked against
their signed digests at disclosure time, so a file changed after the
catalog was built fails the disclosure instead of injecting new text
into the model.

## Tests

```bash
cargo test -p cos agent::skills:: -- --test-threads=1
```
