# Packaging Module

## Purpose

`packaging/` turns compiled binaries and source-tree assets into installable
Debian packages and a signed multi-architecture APT repository.

## Responsibilities

- Assemble and publish `claw-os-agent`, `claw-os-base`, and
  `claw-os-desktop` independently.
- Preserve conffiles and run service-safe maintainer scripts.
- Bind each release to a signed release-security manifest so an installed
  system can refuse a superseded — but still validly signed — release.
- Build and sign Debian repository metadata for amd64 and arm64, with a
  `Valid-Until` freshness bound.
- Compose an independently built web artifact into the final Pages directory
  alongside `dists/` and `pool/`.

## Key Files

| Path | Role |
| --- | --- |
| `deb/build-debs.sh` | Package staging and `.deb` assembly |
| `deb/*/control` | Package metadata, ABI generation, and runtime dependencies |
| `deb/*/{preinst,postinst,prerm,postrm}` | Upgrade/install/remove behavior and the downgrade gates |
| `deb/claw-os-agent/extension-gid-scan.py` | Root-owned mount/ownership/ACL proof used during Agent postinstall |
| `deb/tests/test-extension-gid-scan.py` | Real getfacl, mount pinning, stacked mount, and timeout-process-group coverage |
| `deb/common/security-floor.preinst` | Shared pre-unpack refusal, rendered per package |
| `deb/common/50claw-os-security-floor` | APT pre-install hook configuration (conffile) |
| `release-security/policy.json` | Security epoch, ABI, protocols, tracked components |
| `release-security/make-manifest.py` | Canonical signed release manifest per package |
| `release-security/render-preinst.sh` | Embeds that manifest into the shared preinst |
| `release-security/verify-package-manifest.sh` | Single shared check binding a built `.deb` to its embedded manifest; run by every publish workflow |
| `release-security/sign-manifest.sh` | Fail-closed signing key resolution and manifest emission shared by both package builds |
| `release-security/gpg-sign.sh` | Signing helpers that keep the passphrase off `argv` |
| `apt-repo/check-index-freshness.py` | Refuse a future-dated, expired or stale published index |
| `apt-repo/sync-existing-packages.sh` | Merge local artifacts without replacing equal or newer signed candidates |
| `apt-repo/verify-release-security.sh` | Refuse a publication that regresses, is incoherent, or has no authenticated baseline |
| `apt-repo/tests/test-sync-existing-packages.sh` | Package merge and first-publication regression scenarios |
| `apt-repo/tests/test-release-security-publication.sh` | Publication-side downgrade regression scenarios |
| `deb/tests/test-security-floor-packaging.sh` | Real `.deb`, maintainer-script and signature downgrade scenarios |
| `deb/tests/test-security-floor-install.sh` | Real `dpkg` multi-package and `apt-get` hook transactions |
| `apt-repo/verify-release-security.sh` | Refuse a publication that regresses, is incoherent, or has no authenticated baseline |
| `deb/tests/test-agentd-packaging.sh` | Worker/extension-host binary, identity, service, and isolation contract |
| `../rootfs/overlay/usr/lib/cos/init/remove-home-overlay.sh` | Safe managed-home flattening before Base removal |
| `deb/tests/test-remove-home-overlay.sh` | Merged-tree, metadata, whiteout, and opaque-directory removal regression tests |
| `apt-repo/build-repo.sh` | Multi-arch index, Release, `Valid-Until`, by-hash, and GPG signatures |
| [`../web/`](../web/) | Independent web source; `dist/` is consumed during Pages composition |
| [`README.md`](README.md) | Package contract and manual commands |
| `../.github/workflows/publish-*-package.yml` | Independent package build/publication workflows |
| `../.github/workflows/publish-apt-repo.yml` | Internal reusable signed-repository publisher |
| `../.github/workflows/refresh-apt-metadata.yml` | Scheduled re-signing of repository metadata so `Valid-Until` never lapses |

## Dependencies

Package assembly consumes compiled binaries and source files; it does not need
a rootfs except for the separately staged desktop package. Rootfs features
install the resulting packages. Package dependencies express runtime layering
without forcing synchronized versions or publication schedules.

## Tests

```bash
ARCH=amd64 ./packaging/deb/build-debs.sh
bash packaging/apt-repo/tests/test-sync-existing-packages.sh
bash packaging/apt-repo/tests/test-release-security-publication.sh
bash packaging/deb/tests/test-security-floor-packaging.sh
bash packaging/deb/tests/test-security-floor-install.sh
bash packaging/deb/tests/test-remove-home-overlay.sh
sudo bash packaging/deb/tests/test-remove-home-overlay.sh --privileged-integration
GPG_KEY_ID=<fingerprint> ./packaging/apt-repo/build-repo.sh
```

`test-security-floor-packaging.sh` builds the verifier if it is not already
present; set `COS_SECURITY_FLOOR_BIN` to reuse an existing build, and
`COS_TEST_KEEP=1` to retain the scratch fixtures.
`test-security-floor-install.sh` runs real `dpkg --root` and `apt-get install`
transactions under `fakeroot`, so it needs `dpkg`, `apt-get` and `fakeroot` but
no privileges. All downgrade-protection tests generate their own ephemeral
signing key and need no repository secret.

Maintainer-script or update behavior changes must update
[`../docs/updating.md`](../docs/updating.md). Never publish an unsigned fallback
repository, and never publish a set that regresses the release-security
metadata.
