# Operation Previews Module

## Purpose

Provide one non-executing preview of authenticated App effect declarations for
terminal, Web, and native desktop consumers. A preview is not an execution
grant, a filesystem diff, or proof that an effect will occur.

## Key Files

| Path | Role |
| --- | --- |
| `mod.rs` | Verified manifest preview, bounded input binding, and requested target projection |
| `cli.rs` | Thin terminal client of the shared broker |
| `receipts.rs` | Bounded, redacted capture of App return values as reports, not verified effects |
| `../router/operation_commands.rs` | Explicit normal App execution and receipt-recording orchestration |
| `../caps/manifest/effects.rs` | Optional effect declaration vocabulary and validation |
| `../clawd/operation_previews.rs` | Authenticated general and owner-scoped Activity adapters |
| `../../test/unit/operations/` | Purity, binding, unknown-effect, and target-resolution coverage |

## Invariants

- Manifest bytes are read through the verified package snapshot before labels
  or effect declarations are used.
- Argument binding reuses the existing manifest grammar and literal defaults.
  No path context is supplied, so planning performs no filesystem
  canonicalization. Runtime selectors are not evaluated and do not read
  credentials.
- Targets remain requested values, not authorized final resources. Missing
  targets and runtime-selected arguments are explicit.
- Recovery is an App declaration, not a guarantee that a rollback record or
  usable inverse exists. Missing effect declarations mean unknown, not pure.
- Raw non-resource argument content is not included in the returned preview.
- The module starts no App, model, task, or approval and owns no second store.
  Normal execution still performs every existing check.

The router's explicit execution path captures results through `receipts.rs`
and records them in the shared Activity service. It preserves App errors and
indeterminate outcomes and never retries an operation to repair receipt
storage. See [execution receipts](../../../docs/execution-receipts.md).
Public file-change-plan replies are validated before projecting their real
unified diffs into the same receipt view. The original output digest is
retained, truncation is explicit, and App-reported plans are never promoted
to OS-confirmed effects. See [file change plans](../../../docs/file-change-plans.md).

## Tests

```bash
cargo test -p cos --lib -- operations:: operation_previews:: manifest::effects:: --test-threads=1
```
