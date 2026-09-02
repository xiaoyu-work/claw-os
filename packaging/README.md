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
│   ├── claw-os-desktop/       Optional graphical desktop metadata
│   └── common/                Shared downgrade-protection preinst, APT hook
├── release-security/
│   ├── policy.json            Security epoch, ABI, protocols, tracked components
│   ├── make-manifest.py       Canonical, signed per-package release manifest
│   ├── gpg-sign.sh            Sign without putting the passphrase in argv
│   ├── sign-manifest.sh       Fail-closed key resolution and manifest signing
│   ├── verify-package-manifest.sh Bind a built .deb to its embedded manifest
│   └── render-preinst.sh      Embed that manifest into the shared preinst
└── apt-repo/
    ├── sync-existing-packages.sh  Merge local and signed candidates safely
    ├── verify-release-security.sh Refuse a regressing or incoherent release set
    ├── check-index-freshness.py   Refuse a stale or future-dated signed index
    ├── build-repo.sh              Assemble build/apt-repo/ from build/debs/
    └── tests/
        ├── test-sync-existing-packages.sh      Package merge and first-publication cases
        └── test-release-security-publication.sh Publication regression cases
```

## Packages

| Package | Contains | Architecture | Depends |
|---|---|---|---|
| `claw-os-agent` | `cos`, `clawd`, `claw-agentd`, browser/semantic binaries, headless apps, skills, SDKs, extension-provenance trust roots, Agent system/user units | `amd64`, `arm64` | Debian/Ubuntu runtime libraries and `systemd` |
| `claw-os-base` | `cos-init`, managed agent-home setup, Claw OS boot/service policy | `all` | `claw-os-agent` |
| `claw-os-desktop` | COSMIC desktop, graphical Agent UI/bridge, desktop-only apps and assets | `amd64`, `arm64` | `claw-os-base` |

`claw-os-agent` is the exact same package on Ubuntu and Claw OS. It includes
`cos-browser`, the per-task App/MCP extension host, and all command-style apps.
Fresh installs create `cos-extension` at GID `60999`; safe upgrades may retain
the prior package's arbitrary sysusers GID only after proving it has no
unrelated ownership, membership, user, or process semantics. Supervised
workers keep the task uid, while hosted extensions use an exclusive
package-created locked account from `cos-ext-00..63` (`61000..61063`) plus
that primary gid. This blocks process injection
into the task owner or another extension domain. `clawd.service` also
delegates its cgroup-v2 subtree and pins `KillMode=control-group`; dynamic
extension execution fails closed unless per-task CPU, memory, pids, and
`cgroup.kill` containment plus private tmpfs mounts can be verified. Cleanup
failures retain a durable per-uid quarantine record until restart recovery
proves process, mount, task-state, and routed-ACL residue is gone.
`preinst` stops `clawd` for upgrades but does not run dependency-backed scans.
After the new package and its ordinary dependencies are unpacked/configured,
`postinst` invokes the single-link root-owned
`/usr/lib/cos/extension-gid-scan.py`. The helper snapshots mountinfo, rejects
stacked/duplicate mountpoints, opens each non-kernel-virtual mount, verifies
its `mnt_id`, device, inode, mode, and ownership before and after scanning,
then runs `find -xdev` and real numeric `getfacl` inspection through the pinned
descriptor. Nested, bind, tmpfs, persistent, and network mounts remain
separate roots. Each scan has a dedicated process group with bounded
TERM/SIGKILL escalation and residue verification. Malformed or changed
topology, inaccessible mounts, traversal errors, timeouts, ownership matches,
or access/default ACL qualifiers fail closed. The Agent package depends
explicitly on `acl`, `findutils`, `coreutils`, and Python for this proof.
Partial attempts are rolled back, and `postinst` writes the exact root-owned
runtime reservation manifest. Purge removes only
accounts recorded as package-created, still matching policy, and owning no
live process or runtime/quarantine state; preexisting correct accounts or
changed records are retained.
The systemd service runs with primary group `root`, so managed runtime/state
roots are `root:root`; `clawd.sock` is independently assigned `root:sudo`
mode `0660`.
Legacy subordinate-GID validation covers the union of the fixed UID pool and
the exact retained GID, including values in `61064..61183`, with
overflow-safe endpoint, interior, and covering-range checks.
`claw-os-base` adds only behavior
that intentionally turns a Debian-family rootfs into a Claw OS system.
When `claw-os-base` is removed, its maintainer script first snapshots the
visible managed home, unmounts OverlayFS, and materializes that merged view in
the underlying home. A migration or unmount failure blocks package removal and
retains the overlay/recovery data; see
[`docs/updating.md`](../docs/updating.md#removing-the-claw-os-integration-package).

## Extension trust roots

`claw-os-agent` creates two root-owned publisher trust roots:

| Path | Mode | Purpose |
|---|---|---|
| `/usr/lib/cos/trust/publishers.d/` | `0755`, files `0644` | Vendor keys shipped by the package |
| `/etc/cos/trust/publishers.d/` | `0755`, empty | Operator-managed keys; never shipped |

Content for the vendor root comes from
`packaging/deb/claw-os-agent/trust/publishers.d/`. The build fails if a
file there contains a `private_key` field, so signing keys can never be
shipped by accident. Both roots — and every ancestor up to `/` — must be
root-owned, non-symlink and free of group/world write bits; the loader
refuses a root that fails those checks and reports it as a diagnostic.

Apps and skills installed under `/usr/lib/cos` inherit package-manager
(vendor) trust: their content digest is pinned on first use, and a
later change rotates the pin with a `provenance.vendor_pin_rotated`
audit record. Everything a user installs needs a publisher signature.
See [`../docs/extension-provenance.md`](../docs/extension-provenance.md).

## Update downgrade protection

A repository signature proves who published an artifact, not that the artifact
is current. Every package therefore also carries release-security metadata that
lets an installed system refuse a *superseded* release even though it is still
validly signed.

`packaging/release-security/policy.json` is the source of truth for the security
epoch, the ABI generation, the protocol epochs, the minimum mutually compatible
versions, and the components whose content is tracked. `core/src/update/`
mirrors those constants and a unit test fails if the two disagree.

At package build time:

| Artifact | Path in the package |
|---|---|
| Canonical release manifest | `/usr/lib/cos/release-security/<package>/manifest.json` |
| Detached signature | `/usr/lib/cos/release-security/<package>/manifest.json.asc` |
| Verifier used by maintainer scripts | `/usr/lib/cos/bin/claw-security-floor` |
| APT pre-install hook (single executable token) | `/usr/lib/cos/apt/security-floor-hook` |
| APT hook configuration (conffile) | `/etc/apt/apt.conf.d/50claw-os-security-floor` |

Each package owns its **own** manifest subdirectory. Two packages must never
own the same regular file: dpkg would let whichever unpacked last decide what
the others' maintainer scripts read.

The manifest is generated by `release-security/make-manifest.py` and embedded
verbatim into the shared `preinst` by `release-security/render-preinst.sh`,
because a `preinst` runs before any of its own package's files exist. The
control file also exposes `XB-Claw-Os-Security-Epoch`, `XB-Claw-Os-Abi` and
`Provides: claw-os-abi-<N>`, so the merge job and APT's own solver can see the
epoch and ABI generation without unpacking anything.

Signing happens during the package build, since the manifest travels inside the
`.deb`. Set `CLAW_OS_RELEASE_SECURITY_KEY_ID` (or `GPG_KEY_ID`) with the secret
key available; the publication workflows import
`CLAW_OS_APT_SIGNING_PRIVATE_KEY` for exactly this.

`release-security/sign-manifest.sh` makes that decision for both package
builds, and it fails closed. Requesting *no* key is an explicitly announced
unsigned local build: it still produces a manifest, an installed system never
treats an unsigned one as verified, and `verify-release-security.sh` refuses to
publish it. Requesting a key that cannot be used — missing secret key, failed
signing, or a signature that does not verify — is a hard error that removes the
partial manifest, because clearing the key id and continuing would emit an
unsigned artifact under a name a publication workflow is about to upload.

Each publication workflow exports the public half of exactly the key it
imported and runs `release-security/verify-package-manifest.sh` with
`--require-signature --keyring <that key>` before uploading the artifact, so an
unsigned or foreign-signed package cannot reach the repository.

The installed-system behaviour, the recovery workflow and the threat boundary
are documented in [`../docs/updating.md`](../docs/updating.md#downgrade-protection).

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
indexes are regenerated, signed, and deployed to Pages. A build that carries a
*lower security epoch* is refused outright, even when its Debian version sorts
higher. A fixed non-cancelling concurrency group serializes this
read/merge/publish operation, while package builds remain independent.

Before the indexes are signed, `apt-repo/verify-release-security.sh` checks the
whole pool: every Claw OS package must carry a canonical release manifest that
describes exactly that package, version and architecture, signed by the key the
repository publishes; the set must be mutually compatible; and nothing may
regress the epoch, the version, or the bytes of a version already published.

Publication is anchored to a **signed baseline marker**. Once a repository has
published `dists/<suite>/release-security/baseline.json` and advertised
`Claw-Os-Release-Security-Baseline: 1` in its signed `Release`, every later
publication must retrieve and verify that marker and every published
per-package manifest. A `curl` failure, a non-200 status, a missing manifest or
a signature failure is fatal — never "there is no repository yet". Establishing
the baseline for a repository that predates downgrade protection is a
deliberate, one-time migration: run a package publication workflow with its
`release_security_bootstrap` input enabled. It is rejected once the marker
exists, and no ordinary build can set it.

The marker is never advertised without the artifact behind it.
`verify-release-security.sh` writes a structured status describing what it
actually produced, and `build-repo.sh` emits
`Claw-Os-Release-Security-Baseline: 1` only when that status says a signed
baseline was written and verified. A pool that still carries no
release-security metadata publishes honestly *without* the field, and only
under the explicit migration input — so the repository can never end up in a
signed state that claims a guarantee no artifact backs.

The published `Release` carries a `Valid-Until` bound and advertises
`Acquire-By-Hash`, and the indexes are mirrored under `by-hash/SHA256/`, so a
stale mirror cannot serve an old snapshot indefinitely. `Release` also lists
the SHA-256 of every `release-security/` file, which binds that metadata to the
snapshot it was published with: the next publication cross-checks what it
downloads against the signed index, so an origin cannot pair a current index
with an older, separately signed manifest.

`Valid-Until` is only a protection while it keeps being renewed.
`.github/workflows/refresh-apt-metadata.yml` re-signs the repository metadata
weekly without changing package bytes, preserves and re-verifies the baseline,
and fails loudly if the signing secret is unavailable.

Before trusting the current `InRelease`, the publisher verifies its signature,
refuses an index dated in the future or past its `Valid-Until`, refuses one
older than `COS_PUBLISH_MAX_INDEX_AGE_HOURS` (30 days by default), and fetches
everything with cache-busting, no-cache requests. An attacker who controls the
whole origin can still serve a consistent, still-valid older snapshot; that
residual is documented in `docs/updating.md` and is answered by the short
`Valid-Until`, the scheduled refresh and the per-machine floor.

The signing passphrase is never placed on a command line. Every signing path
goes through `release-security/gpg-sign.sh`, which hands it to `gpg` on a pipe
under `--passphrase-fd 0`, so it cannot be read out of `/proc` by another local
process.

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
echo "deb [signed-by=/usr/share/keyrings/claw-os-archive-keyring.gpg check-valid-until=yes allow-insecure=no allow-weak=no allow-downgrade-to-insecure=no by-hash=yes] https://xiaoyu-work.github.io/claw-os trixie main" \
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
