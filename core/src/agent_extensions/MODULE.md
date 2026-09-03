# Agent Extensions Module

## Purpose

`agent_extensions/` activates explicitly selected, authenticated out-of-process
observers without giving extension code prompt, credential, broker, or
authorization-policy access.

## Responsibilities

- Parse and validate the authenticated `extension.json` schema.
- Load selected ids only through `crate::provenance::verify`.
- Quarantine package/manifest failures with actionable diagnostics.
- Publish bounded least-privilege runtime observations through one ordered
  per-extension FIFO with a reserved terminal slot.
- Stop accepting new observations after repeated backpressure while draining
  every already accepted FIFO event before the ordered terminal marker.
  Security revocation and protocol compromise remain immediate discard paths.
- Track detach acknowledgement independently from the event worker task,
  retry every unacknowledged detach within the shared finish budget, and force
  supervisor-owned host/cgroup teardown when exact child termination cannot be
  proven.
- Keep transient detach failure per extension. A later exact detach
  acknowledgement or accepted host-containment escalation resolves only that
  extension's detach state; unrelated runtime/protocol failures and other
  extensions remain independent.
- Emit model observations at the real provider-attempt boundary with paired
  attempt ids.
- Mint per-extension event/session/package/tool/policy-bound opaque capability
  references under one monotonic deadline and consume result batches
  atomically.
- Route only explicitly cooperative, default-deny proposed tools through exact
  input-derived capability enforcement, approvals, provider enforcement, and
  audit.
- Never inject extension output into canonical prompts or conversation
  history.

## Key Files

| Path | Role |
| --- | --- |
| `manifest.rs` | Identity/version/content binding, subscriptions, requested capabilities, action policies, protocol/features, limits |
| `registry.rs` | Explicit installed-package selection and activation quarantine |
| `capability_ref.rs` | Per-extension 256-bit one-use reference leases and atomic result consumption |
| `runtime.rs` | Attempt/tool observation, ordered backpressure/lifecycle, and default-deny action mediation |
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
