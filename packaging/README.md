# Claw OS packaging

This directory produces the **`.deb` packages** that make up an installed
Claw OS system, and the **apt repository** that lets users `apt upgrade`
to newer versions.

## Layout

```
packaging/
├── deb/                       Debian package definitions
│   ├── build-debs.sh          Build Agent, Base, or both into build/debs/
│   ├── build-desktop-deb.sh   Wrap a staged desktop root into a .deb
│   ├── claw-os-agent/         Reusable headless Agent for Debian/Ubuntu
│   ├── claw-os-base/          Claw OS distribution integration
│   └── claw-os-desktop/       Optional graphical desktop metadata
└── apt-repo/
    ├── sync-existing-packages.sh  Merge local and signed candidates safely
    ├── build-repo.sh              Assemble build/apt-repo/ from build/debs/
    └── tests/
        └── test-sync-existing-packages.sh  Exercise safe package merging
```

## Packages

| Package | Contains | Architecture | Depends |
|---|---|---|---|
| `claw-os-agent` | `cos`, `clawd`, browser/semantic binaries, headless apps, skills, SDKs, Agent system/user units | `amd64`, `arm64` | Debian/Ubuntu runtime libraries and `systemd` |
| `claw-os-base` | `cos-init`, managed agent-home setup, Claw OS boot/service policy | `all` | `claw-os-agent` |
| `claw-os-desktop` | COSMIC desktop, graphical Agent UI/bridge, desktop-only apps and assets | `amd64`, `arm64` | `claw-os-base` |

`claw-os-agent` is the exact same package on Ubuntu and Claw OS. It includes
`cos-browser` and all command-style apps. `claw-os-base` adds only behavior
that intentionally turns a Debian-family rootfs into a Claw OS system.
When `claw-os-base` is removed, its maintainer script first snapshots the
visible managed home, unmounts OverlayFS, and materializes that merged view in
the underlying home. A migration or unmount failure blocks package removal and
retains the overlay/recovery data; see
[`docs/updating.md`](../docs/updating.md#removing-the-claw-os-integration-package).

## Build

The .debs are built **from already-compiled binaries** — `dpkg-deb --build`
just assembles staging trees. CI builds `cos` (musl) and `cos-browser`
(glibc) first, then invokes `packaging/deb/build-debs.sh`.

Each package version is generated independently as
`<semver>+git<commit-count>.g<sha>`. Pull-request artifacts use a lower-sorting
`~pr...` suffix. Local dirty trees must set `COS_PACKAGE_VERSION` explicitly
to avoid reusing a stale commit-derived package filename.

```bash
# Build binaries for the host architecture (amd64 shown here).
cargo build --release -p cos --target x86_64-unknown-linux-musl
cargo build --release -p cos-browser --target x86_64-unknown-linux-gnu

# Build only the reusable Agent
./packaging/deb/build-debs.sh agent
# -> build/debs/claw-os-agent_<version>_amd64.deb

# Build only the architecture-independent Claw OS integration
./packaging/deb/build-debs.sh base
# -> build/debs/claw-os-base_<version>_all.deb

# Rootfs composition can still build Agent + Base together:
./packaging/deb/build-debs.sh all

# The desktop rootfs feature builds Desktop independently:
# -> build/debs/claw-os-desktop_<version>_amd64.deb

# Build the independent website artifact
npm ci --prefix web --replace-registry-host=always
npm run build --prefix web

# Compose it beside the signed apt repo
GPG_KEY_ID=<signing-key-fingerprint> ./packaging/apt-repo/build-repo.sh
# -> build/apt-repo/dists/trixie/main/binary-amd64/Packages.gz
# -> build/apt-repo/dists/trixie/main/binary-arm64/Packages.gz
# -> build/apt-repo/pool/main/c/claw-os-{agent,base,desktop}/*.deb
```

When the checkout is on a Windows-mounted WSL path whose directories always
report mode `0777`, point staging at the Linux filesystem so `dpkg-deb` can
enforce its control-directory permissions:

```bash
COS_DEB_STAGE_DIR=/tmp/claw-os-deb-staging ./packaging/deb/build-debs.sh
```

## Independent publication

Each package has its own manually dispatched CI:

| Workflow | Builds and publishes |
| --- | --- |
| **Publish Agent package** | `claw-os-agent` for amd64 and arm64 |
| **Publish Base package** | `claw-os-base` (`Architecture: all`) |
| **Publish Desktop package** | `claw-os-desktop` for amd64 and arm64 |

Every package workflow calls the internal reusable APT publisher. The publisher
reads the current signed repository, restores the latest packages not rebuilt
by the caller, and compares each locally built package with the signed candidate
for that package and architecture. Only a strictly newer local version replaces
the existing candidate; equal or older artifacts are discarded before the
indexes are regenerated, signed, and deployed to Pages. A fixed non-cancelling
concurrency group serializes this read/merge/publish operation, while package
builds remain independent.

No workflow run IDs or bootstrap flags are exchanged between package
workflows. A missing repository is treated automatically as first publication.
Desktop publication requires Linux runners labeled `claw-os-desktop-amd64`
and `claw-os-desktop-arm64`, each with at least 50 GB free.

## Apt repo URL

CI publishes the repo to GitHub Pages. The default URL hard-coded in the
`apt-source` rootfs feature is `https://xiaoyu-work.github.io/claw-os`.

End-user setup on a non-Claw-OS Debian/Ubuntu machine:

```bash
curl -fsSL https://xiaoyu-work.github.io/claw-os/claw-os-archive-keyring.gpg \
  | sudo tee /usr/share/keyrings/claw-os-archive-keyring.gpg >/dev/null
echo "deb [signed-by=/usr/share/keyrings/claw-os-archive-keyring.gpg] https://xiaoyu-work.github.io/claw-os trixie main" \
  | sudo tee /etc/apt/sources.list.d/claw-os.list
sudo apt update
sudo apt install claw-os-agent
```

That one package installs and enables `clawd.service`. It does not install
Claw OS home overlays, boot policy, or desktop integration. To turn a composed
rootfs into Claw OS, install `claw-os-base`; its runtime dependency pulls in
`claw-os-agent` without coupling their release schedules.

Repository builds require `GPG_KEY_ID` and refuse to emit unsigned metadata.
GitHub Actions imports the private key from the
`CLAW_OS_APT_SIGNING_PRIVATE_KEY` secret; the corresponding public key is
embedded in Claw OS images and published beside the repository.
Local rootfs builds should set `COS_APT_PUBLIC_KEY_FILE` to a trusted
binary export of that public key. Download fallback is available only when
`COS_APT_PUBLIC_KEY_FINGERPRINT` is supplied explicitly.

Package publication workflows require `CLAW_OS_APT_SIGNING_PRIVATE_KEY` and
cannot publish without it. Image workflows never fall back to an unsigned
source: when the key is unavailable they omit the `apt-source` feature while
continuing to build Docker and WSL artifacts.

OAuth client registration and end-user authorization are runtime concerns.
Agent packages and publication workflows never accept or embed Google or
Microsoft client identities. Apps or users configure provider clients through
the Claw OS credential store or runtime environment before starting OAuth
login.
