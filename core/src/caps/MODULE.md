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
| `manifest/objects.rs` | Optional App object types and self-contained resolver argument contracts |
| `manifest/effects.rs` | Optional App-declared effects; never authorization or observed outcomes |
| `../../test/unit/caps/manifest.rs` | Manifest parsing, need binding, AI/session/desktop tests |
| `enforcement.rs` | Permission decision path |
| `activity_boundary.rs` | Shared-service consumer for pinned Activity policy revisions, exact confirmation needs and non-serialized grant bindings |
| `mod.rs` | Shared capability types and exports |

## Dependencies

Capability definitions are stable inputs to `clawd`, app discovery, sessions,
and agent tools. Validation occurs before side effects. Consumers request the
narrowest scope and do not reinterpret scope strings independently.

Activity boundaries consume `ActivityService` and pin an execution's owner,
session and policy revision. They are constraints, not capabilities: deny
prevents consent escalation, and required confirmation precedes the standing
capability shortcut. Brokered App plans settle all confirmation at root;
their non-serialized grant binding is rechecked before effects. See
[`docs/activity-capability-policies.md`](../../../docs/activity-capability-policies.md).

Policy and confirmation checks reuse the same pure whole-scope matcher.
Executable Path checks additionally require ordinary symlink-aware
`Scope::covers` containment in that same scope, including a missing leaf
under an existing ancestor. A lexical in-scope alias cannot therefore reach
outside the Activity subtree. This runtime check does not add filesystem I/O
to policy validation or pure matching, or promise a filesystem transaction.

`caps/` is the *vocabulary*, not the authority. A `CapSet` describes what some
principal may do; it never establishes that they may do it. The thing that
decides is `clawd::authority`, which holds grants bound to an authenticated
process and hands each broker route one decision. A serialized `CapSet` found on
disk, in a request body, or in a session row is a description to be re-derived
and clamped — never promoted.

## Tests

```bash
cargo test -p cos caps:: -- --test-threads=1
```

Changes require containment/normalization tests plus an end-to-end consumer or
enforcement test.

A broker-authenticated, identity-matched non-App task scope checks its granted
capabilities without opening the extension runtime's protected write lock.
This does not create authority or skip capability containment. App and MCP
identities still require their provenance/liveness checks; a forged environment
session name cannot create a trusted task-local scope.
