# Claw OS packaging

This directory produces the **`.deb` packages** that make up an installed
Claw OS system, and the **apt repository** that lets users `apt upgrade`
to newer versions.

## Layout

```
packaging/
├── deb/                       Debian package definitions
│   ├── build-debs.sh          Build all .debs into build/debs/
│   ├── claw-os-base/          The cos binary + apps/skills
│   ├── claw-os-browser/       cos-browser (Obscura) + service config
│   └── claw-os-systemd/       systemd unit files for system boot targets
└── apt-repo/
    └── build-repo.sh          Assemble build/apt-repo/ from build/debs/
```

## Packages

| Package | Contains | Architecture | Depends |
|---|---|---|---|
| `claw-os-base` | `cos`, `clawd`, `cos-init`, apps, skills, `/etc/cos/*`, `setup-home.sh` | `amd64`, `arm64` | `bash`, `coreutils`, `ca-certificates` |
| `claw-os-browser` | `cos-browser`, `cos-browser-worker`, `browser/service.json` | `amd64`, `arm64` | `claw-os-base (= ${binary:Version})`, `chromium` |
| `claw-os-systemd` | `clawd.service`, `cos-home-setup.service`, `cos-browser.service`, `/etc/default/cos-home` | `all` | `claw-os-base (= ${binary:Version})`, `systemd` |

## Build

The .debs are built **from already-compiled binaries** — `dpkg-deb --build`
just assembles staging trees. CI builds `cos` (musl) and `cos-browser`
(glibc) first, then invokes `packaging/deb/build-debs.sh`.

```bash
# Build binaries for the host architecture (amd64 shown here).
cargo build --release -p cos --target x86_64-unknown-linux-musl
cargo build --release -p cos-browser --target x86_64-unknown-linux-gnu

# Build .debs
./packaging/deb/build-debs.sh
# -> build/debs/claw-os-base_<version>_amd64.deb
# -> build/debs/claw-os-browser_<version>_amd64.deb
# -> build/debs/claw-os-systemd_<version>_all.deb

# Build apt repo
GPG_KEY_ID=<signing-key-fingerprint> ./packaging/apt-repo/build-repo.sh
# -> build/apt-repo/dists/trixie/main/binary-amd64/Packages.gz
# -> build/apt-repo/dists/trixie/main/binary-arm64/Packages.gz
# -> build/apt-repo/pool/main/c/claw-os-{base,browser,systemd}/*.deb
```

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
sudo apt install claw-os-base
```

Repository builds require `GPG_KEY_ID` and refuse to emit unsigned metadata.
GitHub Actions imports the private key from the
`CLAW_OS_APT_SIGNING_PRIVATE_KEY` secret; the corresponding public key is
embedded in Claw OS images and published beside the repository.
Local rootfs builds should set `COS_APT_PUBLIC_KEY_FILE` to a trusted
binary export of that public key. Download fallback is available only when
`COS_APT_PUBLIC_KEY_FINGERPRINT` is supplied explicitly.
