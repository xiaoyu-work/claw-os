# Extension package provenance

Claw OS authenticates executable Agent extension packages before it trusts a
manifest, event subscription, capability request, entrypoint, or any other
package-supplied metadata. Local installation and root ownership are necessary
but not sufficient.

## Installed layout

The package-owned root is `/usr/lib/cos/extensions`. Each immediate child is
one package whose directory name matches its signed package id:

```text
/usr/lib/cos/extensions/example-observer/
├── provenance.json
├── extension.json
└── bin/
    └── observer
```

The current implementation intentionally provides an authenticated registry,
not a general installer. A package manager installs the complete directory as
root, with no group/world-writable ancestor or object. Activation is a separate
explicit user decision through `agent.extensions` in
`~/.config/cos/config.json`; installing a package never starts it.

`COS_AGENT_EXTENSIONS_DIR` may select another package root for development and
tests. It changes only the location: signature, inventory, ownership, type,
size, and manifest checks remain mandatory. It cannot add a trust root or
disable verification.

## Signed inventory

`provenance.json` is a closed schema:

```json
{
  "schema_version": 1,
  "kind": "agent-extension",
  "publisher": "claw-os",
  "key_id": "release-1",
  "package_id": "example-observer",
  "package_version": "1.0.0",
  "package_digest": "<64 lowercase hex characters>",
  "files": [
    {
      "path": "bin/observer",
      "sha256": "<64 lowercase hex characters>",
      "size": 12345,
      "executable": true
    },
    {
      "path": "extension.json",
      "sha256": "<64 lowercase hex characters>",
      "size": 900,
      "executable": false
    }
  ],
  "signature": "<128 lowercase hex characters>"
}
```

The inventory is strictly sorted and complete. `provenance.json` signs, but
does not list, itself. Packages are limited to 128 files and 1 MiB of signed
content. Absolute paths, dot/traversal components, symlinks, special files,
mount substitution, duplicate paths, unsafe ownership/modes, digest drift, and
changes during a read all fail verification.

The package digest is SHA-256 over the canonical, length-prefixed inventory.
The Ed25519 signature covers the versioned domain, package identity, package
digest, and every file record. The exact canonical encoding is
[`provenance::signing_input`](../core/src/provenance.rs); publisher tooling must
call that source of truth rather than reproduce JSON ordering.

Release trust roots are compiled into `cos` and `claw-extension-host`.
Environment, configuration, manifests, model input, and broker requests cannot
add a key. Debug builds also compile a publisher named `claw-os-test` for
hostile process tests; release builds omit it.

## Immutable verification snapshot

[`provenance::verify`](../core/src/provenance.rs) opens every object without
following links, rechecks identity and timestamps after reading, verifies the
complete signed inventory, and returns a `VerifiedPackage`. Consumers parse
only bytes held by that object. They never reopen the mutable install path.

The worker transports a serialized `PackageSnapshot` to the task's
`claw-extension-host`. The host independently calls
`provenance::verify_snapshot` with its own compiled trust roots before
materializing any byte in task-private storage. A compromised worker therefore
cannot substitute an unsigned entrypoint.

Verification or manifest failure puts that selected id in activation
quarantine: it is excluded from the registry and an actionable diagnostic is
logged. There is no fallback to unverified content and one bad package does not
hide another selected package.

## Change contract

Any new extension kind, discovery root, or install path must:

1. Call `crate::provenance::verify`.
2. Consume only the returned `VerifiedPackage` or a reverified
   `PackageSnapshot`.
3. Keep trust roots compiled in.
4. Quarantine failures with an actionable diagnostic.
5. Add adversarial unit and process coverage for inventory drift, link/path
   substitution, signatures, trust roots, and mutable-source races.
