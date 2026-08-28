# Extension Provenance Module

## Purpose

`provenance/` is the single authentication gate every extension package
passes through before its manifest, operations, capability needs, tool
schemas or model-visible metadata are trusted.

## Responsibilities

- Define and validate the versioned `claw.provenance/v1` envelope and
  its deterministic canonical signing bytes.
- Load publisher trust roots with ownership/mode gating, enforce key
  usage constraints, rotation and revocation.
- Verify a package's signature and its complete file tree, returning a
  snapshot bound to an open directory descriptor.
- Stage, verify and atomically publish untrusted bundles, retaining
  content-addressed artifacts for verified rollback.
- Expose the operator/developer CLI (`cos provenance …`).

## Key Files

| Path | Role |
| --- | --- |
| `envelope.rs` | `claw.provenance/v1` format, canonical bytes, strict parsing |
| `trust.rs` | Trust roots, key ids, usage/kind constraints, revocation, developer grants |
| `verify.rs` | Signature + tree verification, `VerifiedPackage` snapshots, vendor pins, cache |
| `sign.rs` | Publisher signing key handling and envelope construction |
| `install.rs` | Bounded staging, hostile-tree rejection, atomic publish, rollback |
| `fsec.rs` | Ownership/mode gating and TOCTOU-resistant `openat` reads |
| `state.rs` | Durable per-domain generation/fingerprint so daemons notice revocations cheaply |
| `ceiling.rs` | The capability ceiling each trust tier implies |
| `runtime.rs` | Which package and which exact process a running session is, immediate denial on use, and the bounded lifecycle pass that stops it |
| `consent.rs` | Interactive, phrase-matched human approval for unsigned code |
| `cli.rs` | `cos provenance keygen/sign/verify/trust/dev-trust/artifacts/rollback` |

## Dependencies

Depends only on `crypto` (SHA-256), `ed25519-dalek`, `paths` and
`audit`. It must not depend on `apps`, `agent` or `clawd`: those are
consumers. Trust roots are compiled-in absolute paths plus the passwd
home of the effective uid — never environment-derived, so no caller can
widen trust by setting a variable.

## Tests

```bash
cargo test -p cos --lib provenance:: -- --test-threads=1
cargo test -p cos --test extension_provenance_process -- --test-threads=1
```

`test/unit/provenance/` covers format, trust policy, verification,
install bounds and CLI argument handling.
`core/tests/extension_provenance_process.rs` drives the public API
against a real filesystem: real Ed25519 signatures, real renames,
TOCTOU replacement, concurrent update/verify, hostile tree shapes and a
spawned process.
