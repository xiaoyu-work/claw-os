# Packaging Module

## Purpose

`packaging/` turns compiled binaries and source-tree assets into installable
Debian packages and a signed multi-architecture APT repository.

## Responsibilities

- Assemble and publish `claw-os-agent`, `claw-os-base`, and
  `claw-os-desktop` independently.
- Preserve conffiles and run service-safe maintainer scripts.
- Build and sign Debian repository metadata for amd64 and arm64.
- Compose an independently built web artifact into the final Pages directory
  alongside `dists/` and `pool/`.

## Key Files

| Path | Role |
| --- | --- |
| `deb/build-debs.sh` | Package staging and `.deb` assembly |
| `deb/*/control` | Package metadata and runtime dependencies |
| `deb/*/{postinst,prerm,postrm}` | Upgrade/install/remove behavior |
| `apt-repo/sync-existing-packages.sh` | Merge local artifacts without replacing equal or newer signed candidates |
| `apt-repo/tests/test-sync-existing-packages.sh` | Package merge and first-publication regression scenarios |
| `../rootfs/overlay/usr/lib/cos/init/remove-home-overlay.sh` | Safe managed-home flattening before Base removal |
| `deb/tests/test-remove-home-overlay.sh` | Merged-tree, metadata, whiteout, and opaque-directory removal regression tests |
| `apt-repo/build-repo.sh` | Multi-arch index, Release, and GPG signatures |
| [`../web/`](../web/) | Independent web source; `dist/` is consumed during Pages composition |
| [`README.md`](README.md) | Package contract and manual commands |
| `../.github/workflows/publish-*-package.yml` | Independent package build/publication workflows |
| `../.github/workflows/publish-apt-repo.yml` | Internal reusable signed-repository publisher |

## Dependencies

Package assembly consumes compiled binaries and source files; it does not need
a rootfs except for the separately staged desktop package. Rootfs features
install the resulting packages. Package dependencies express runtime layering
without forcing synchronized versions or publication schedules.

## Tests

```bash
ARCH=amd64 ./packaging/deb/build-debs.sh
bash packaging/apt-repo/tests/test-sync-existing-packages.sh
bash packaging/deb/tests/test-remove-home-overlay.sh
sudo bash packaging/deb/tests/test-remove-home-overlay.sh --privileged-integration
GPG_KEY_ID=<fingerprint> ./packaging/apt-repo/build-repo.sh
```

Maintainer-script or update behavior changes must update
[`../docs/updating.md`](../docs/updating.md). Never publish an unsigned fallback
repository.
