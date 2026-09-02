# Agent Extensions Module

## Purpose

`agent_extensions/` activates explicitly selected, authenticated out-of-process
observers without giving extension code prompt, credential, broker, or
authorization-policy access.

## Responsibilities

- Parse and validate the authenticated `extension.json` schema.
- Load selected ids only through `crate::provenance::verify`.
- Quarantine package/manifest failures with actionable diagnostics.
- Publish bounded least-privilege runtime observations through per-extension
  queues.
- Mint event/session/package-bound opaque capability references.
- Route proposed actions through `ToolRegistry`, an attenuating capability
  ceiling, approvals, exact provider enforcement, and audit.
- Never inject extension output into canonical prompts or conversation
  history.

## Key Files

| Path | Role |
| --- | --- |
| `manifest.rs` | Identity/version/content binding, subscriptions, requested capabilities, protocol/features, limits |
| `registry.rs` | Explicit installed-package selection and activation quarantine |
| `capability_ref.rs` | 256-bit one-use capability-reference store |
| `runtime.rs` | Observation hook, per-extension backpressure, lifecycle, and action mediation |
| `../extension_host/abi.rs` | Framed child protocol |
| `../extension_host/agent_extension.rs` | Host-side process and cleanup |
| `../provenance/` | Shared `VerifiedPackage` authentication and pinned package snapshots |

## Tests

```bash
cargo test -p cos agent_extensions -- --test-threads=1
cargo test -p cos extension_host::abi::tests -- --test-threads=1
cargo test -p cos --test extension_provenance_process -- --test-threads=1
cargo test -p cos --test extension_host_boundary -- --test-threads=1
```

See [`../../../docs/extension-abi.md`](../../../docs/extension-abi.md) and
[`../../../docs/extension-provenance.md`](../../../docs/extension-provenance.md).
