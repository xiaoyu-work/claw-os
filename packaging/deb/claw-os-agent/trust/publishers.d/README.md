# Vendor publisher trust root

Files placed here are installed to `/usr/lib/cos/trust/publishers.d/`
and become the **vendor** trust tier for extension packages (Apps,
Skills and MCP/adapter packages).

## Explicitly approved release publishers

`clawos-app.json` contains the owner-approved public key for official App
releases. Its private half is held outside source control and in the App
repository's dedicated signing Secret. It is restricted to package signing
for `app` packages; it cannot authenticate Skills, MCP packages or Agent
extensions. The existing GPG APT archive key is separate and unchanged.

Publisher authentication verifies package bytes, not runtime permission.
Every installed App still requires the ordinary protected review, Host,
capability and sandbox gates. There is no business-name entitlement.

* **Built-in content still works.** Apps and Skills the Debian package
  installs under `/usr/lib/cos` are trusted through *package-manager
  (vendor) trust* — root owns every path component and no unprivileged
  user can modify them — not through a publisher key. Their content
  digest is pinned on first use so post-install tampering is detected.
* **Nothing third-party is trusted by default.** A signed package from
  a hub or any other publisher stays **quarantined** until an operator
  deliberately installs that publisher's public key. `cos app`, the
  skill loader and MCP discovery all report the package with its reason
  instead of running it.
* **No fallback publisher is selected.** The packaging contract checks
  that every supplied key is a real, structurally valid public entry.
  Unknown signatures remain quarantined rather than falling back to
  package-manager trust.

Shipping an unknown key would be strictly worse than shipping none: it
would create a trusted signer nobody controls.

## Installing a publisher key

Trust is a human decision, taken out of band:

```bash
# Operator-wide (root): drop the entry into the system root.
sudo install -m 0644 publisher.json /etc/cos/trust/publishers.d/

# Or for one user only:
cos provenance trust add --file publisher.json
```

Both paths take a `claw.trust/v1` document containing **public** key
material only. `packaging/deb/build-debs.sh` refuses to build if a file
in this directory contains a `private_key` field.

## Rules for a file in this directory

- One `claw.trust/v1` document per file, public material only.
- Installed mode `0644`, directories `0755`, owner `root`. The loader
  refuses any trust root whose path — or any ancestor up to `/` — is a
  symlink, is owned by a non-root user, or is group- or world-writable.
- A key id uses the canonical `key_id_for` digest: SHA-256 over
  `claw-provenance/v1\0publisher-key\0` followed by the raw 32-byte
  Ed25519 verifying key. The loader recomputes it and rejects an entry whose declared id does
  not bind to its key material, so an id can never be aliased onto a
  different publisher's key.

## Shape

```json
{
  "schema": "claw.trust/v1",
  "keys": [
    {
      "key_id": "sha256:<64 hex>",
      "algorithm": "ed25519",
      "public_key": "<64 hex>",
      "usages": ["package-signing"],
      "kinds": ["app", "skill", "mcp"],
      "status": "active",
      "not_before": null,
      "not_after": null,
      "comment": "Example Publisher, rotated 2026-01"
    }
  ],
  "revoked_keys": [],
  "revoked_packages": []
}
```

Generate a key pair with:

```bash
cos provenance keygen --out ~/.secrets/publisher.json --comment "example 2026"
```

and publish only the printed `trust_entry` object. **Never commit the
private key file.**

## Validity windows

`not_before` and `not_after` are strict RFC 3339 timestamps, normalised
to UTC before comparison. A malformed, ambiguous or out-of-range value
rejects the whole entry — a key whose expiry cannot be understood must
not authorise anything.

## Rotation and revocation

Rotation is "publish the successor, then bound the predecessor":

1. Add the new key with `"status": "active"`.
2. Set `"not_after"` on the old key so it stops authorising new
   verifications after that instant.
3. When the old key must stop working immediately, set
   `"status": "revoked"` or list its id under `revoked_keys`.

A single compromised artifact is revoked by content digest through
`revoked_packages`, which stops launches, disclosures and attachments
for that exact package without invalidating the publisher.

Any change must be followed by a `cos provenance trust` command (or, for
the system domain, a re-run of the packaging step) so the domain's
durable generation in `state.json` is re-recorded. Long-lived daemons
compare that generation to decide when to reload; a trust file edited
without re-recording fails the domain closed rather than being honoured.
