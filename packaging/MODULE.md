# Packaging Module

## Purpose

`packaging/` turns compiled binaries and source-tree assets into installable
Debian packages and a signed multi-architecture APT repository.

## Responsibilities

- Assemble the reusable `claw-os-agent`, the Claw OS integration
  `claw-os-base`, and the optional desktop package with one monotonic version.
- Preserve conffiles and run service-safe maintainer scripts.
- Build and sign Debian repository metadata for amd64 and arm64.
- Publish repository site assets alongside `dists/` and `pool/`.

## Key Files

| Path | Role |
| --- | --- |
| `deb/build-debs.sh` | Package staging and `.deb` assembly |
| `deb/*/control` | Package metadata and exact-version dependencies |
| `deb/*/{postinst,prerm,postrm}` | Upgrade/install/remove behavior |
| `apt-repo/preserve-desktop.sh` | Retain signed desktop artifacts across lightweight Agent/base publications |
| `apt-repo/build-repo.sh` | Multi-arch index, Release, and GPG signatures |
| [`README.md`](README.md) | Package contract and manual commands |
| `../.github/workflows/build-apt-repo.yml` | CI build/sign/deploy channel |
| `../.github/workflows/build-desktop-debs.yml` | Manual full-rootfs desktop package build |

## Dependencies

Package assembly consumes compiled binaries and source files; it does not need
a rootfs except for the separately staged desktop package. Rootfs features
install the resulting packages. All related packages use the same
commit-derived version when built together. Agent/base upgrades are atomic;
the independently built desktop declares a minimum compatible base version.

## Tests

```bash
ARCH=amd64 ./packaging/deb/build-debs.sh
GPG_KEY_ID=<fingerprint> ./packaging/apt-repo/build-repo.sh
```

Maintainer-script or update behavior changes must update
[`../docs/updating.md`](../docs/updating.md). Never publish an unsigned fallback
repository.
