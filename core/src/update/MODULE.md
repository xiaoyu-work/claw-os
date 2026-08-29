# Update Freshness Module

## Purpose

`update/` is the single place that answers "is this Claw OS release still the
current one?". Signatures answer authenticity; nothing in a signature expires,
so an artifact that was validly signed years ago is still validly signed today.
This module is the freshness authority that keeps a superseded release from
being installed, activated or run.

## Responsibilities

- Define and validate the signed `claw.release-security/v1` manifest and its
  canonical encoding.
- Order Debian versions exactly as `dpkg` does, including epochs and `~`.
- Own the durable, root-owned monotonic security floor and its hash-chained
  generation history.
- Publish the unprivileged runtime projection every other Claw OS binary
  enforces against, and keep it in step with the authority.
- Verify release manifests against the OpenPGP trust the machine already has,
  without generating, shipping or inventing key material.
- Decide one refusal policy that every enforcement point shares.
- Record one-use, narrowly scoped operator recovery authorizations.
- Gate `clawd` startup, agent-worker spawn, and every Claw OS binary.

## Key Files

| Path | Role |
| --- | --- |
| `canonical.rs` | Deterministic JSON used for signing, hashing and chaining |
| `debver.rs` | `dpkg --compare-versions` ordering and validation |
| `manifest.rs` | `claw.release-security/v1` parsing and structural validation |
| `signature.rs` | Detached OpenPGP verification via `gpgv`, keyring discovery |
| `floor.rs` | Floor state, atomic durable commit, rollback detection |
| `projection.rs` | The unprivileged, root-owned runtime view of that floor |
| `recovery.rs` | One-use authorizations: scope, expiry, atomic consumption |
| `decide.rs` | The shared refusal policy and its stable decision classes |
| `journal.rs` | Auditable record of every decision |
| `runtime.rs` | Startup and worker-spawn gates |
| `cli.rs` | `claw-security-floor`, called by maintainer scripts and the APT hook |

## Dependencies

Depends only on `crypto` (SHA-256), `provenance::fsec` (ownership/mode gating
and `openat` reads), `debver` and `chrono`. It must not depend on `agent`,
`apps` or the broker routes: `clawd`, `cos` and the maintainer scripts are
consumers. The state path is a compiled-in absolute path — never derived from
the environment — so no caller can point enforcement at state it controls.

`packaging/release-security/policy.json` mirrors the epoch, ABI, protocol and
component constants here; a unit test fails when they diverge.

## Threat Boundary

In scope: an unprivileged local attacker, a stale or hostile mirror, a
preserved repository snapshot, `apt install <pkg>=<old>`, a reordered or
partial transaction, and a component binary replaced behind dpkg's back.

Out of scope: local root, and replacement of the complete filesystem *and*
state together. Both can rewrite the floor alongside the binaries. Detecting
that requires a TPM measurement or a remote attestation anchor, which Claw OS
does not have; this is not hardware anti-rollback.

## Tests

```bash
cargo test -p cos --lib update:: -- --test-threads=1
cargo test -p cos --test security_floor_process -- --test-threads=1
bash packaging/deb/tests/test-security-floor-packaging.sh
bash packaging/deb/tests/test-security-floor-install.sh
bash packaging/apt-repo/tests/test-release-security-publication.sh
```

`test/unit/update/` covers encoding, ordering, manifest validation, floor
state, the runtime projection, decision policy, recovery scope and the CLI
surface. `core/tests/security_floor_process.rs` cross-checks version ordering
against the real `dpkg` and drives the compiled helper against a real
filesystem. The shell suites build real `.deb` archives, run real
`dpkg`/`apt-get` transactions, and sign with an ephemeral key that never leaves
the test run.
