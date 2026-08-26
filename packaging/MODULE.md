# Packaging Module

## Purpose

`packaging/` turns compiled binaries and source-tree assets into installable
Debian packages and a signed multi-architecture APT repository.

## Responsibilities

- Assemble and publish `claw-os-agent`, `claw-os-base`, and
  `claw-os-desktop` independently.
- Preserve conffiles and run service-safe maintainer scripts.
- Build and sign Debian repository metadata for amd64 and arm64.
- Publish repository site assets alongside `dists/` and `pool/`.

## Key Files

| Path | Role |
| --- | --- |
| `deb/build-debs.sh` | Package staging and `.deb` assembly |
| `deb/*/control` | Package metadata and runtime dependencies |
| `deb/*/{postinst,prerm,postrm}` | Upgrade/install/remove behavior |
| `apt-repo/sync-existing-packages.sh` | Retain signed packages not rebuilt by the current CI |
| `apt-repo/build-repo.sh` | Multi-arch index, Release, and GPG signatures |
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
GPG_KEY_ID=<fingerprint> ./packaging/apt-repo/build-repo.sh
```

Maintainer-script or update behavior changes must update
[`../docs/updating.md`](../docs/updating.md). Never publish an unsigned fallback
repository.
