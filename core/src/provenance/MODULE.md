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
| `install.rs` | Bounded staging, hostile-tree rejection, atomic publish, read-only retained verification and non-App rollback |
| `fsec.rs` | Ownership/mode gating and TOCTOU-resistant `openat` reads |
| `state.rs` | Durable per-domain generation/fingerprint so daemons notice revocations cheaply |
| `ceiling.rs` | The capability ceiling each trust tier implies |
| `runtime.rs` | Which package and which exact process a running session is, immediate denial on use, and the bounded lifecycle pass that stops it |
| `consent.rs` | Interactive, phrase-matched human approval for unsigned code |
| `cli.rs` | Provenance command mechanics and read-only App rollback selection; public App rollback orchestration lives in the router |
| `withdrawal.rs` | Pure bounded withdrawal selectors and fresh owner-private denial-only policy snapshots; never a publisher authority |

## Dependencies

Depends on `crypto` (canonical/envelope SHA-256), the already-declared `sha2`
backend (file-body SHA-256), `ed25519-dalek`, `paths` and
`audit`. It must not depend on `apps`, `agent` or `clawd`: those are
consumers. Trust roots are compiled-in absolute paths plus the passwd
home of the effective uid — never environment-derived, so no caller can
widen trust by setting a variable.

File-body hashing uses the existing CPU-dispatched SHA-256 implementation.
Every fresh verification still reads and hashes the entire selected package,
checks its signature and exact inventory, and rejects permission/type changes.
No hash/stat cache or cached authorization verdict is introduced.

The workspace development profile optimizes the existing `sha2` package, also
inherited by test builds. This avoids unoptimized full-payload hashing without
changing dependencies, CPU dispatch, debug checks or fresh verification.
Release compilation remains unchanged.

App rollback is a consumer-owned lifecycle operation, not local provenance
publication. The public router uses the read-only, signature-only retained
artifact resolver and the existing signed Root directory controller. Direct
`install::rollback` and `cli::run` App publication are refused. Non-App rollback
keeps its existing staging/verification/publication behavior; no App/broker
dependency is introduced here.

Trust withdrawal writes only the effective UID's private policy, before any
Root request. The router then requests checked Root reconciliation; direct
provenance CLI calls report retirement unconfirmed. Provenance never imports
the App broker. Generic runtime termination refuses App records, retaining
them for Root custody rather than treating an unreadable process as absence.
Developer liveness requires the retained supporting path, kind, ID and digest.
Missing consumer-private keys/grants do not alter Root's positive verifier.
Owner-policy readers bound file bytes and directory entries and use pinned,
no-follow, nonblocking descriptors so a replaced FIFO cannot stall Root.

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
