# Packaging Module

## Purpose

`packaging/` turns compiled binaries and source-tree assets into installable
Debian packages and a signed multi-architecture APT repository.

## Responsibilities

- Assemble `claw-os-base`, `claw-os-browser`, `claw-os-systemd`, and desktop
  packages with one monotonic version.
- Preserve conffiles and run service-safe maintainer scripts.
- Build and sign Debian repository metadata for amd64 and arm64.
- Publish repository site assets alongside `dists/` and `pool/`.

## Key Files

| Path | Role |
| --- | --- |
| `deb/build-debs.sh` | Package staging and `.deb` assembly |
| `deb/*/control` | Package metadata and exact-version dependencies |
| `deb/*/{postinst,prerm,postrm}` | Upgrade/install/remove behavior |
| `apt-repo/build-repo.sh` | Multi-arch index, Release, and GPG signatures |
| [`README.md`](README.md) | Package contract and manual commands |
| `../.github/workflows/build-apt-repo.yml` | CI build/sign/deploy channel |

## Dependencies

Package assembly consumes compiled binaries and source files; it does not need
a rootfs. Rootfs features install the resulting packages. All related packages
use the same commit-derived version so APT upgrades them together.

## Tests

```bash
ARCH=amd64 ./packaging/deb/build-debs.sh
GPG_KEY_ID=<fingerprint> ./packaging/apt-repo/build-repo.sh
```

Maintainer-script or update behavior changes must update
[`../docs/updating.md`](../docs/updating.md). Never publish an unsigned fallback
repository.
