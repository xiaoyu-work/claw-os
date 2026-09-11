# Objects Module

## Purpose

Expose App-owned object declarations through a shared, non-executing catalogue.
References are portable data locators, never authority or cached object data.

## Key Files

| Path | Role |
| --- | --- |
| `mod.rs` | `ObjectCatalog` definition, verified installed-App provider, and invocation descriptions |
| `../caps/manifest/objects.rs` | Optional manifest object types and resolver validation |
| `../router/object_commands.rs` | Terminal discovery/reference formatting and ordinary App resolution |
| `../clawd/activity_objects.rs` | Owner-scoped Activity attachment and description |
| `../../test/unit/objects/` | Provenance, identity, and non-execution regressions |

## Dependencies and Invariants

Core consumes the public SDK's `ObjectRef` and pure URI helpers; it does not
define a competing parser for terminal, Web, or desktop.
`InstalledObjectCatalog` consumes the existing authenticated App discovery
provider. A verified snapshot supplies every label and resolver; preparation
retains that same `App` until the ordinary execution path rechecks it.
Neither catalogue inspection nor Activity attachment executes an App.

The resolver has one explicit ID argument and an optional revision flag.
Arguments are passed as argv values, with a delimiter for positional IDs.
Path-backed IDs must be absolute. Missing revision support is an error, not a
fallback to another version.

Activity attachment updates the existing resource list atomically. Metadata
descriptions are computed from current authenticated declarations and do not
duplicate Activity lifecycle, job state, or App data. `declared` authenticates
metadata only; it does not establish object existence, access, or purity.

See [the App object contract](../../../docs/app-objects.md).

## Tests

```bash
cargo test -p cos --lib object -- --test-threads=1
```
