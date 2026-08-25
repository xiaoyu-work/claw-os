# Capabilities Module

## Purpose

`caps/` defines the verbs, scopes, manifests, and enforcement decisions that
separate description from system authority.

## Responsibilities

- Maintain the capability catalog and risk metadata.
- Parse, normalize, and compare scopes.
- Validate app manifests and derive operation needs.
- Enforce permissions for sessions, tools, apps, and broker requests.

## Key Files

| Path | Role |
| --- | --- |
| `catalog.rs` | Known capability verbs and metadata |
| `scope.rs` | Scope kinds, normalization, containment |
| `manifest.rs` | `app.json` schema and validation |
| `manifest/tests.rs` | Manifest parsing, need binding, AI/session/desktop tests |
| `enforcement.rs` | Permission decision path |
| `mod.rs` | Shared capability types and exports |

## Dependencies

Capability definitions are stable inputs to `clawd`, app discovery, sessions,
and agent tools. Validation occurs before side effects. Consumers request the
narrowest scope and do not reinterpret scope strings independently.

## Tests

```bash
cargo test -p cos caps:: -- --test-threads=1
```

Changes require containment/normalization tests plus an end-to-end consumer or
enforcement test.
