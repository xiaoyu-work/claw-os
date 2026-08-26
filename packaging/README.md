# Claw OS packaging

This directory produces the **`.deb` packages** that make up an installed
Claw OS system, and the **apt repository** that lets users `apt upgrade`
to newer versions.

## Layout

```
packaging/
├── deb/                       Debian package definitions
│   ├── build-debs.sh          Build Agent/base .debs into build/debs/
│   ├── build-desktop-deb.sh   Wrap a staged desktop root into a .deb
│   ├── claw-os-agent/         Reusable headless Agent for Debian/Ubuntu
│   ├── claw-os-base/          Claw OS distribution integration
│   └── claw-os-desktop/       Optional graphical desktop metadata
└── apt-repo/
    ├── preserve-desktop.sh     Retain the last signed desktop artifacts
    └── build-repo.sh          Assemble build/apt-repo/ from build/debs/
```

## Packages

| Package | Contains | Architecture | Depends |
|---|---|---|---|
| `claw-os-agent` | `cos`, `clawd`, browser/semantic binaries, headless apps, skills, SDKs, Agent system/user units | `amd64`, `arm64` | Debian/Ubuntu runtime libraries and `systemd` |
| `claw-os-base` | `cos-init`, managed agent-home setup, Claw OS boot/service policy | `all` | `claw-os-agent (= ${binary:Version})` |
| `claw-os-desktop` | COSMIC desktop, graphical Agent UI/bridge, desktop-only apps and assets | `amd64`, `arm64` | `claw-os-base (>= ${binary:Version})` |

`claw-os-agent` is the exact same package on Ubuntu and Claw OS. It includes
`cos-browser` and all command-style apps. `claw-os-base` adds only behavior
that intentionally turns a Debian-family rootfs into a Claw OS system.

## Build

The .debs are built **from already-compiled binaries** — `dpkg-deb --build`
just assembles staging trees. CI builds `cos` (musl) and `cos-browser`
(glibc) first, then invokes `packaging/deb/build-debs.sh`.

Package versions are generated as `<semver>+git<commit-count>.g<sha>`.
Pull-request artifacts use a lower-sorting `~pr...` suffix. Local dirty
trees must set `COS_PACKAGE_VERSION` explicitly to avoid reusing a stale
commit-derived package filename.

```bash
# Build binaries for the host architecture (amd64 shown here).
cargo build --release -p cos --target x86_64-unknown-linux-musl
cargo build --release -p cos-browser --target x86_64-unknown-linux-gnu

# Build .debs
./packaging/deb/build-debs.sh
# -> build/debs/claw-os-agent_<version>_amd64.deb
# -> build/debs/claw-os-base_<version>_all.deb

# The desktop rootfs feature stages and builds this separately:
# -> build/debs/claw-os-desktop_<version>_amd64.deb

# Build apt repo
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

The lightweight APT workflow rebuilds Agent/base on every architecture. Before
regenerating the repository, it verifies the currently published signed index
and restores the latest desktop package for any architecture without a newly
built desktop artifact. Agent/base releases therefore cannot erase an
independently published desktop from the cumulative APT pool.

To publish a new desktop version:

1. Provision Linux runners labeled `claw-os-desktop-amd64` and
   `claw-os-desktop-arm64`, each with at least 50 GB free on the workspace
   filesystem.
2. Manually run **Build Desktop packages** and wait for both architecture
   artifacts.
3. Copy that workflow run ID into the `desktop_run_id` input of
   **Build APT repo (.deb packages)** or **Release everything**.
4. The APT workflow rejects a desktop built from a newer commit than the
   Agent/base packages it is about to publish.

For the first repository publication, either provide both desktop artifacts or
explicitly select `bootstrap_repository`. Normal publications fail closed when
the existing signed repository is missing, so a transient Pages 404 cannot
silently erase cumulative packages.

APT publications share one non-cancelling concurrency group across direct and
umbrella runs. The signed-repository read, desktop preservation, regeneration,
and Pages deployment therefore cannot race another publication.

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
rootfs into Claw OS, install `claw-os-base`; its exact-version dependency pulls
in the same `claw-os-agent` artifact.

Repository builds require `GPG_KEY_ID` and refuse to emit unsigned metadata.
GitHub Actions imports the private key from the
`CLAW_OS_APT_SIGNING_PRIVATE_KEY` secret; the corresponding public key is
embedded in Claw OS images and published beside the repository.
Local rootfs builds should set `COS_APT_PUBLIC_KEY_FILE` to a trusted
binary export of that public key. Download fallback is available only when
`COS_APT_PUBLIC_KEY_FINGERPRINT` is supplied explicitly.

When the `CLAW_OS_APT_SIGNING_PRIVATE_KEY` secret is not configured (forks,
pull requests, or before a key has been provisioned), CI does not fail and
does not fall back to an unsigned repo: it drops the `apt-source` feature
from the images and skips the apt repo build and publication entirely. The
Docker and WSL pipelines are unaffected. Configuring the secret re-enables
the signed repo automatically, with no workflow changes.
