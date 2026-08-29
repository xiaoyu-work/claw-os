# Credential Module

## Purpose

`core/src/credential/` owns encrypted credential persistence and the typed
operations used by CLI, OAuth, brokers, schedulers, and provider setup.
`mod.rs` is a compatibility facade; consumers must not import storage or key
material internals.

## Responsibilities

| Path | Ownership and invariants |
| --- | --- |
| `domain.rs` | Validated namespace/credential identifiers, persistent record shapes, and the credential-store interface consumed by OAuth |
| `crypto.rs` | Byte-compatible SHA-256 and AES-256-GCM primitives, strict base64, constant-time tag checks, and legacy XOR reads; no format or algorithm changes without vectors |
| `master_key.rs`, `keyring.rs` | OS randomness and `credential-root.key`, plus an isolated Linux session-keyring cache; key source order and `cos-credential-key` label are compatibility contracts |
| `authorization.rs` | Secret capability scopes, tier ordering, metadata/path binding, expiry checks, and fail-closed session-tier resolution |
| `store.rs` | Credential and bundle paths, encrypted record persistence, per-record locking, atomic replacement, scheduler owner/path checks, and public read compatibility |
| `lifecycle.rs` | Refresh-command execution policy and TTL preservation |
| `oauth.rs`, `oauth_login.rs` | Google and Microsoft transports and token lifecycle through `CredentialStore`; request bodies stay on stdin and never enter process argv |
| `cli.rs` | Stable command/flag parsing and JSON or fd presentation; it coordinates typed operations and never receives encryption keys |
| `error.rs` | `CredentialError` categories plus operation/source context and secret-safe formatting |

## Persistent Compatibility

- Credential JSON keeps `value_b64`, optional 12-byte `nonce_b64`,
  `min_tier`, timestamps, owner session, expiry, and refresh command.
- AES-256-GCM stores `ciphertext || 16-byte tag`, uses an empty AAD, and
  obtains a unique 12-byte nonce from the OS CSPRNG.
- The master-key source order is Linux session keyring, SHA-256 of
  `/etc/machine-id`, then the 32-byte persistent random root key.
- Root keys and credential temporary files are created as `0600`; writes use
  write-lock, exclusive temporary creation, fsync, rename, and parent fsync.
- Refresh locks are distinct from write locks. Revoke takes them in
  refresh-then-write order.
- Scheduled reads reject symlinks, non-regular files, wrong ownership, home
  escapes, and records larger than 1 MiB.
- Randomness, keyring syscalls, root-key persistence, credential atomic writes,
  lock acquisition, rename, file fsync, and parent-directory fsync return
  `CredentialError` with an operation and source. The optional Linux keyring is
  an explicit cache: a typed cache failure is logged before durable key sources
  are tried.

## Error Boundaries

- `CredentialStore`, command handlers, `run_typed`, and `try_load_typed` are
  typed ownership boundaries. Reads, parsing, crypto, randomness, persistence,
  and locking remain `CredentialResult` end-to-end; provider composition
  consumes `try_load_typed`.
- `run` and `try_load` retain their historical `Result<_, String>` signatures
  for Rust/CLI compatibility and stringify exactly once.
- OAuth transport remains an explicit external adapter and maps its
  network/protocol strings to `CredentialErrorKind::External` at command
  dispatch. It does not own randomness, key material, files, locks, or keyring
  state.

## Change Together

- Change `domain.rs`, `authorization.rs`, and capability/audit tests together
  when an identifier, scope, or tier rule changes.
- Change `crypto.rs`, `master_key.rs`, fixtures, and vectors together for any
  persistent cryptographic change.
- Change `store.rs` and locking/atomicity/permissions/symlink tests together.
- Change OAuth provider behavior with both OAuth modules and provider setup
  consumers; OAuth must continue to depend only on `CredentialStore`.
- Change `cli.rs`, CLI help/catalog, and command-output tests together.

## Tests

Credential unit tests live under `core/test/unit/credential/`. Run combined
tests serially because they share process-global environment variables:

```bash
cargo test -p cos credential:: -- --test-threads=1
```
