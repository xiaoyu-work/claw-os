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
    ├── check-index-freshness.py   Check signed dates and any declared expiry
    ├── build-repo.sh              Assemble build/apt-repo/ from build/debs/
    └── tests/
        ├── test-sync-existing-packages.sh      Package merge and first-publication cases
        └── test-release-security-publication.sh Publication regression cases
```

## Packages

| Package | Contains | Architecture | Depends |
|---|---|---|---|
| `claw-os-agent` | `cos` with the Claw-owned TUI, `clawd`, `claw-agentd`, browser/semantic binaries, skills, SDK/runtime, trust roots, Agent system/user units and App channel configuration; no App payloads | `amd64`, `arm64` | Debian/Ubuntu runtime libraries, `systemd`, and `pkexec`; recommends `claw-apps-agent` |
| `claw-os-base` | `cos-init`, managed agent-home setup, Claw OS boot/service policy | `all` | `claw-os-agent` |
| `claw-os-desktop` | COSMIC shell/services, graphical Agent UI/bridge, initial setup, PAM/policy and system assets; no product-native payloads | `amd64`, `arm64` | `claw-os-base`; recommends `claw-apps-desktop` |

### Coordinated 0.2.1 source ownership cutover

The approved source boundary is **`1:0.2.1`**, not `1:0.3.0`. App packages
bound `Breaks`/`Replaces` to the former Agent/Desktop owners strictly below
that boundary. Installed paths, all 63 retained identities (51 headless and
12 native), data and grants are unchanged. No updater, identity migration or
new privilege is introduced.

The separately signed App channel owns product/capability packages, native
ELFs/resources, Browser assets, Mail's XPI, `claw-app-support` and named Python
libraries. Support provides `claw-app-support-v1`; Files alone provides
`claw-app-python-claw-files-v1`. App packages cannot provide OS/package/authority
ABIs. Agent retains `claw_os_sdk`, `cos_runtime`, toolkit/system services and
Root helpers. Its `claw-os-app-runtime-v1 (= 0.2.1)` implementation version
comes from `app-platform.json`; App packages require `>= 0.2.1`, still ABI 1.

`scripts/app_packages.py prepare` authenticates the official App APT release,
bounded freshness and package index, downloads the selected dependency closure,
then verifies final Debian bytes and one dpkg owner per payload path. Exact
native/headless product dependencies can select retained matching versions.
Release jobs retain those public APT inputs, reverify them before assembly and
refuse any OS/App file overlap. Missing, stale, incomplete or changed inputs
fail closed; there is no production source fallback. Images install the same
checked independent `.deb` files in the ordinary APT transaction.

Bootstrap order is approved SDK artifact → independent App APT packages →
payload-free OS Agent/Desktop. Agent only *recommends* the headless set, so
App runtime dependencies create no circular hard dependency and Agent-only
Ubuntu/WSL installs do not require the full Desktop. Configure both signed
channels before a default install; Agent also ships `claw-apps.sources` and
the public key. An existing minimal `--no-install-recommends` Agent can run
`apt update` and explicitly select `claw-apps-agent`, native App packages or
the protected Store later. Maintainer scripts do not run nested APT installs.

Agent also owns the shared `Cosmic` icon theme used by the public toolkit.
Native Apps installed on standalone Ubuntu/WSL must not require the Desktop
package just to draw standard controls. `deb/native_runtime_assets.py` stages
the original theme and its CC-BY-SA attribution/licenses; Desktop retains
its additional Tela theme but cannot package another copy of `Cosmic`.
The original paths remain unchanged, with the Agent's `Replaces` bounded to
Desktop versions below `1:0.2.1`. This data-only ownership transfer grants no
display, clipboard, filesystem or other App capability.

The Agent also supplies the unprivileged `pop-launcher` runtime, its original
declared plugin manifests/scripts and MPL/GPL attribution through
`claw-os-launcher-backend-v1 (= 1)`. It builds the actual OS-owned backend;
neither an App nor an OS package embeds a second frontend or installed App
identity. Desktop refuses a second owner of those paths, using the same bounded
pre-0.2.1 ownership transfer. The independently packaged upstream
`pop-launcher` conflicts explicitly rather than having files silently
overwritten. This is runtime availability only, not catalog/history sharing,
launch permission, service activation or GUI acceptance. App sandboxes and
ordinary owner reviews still control those resources.

**Desktop source links are cut over:** Calendar, Clipboard and Widget Rail
binaries, desktop entries, translations/icons and common-Host launchers are
App-owned; the OS retains their panel layout IDs without linking product UI
or installing duplicate aliases. Notifications uses the existing public SDK
presentation records/proxy/FD constants instead of private config/util or
direct product preference writes. Grouping, styles, actions, bounded decoded
icons and confirmed popup mute are retained without merging renderer graphs.
`native-boundary` still rejects reintroduced App source links.
`desktop/justfile` keeps source-only `build-source-fixtures` and
`install-source-fixtures` with an explicit immutable `CLAW_APP_SOURCE_LOCK`;
production no longer fetches/mounts native App sources or copies manifests.
Desktop requires runtime-v1 implementation 0.2.1 through the normal template.
Actual Linux native builds and installed native/GUI acceptance remain
required. Public 0.2.1 SDK/App artifacts are not adopted or published here;
App development still pins real published platform 0.2.0 and publication gates
remain pending.

The Desktop source also installs `/usr/libexec/claw-os-applet-provider` and
provides `claw-os-applet-services-v1 (= 1)`. Assembly refuses a missing,
non-regular, non-executable, truncated or wrong-architecture helper before
declaring that ABI. The [public client contract](../claw-os-sdk/wire/v1/applet-services.md)
enters a later SDK release; provider implementation stays in the OS package.

Calendar reads use Agent's `claw-os-calendar-read-v1` service and the separately
built `/usr/lib/cos/bin/claw-calendar-reader`. Its query code is GPL-3.0-only,
with `calendar-reader.LICENSE` installed in Agent's documentation directory.
The binary is part of the signed component inventory and is measured before
launch. Desktop retains the public Applet helper and requires this service;
neither package availability nor a consumer name grants Calendar access.
This source wiring does not cut over existing App payloads, change grants or
clear GUI/Clipboard resource-admission gates; see [updating](../docs/updating.md).

### Pre-cutover compatibility history (not production 0.2.1 ownership)

The source-composition descriptions below record the earlier bundled layout.
The ownership graph and guarded APT consumers above supersede those delivery
claims; their source staging is retained only for explicit fixtures.

`claw-os-agent` was the exact same package on Ubuntu and Claw OS. It included
`cos-browser`, the per-task App/MCP extension host, and all command-style apps.
It also owns the Mail XPI at
`/usr/lib/thunderbird/distribution/extensions/claw-mail-ai@claw.os.xpi`, built
reproducibly by the Mail product in
[`xiaoyu-work/clawos-app`](https://github.com/xiaoyu-work/clawos-app/tree/main/products/mail).
This small UI protocol adapter
must upgrade with its native host and Python implementation, not wait for an
independent desktop release. It adds no Thunderbird runtime dependency;
desktop/rootfs registration enables it when Thunderbird is installed.

[`apps.lock.json`](apps.lock.json) selects the App repository's `main` branch,
business `products`, optional shared-client `capabilities`, and the exact
installed App IDs. Its version-2 selection has no manually maintained commit
pin. `scripts/app_sources.py` queries `main` on each fresh preparation, fetches
the resolved commit into an immutable `build/app-sources/<sha>` cache, and
records `build/app-sources/resolved.json`. Failed branch resolution never
falls back to an older cache.

Each package build freezes that selection once. Agent packaging uses
`--write-lock` and passes the resulting snapshot through `CLAW_APP_SOURCE_LOCK`
to all later staging steps. Desktop packaging consumes the snapshot captured
when its native sources were prepared, not a newer manifest fetched after
compilation. Both packages retain the selection and exact SHA in
`/usr/share/doc/<package>/app-sources.json`. Explicit version-1 snapshots
remain readable for reproducing a build; they are not the repository's default
source policy. Modified caches, mismatched selections and unexpected App IDs
remain errors.

The OS still owns capability enforcement, native launch authority and package
signing. New builds consume new App `main` commits without a pin-edit commit;
installed changes still arrive through signed packages, never runtime Git
downloads. The separate SDK/runtime/toolkit artifact pin is unchanged.

Common App support comes from the same build snapshot's `shared/python`. The OS wrapper
`python3 scripts/app_sources.py --stage-shared <root>` invokes the public
`tools/stage.py --shared --root <root>` contract once, separately from product
staging and OS SDK/runtime. It installs the three owned names `_shared`,
`gateway` and `canonical_argv.py` under `usr/lib/cos/python`; exact identical
co-staging is reusable, while conflicting bytes, modes, symlinks or extra files
fail before copying any common library. Tests, vectors and bytecode are excluded.
No OS `apps/` source tree or duplicate support under `usr/lib/cos/apps` remains.
Agent keeps its `python3-idna (>= 3.3), python3-idna (<< 4)` dependency.

This is the existing compatibility delivery: `claw-os-agent` still
owns the installed common files. Independent App APT/common packages must replace
that delivery in a separately coordinated ownership cutover. This checkpoint
adds neither a permanent new source-build dependency for that cutover nor an
installed Git updater.

A compatible App business change needs no OS core implementation change.
The remaining coupling is explicit source composition and release delivery:
package metadata and the staging CLI are build contracts, development selects
the versioned SDK/runtime/toolkit platform artifact, and
installed changes still require a package release here. Bundled
`cos_runtime` is not an independently published third-party SDK. Paired
migration commits are cutover choreography, not a requirement to edit both
implementations for every future feature. Pins alone do not prove complete
decoupling.

Native Files is composed under `build/native-apps/cosmic-files`, preserving
both executables and resources in the desktop package. Files also exports the
shared `claw_files.document` parsing/conversion library into the Agent package;
the Document Engine client and native Files use this one implementation.
The native executable embeds its product helpers and uses installed OS
SDK/runtime libraries, not mutable App entrypoints.

Document Engine's complete legacy `doc` source lives at the pinned
`capabilities/document-engine/apps/doc`, with `kind: "shared-capability-client"`
package metadata. This is not a Documents business product or a new runtime
identity. The group's declared dependency selects Files' named `claw_files`
export for tests and staging, including Doc-only staging without Files Apps.
Co-staging never merges conflicting library trees. Agent still owns
`/usr/lib/cos/apps/doc`, `/usr/lib/cos/python/claw_files`, SDK/runtime and the
canonical argument module. All six operations, manifest needs/AI declarations,
outputs, memory identity and user state remain unchanged. The source lock now
selects 63 retained identities after the original 75-identity source relocation:
24 business products plus four capability groups, with 51 Agent and 12 desktop
identities; the retired messaging connectors and Email Gateway are not fallbacks.
Native preparation stays product-only.

Storage SDK supplies independent `db` and `kv` clients from
`capabilities/storage-sdk/apps/<id>`, not from the distinct Storage business
product. DB's original manifest, SQLite
implementation and MCP server install unchanged at `/usr/lib/cos/apps/db`.
Five MCP/CLI tools retain their exact read/write needs and 1,000-returned-row
bound. Existing files stay at `$COS_DATA_DIR/db/<name>.db` within the same
owner/App partition; no KV or Agent-memory state is read, imported or merged.
No SDK/provider or native source is added to the capability payload. The
installed Agent package still has 63 identities, all from the external App pin;
Desktop still has 12. Normal signed package updates remain the only installed
update path; source relocation adds no data transition.
KV's manifest and MCP-only data implementation install at `/usr/lib/cos/apps/kv`;
its string map remains at `$COS_DATA_DIR/kv.json` in the independent `kv`
partition. No `main.py` is invented. App common support supplies `_shared.atomic`
alongside the separately owned OS SDK/runtime. KV fixes stale caches, lost
concurrent updates, failed-write cache publication and non-private replacement.
List/dump now require fixed whole-store read authority instead of borrowing
named-key grants; get/set/delete keep separate exact-key needs and no stored
grant changes. See [the update contract](../docs/updating.md#app-data-moves-into-per-app-directories).

HTTP supplies `net` from `capabilities/http/apps/net` at the same immutable pin.
Its original manifest, HTTP implementation and MCP server install at
`/usr/lib/cos/apps/net`; App common support supplies `_shared.safe_http`, while
OS supplies SDK/runtime, policy, egress enforcement and worker isolation. Existing two
MCP/CLI contracts, exact endpoint/output grants, size bounds and atomic download
behavior are unchanged. No provider, user data, additional identity or runtime
source-fetch/update mechanism moves into this capability group.

AI Helpers supplies the complete `summarize` client from
`capabilities/ai-helpers/apps/summarize`, installed at `/usr/lib/cos/apps/summarize`
in Agent only. The OS supplies SDK AI and bundled policy/memory exports, not
copied providers. Its strict external-content AI policy, monthly budget,
request cap, result shape and `self:summarize` memory namespace remain unchanged.
The manifest's fixed wildcard AI binding now matches its existing runtime
check; [the update note](../docs/updating.md#summarize-source-and-ai-scope-binding)
explains this correction without rewriting grants or AI consent.
No App production or helper source remains under OS `apps/`. Source completion does not establish
full boot/upgrade acceptance or complete backend/state/identity consolidation.

DB/KV/Net/Summarize integration fixtures compare the entire staged runtime payload, including
bytes, modes and symlinks, with the pinned source's declared staging exclusions.
They execute the manifest-selected MCP entrypoint over stdio with installed
OS libraries, rather than importing private App modules or the SDK dispatcher.
Signed worker fixtures stage that same complete payload; adding a private
module or changing the declared entrypoint does not require an OS filename list.
Summarize's real Agent build-block fixture exercises public MCP and SDK wire
with synthetic AI/policy/memory responses, including denied consent/budgets,
safety and memory failures. It does not contact a model or mount owner state.

Native Store is composed under `build/native-apps/cosmic-store`, including its
original default-feature graph and flathub-stats workspace. The desktop package
retains the Store descriptor, executable and resources; pkg remains in the
Agent package with its own transaction grants. The native read-only catalog
embeds shared product logic and imports only installed OS support libraries.

Native Settings is composed under `build/native-apps/cosmic-settings`, retaining
the full nested workspace, all original default pages and toolkit patches.
The desktop package keeps its native manifest/binary, page entries, translations,
icons, polkit resources and config defaults. Its eleven management Apps remain
separate Agent-package identities; source relocation does not move user settings
or copy settings-daemon/provider authority into the product.
Permission-management UI/MCP requires the `claw-os-app-permissions-v1`
virtual service dependency. The Agent package provides it; the Desktop package
requires it so independent publication cannot silently pair the new UI with
an old broker. Permission overrides stay in the existing root-owned approval
generation state and durable approval receipts, outside App data and signed
manifests. Until-revoked restoration does not extend ordinary execution-grant
lifetimes. Native activation requires the existing owner user-systemd/session
bus, never removing `NoNewPrivileges` from system services. The compiled
permission and fixed-launch clients use the packaged `/usr/local/bin/cos`
explicitly.

Native Capture is composed under `build/native-apps/cosmic-screenshot` with
its original independent portal-client graph. The desktop package retains
`/usr/bin/cosmic-screenshot`, its descriptor, desktop entry and all icons.
It depends on Agent's `claw-os-capture-v1`: non-interactive capture requires
separate screen and exact-directory write grants, not a worker session bus.
The shared interactive portal and installed screenshots/configuration remain
OS/user-owned. No Capture binary or native resources enter the headless Agent
payload, and normal signed package updates remain the only installed update path.

Media Player is composed at `build/native-apps/cosmic-player` with its full
native source and original locked renderer. Desktop owns `/usr/bin/cosmic-player`,
the `com.clawos.Player` resources/thumbnailer and the seven-tool descriptor;
Agent provides the required `claw-os-media-player-v1` adapter. Observation and
control require separate exact grants and operate only this owner's installed
native Player. There is no worker bus or second playback state. Replace the
signed manifest and matching packages together; binary-only hot-swap is refused.
No Player binary/resources or test-fixture executable enter the headless payload.

Notifications is composed at `build/native-apps/cosmic-notifications`, including
the full original daemon, config/util crates and standalone default/optional
graph. Desktop owns `/usr/bin/cosmic-notifications` and its MCP descriptor;
its panel/applets retain their existing links to the component's private
config/util sources. Those inputs are still composed with the whole native
component, but are no longer declared `native_libraries` exports; the source
stager does not invent exports to preserve these legacy links.
Agent provides `claw-os-notifications-v1`, preserving authoritative SQLite,
owner/source validation, DND/preferences, audit and delivery leases/retries.
MCP 0.2 uses durable strings instead of desktop integer IDs and explicitly
rejects icon paths/URLs; popup timeout/transient metadata does not bypass
durability. Update the signed desktop payload and compatible Agent together,
then restart the desktop; binary-only hot-swap is refused. No fixture binary,
new user-data store or legacy `notify` JSON import enters either payload.
The complete `notify` Python facade now comes from the same product revision,
but `/usr/lib/cos/apps/notify` belongs only to Agent. Its separate send/list
grants address `app:notify` through the OS service and installed Python SDK.
There is no dependency on calling the native App, no new storage directory,
and no historical-data import. See [the explicit compatibility decision](../docs/updating.md#legacy-notify-service-transition).

It also creates the empty root-owned `/usr/lib/cos/extensions` registry.
Executable Agent extension packages placed there remain inactive until their
signed id is selected in user configuration; see
[`docs/extension-provenance.md`](../docs/extension-provenance.md) and
[`docs/extension-abi.md`](../docs/extension-abi.md).
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
`preinst` stops `clawd` for upgrades. Identity provisioning checks exact
name/UID/GID/NSS, shadow, systemd-homed, subordinate-ID and live-process
collisions. It does not crawl host filesystem trees or enumerate mounts:
WSL, container, network and removable mount topologies are not
package-installation prerequisites. Numeric identity is not filesystem
authority; extension access remains governed by the worker sandbox and
capability-bound mounts.
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

## Authenticated desktop runtime

The unpublished authenticated desktop runtime adds the OS-only
`claw-os-display-session-v1` interface: Agent provides it and Desktop requires
it. Agent owns `/usr/lib/cos/bin/claw-display-host`,
`/usr/local/bin/claw-display-session`, `/usr/local/bin/claw-gui-runner` and
`/usr/lib/<GNU-multiarch>/security/pam_claw_display.so`. The executables install
as `0755`, PAM as `0644`, with Root ownership and protected ancestors. Desktop
owns the compositor/session and the greetd/PAM activation policy.

Build the PAM crate for GNU/glibc with `libpam0g-dev`; runtime requires
`libpam0g`. The Agent workflow builds that native library explicitly, separately
from musl core binaries. Neither the interface nor package installation grants
an App GUI resources. Coordinate the package set and
[logout/re-login](../docs/updating.md#root-managed-desktop-session-lifetime);
the private fixture does not establish a full Debian transaction or image
acceptance. No release or version change is implied by this source wiring.

### File-manager desktop intents

Agent provides `claw-os-file-manager-v1 (= 1)` and installs the unprivileged
`/usr/lib/cos/bin/claw-file-manager` (`0755`). Desktop requires that public OS
interface and installs `/etc/xdg/autostart/claw-file-manager.desktop` as a
`0644` conffile.
`cosmic-session` consumes this optional entry directly, retains its child and
terminates/waits it on session exit or restart. This works independently of
the general autostart feature. `OnlyShowIn=COSMIC;` limits availability, while
`X-systemd-skip=true` prevents activation under a user manager without the
authenticated session's kernel login ancestry. Neither label grants authority.
Agent-only Ubuntu/WSL installs no activation file, user service, autostart or
GNOME/Nautilus override and never starts or claims this desktop interface.

The helper owns `org.freedesktop.FileManager1` at
`/org/freedesktop/FileManager1` without replacing or queuing behind another
owner. `ShowFolders`, `ShowItems` and `ShowItemProperties` accept URI arrays
and StartupID. Each authenticates `header.sender()` with the bus daemon's
`GetConnectionUnixUser` and requires the helper's kernel-derived non-root UID.
The three methods forward the literal URIs through only
`/usr/local/bin/cos app <configured-id> --gui [URIs...]`; they do not supply
method-specific executable flags. StartupID retains the existing ignored
presentation semantics. No shell, legacy executable, owner/session/socket
selector or new installed App identity is involved.

`--app <canonical-id>` selects the OS routing preference, defaulting to
`cosmic-files`, not an exclusive App entitlement. CLI management namespaces
cannot be targets. Normal Root admission still
verifies that target's signed GUI, actual ancestry and delegation, owner,
PAM/kernel login, protected review, capabilities, sandbox and managed lifetime.
The helper clears inherited authority/override environment, including all
`COS_*` and `CLAW*` markers, and forwards only basic owner/presentation settings.
Apps still have no raw user bus, FileManager1 bridge or weakened IPC sandbox.

Each request permits at most 128 URLs, 4096 UTF-8 bytes per URL and 64 KiB of
JSON-encoded GUI arguments; URLs and the bounded StartupID reject controls.
Eight pending/running launcher children retain capacity through `wait`.
Shutdown cancels queued intents and kills/waits owned CLI children; parent
session death signals the facade, and facade death also kills those CLIs.
Root, not the facade, owns actual GUI retirement.
No intent is automatically retried. Diagnostics report only launcher status,
never URIs, StartupIDs, credentials or captured App output.

A successful D-Bus method means **intent accepted**, not GUI started or
completed. The ordinary `cos --gui` child awaits Root's real checked outcome;
process creation alone is not that evidence. The private core bin tests use
their own bus and an explicit test-only fake executable, never installed
`cos`, a live GUI, real PAM, accounts or clipboard. Run on Linux:

```bash
cargo test -p cos --bin claw-file-manager -- --test-threads=1
cargo test --manifest-path desktop/session/Cargo.toml --locked file_manager::tests
```

The full Files product/App-owned FileManager1 removal and real authenticated
COSMIC autostart/GUI acceptance are separate coordinated cutover checks.
The existing GUI CLI audit in `core/src/router.rs::launch_app_gui` still passes
raw GUI arguments to `audit::log_entry`; its parent-owned privacy projection
must be corrected before this transport is installed. Discarding child output
does not sanitize that durable log, and the facade does not bypass OS auditing.
This source wiring changes no release, App pin, security floor or update gate.

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
python3 scripts/app_packages.py prepare --variant agent --architecture amd64
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

Build from a Linux-filesystem checkout. Windows-mounted WSL directories may
report mode `0777`, which cannot preserve Debian control permissions or native
source modes; they are not supported native build/staging roots.

The terminal renderer is part of the musl-built `cos` binary. Packaging has no
separate TUI source acquisition, executable, runtime dependency or update
channel.

## Independent publication

The **Release Claw OS** workflow (`release.yml`) has one target selector:

| Target | Builds and publishes |
| --- | --- |
| `agent` | `claw-os-agent` for amd64 and arm64, with TUI and Ubuntu install/reinstall checks |
| `base` | `claw-os-base` (`Architecture: all`) |
| `core` | Agent and Base together, without Desktop or image builds; coordinated protocol updates and headless recovery |
| `desktop` | `claw-os-desktop` for amd64 and arm64, using dedicated rootfs runners |
| `images` | Shared-rootfs Docker/WSL images; consumes same-run signed Agent/Base packages without publishing them to APT |
| `website` | Website and Pages composition; preserves every existing package |
| `all` | Shared tests, all packages, Docker/WSL and one Pages publication |

Each run freezes one commit-derived package version and the authenticated
App APT package closure for selected architectures. Only the shared test suite
in `all` separately receives an explicit immutable App-source fixture; no
production builder consumes it. Package checks reverify official APT evidence
and refuse App/support/library ownership before upload. Images download the
signed Agent/Base packages instead of rebuilding the common binaries; Docker
and WSL share one stamped rootfs per architecture.

One publisher gathers every selected package and one website artifact. It
reads the current signed repository, restores the latest packages not rebuilt
by the caller, and compares each locally built package with the signed candidate
for that package and architecture. Only a strictly newer local version replaces
the existing candidate; equal or older artifacts are discarded before the
indexes are regenerated, signed, and deployed to Pages. A build that carries a
*lower security epoch* is refused outright, even when its Debian version sorts
higher. A non-cancelling release queue serializes publication runs; architecture
and component builds within a run remain parallel.

Before the indexes are signed, `apt-repo/verify-release-security.sh` checks the
whole pool: every Claw OS package must carry a canonical release manifest that
describes exactly that package, version and architecture, signed by the key the
repository publishes; the set must be mutually compatible; and nothing may
regress the epoch, the version, or the bytes of a version already published.
Protocol generations are compared against the maximum across the candidate
pool and authenticated previous package manifests. A newer package version
cannot lower that floor, and an Agent-only release cannot preserve a Base
whose declared protocols are too old. Select `core` for a coordinated
Agent/Base release, or `all` if Desktop also declares an older protocol.
Compatible package versions remain independently publishable.

Publication is anchored to a **signed baseline marker**. Once a repository has
published `dists/<suite>/release-security/baseline.json` and advertised
`Claw-Os-Release-Security-Baseline: 1` in its signed `Release`, every later
publication must retrieve and verify that marker and every published
per-package manifest. A `curl` failure, a non-200 status, a missing manifest or
a signature failure is fatal — never "there is no repository yet". Establishing
the baseline for a repository that predates downgrade protection is a
deliberate, one-time migration: select a package target in `release.yml` with its
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

The published `Release` intentionally omits `Valid-Until`, so an infrequently
updated repository needs no scheduled re-signing. It advertises
`Acquire-By-Hash`, and indexes are mirrored under `by-hash/SHA256/` to retrieve
the exact index named by a signed snapshot. `Release` also lists
the SHA-256 of every `release-security/` file, which binds that metadata to the
snapshot it was published with: the next publication cross-checks what it
downloads against the signed index, so an origin cannot pair a current index
with an older, separately signed manifest.

Before trusting the current `InRelease`, the publisher verifies its signature,
refuses an index dated in the future or past any declared `Valid-Until`, and fetches
everything with cache-busting, no-cache requests. An attacker who controls the
whole origin can indefinitely replay a consistent, correctly signed
non-expiring index. This maintenance tradeoff is documented in
`docs/updating.md`: the per-machine floor prevents observed-state regression,
not withholding an update the machine has never seen. Indexes have no maximum
age; signing keys and package release-security manifests retain their
independent validity rules.

The signing passphrase is never placed on a command line. Every signing path
goes through `release-security/gpg-sign.sh`, which hands it to `gpg` on a pipe
under `--passphrase-fd 0`, so it cannot be read out of `/proc` by another local
process.

Artifacts are downloaded from the same workflow run, never selected from an
older successful run. The one-time bootstrap remains an explicit package
publication input. A missing repository is treated automatically as first publication.
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
## Nearby transport package boundary

Agent builds [`claw-nearby`](../crates/claw-nearby/MODULE.md) through its own
manifest and lock with `--locked`, then installs the two fixed binaries,
Root-only D-Bus policy, D-Bus activation service, NetworkManager device section
and awdl0 link policy. `claw-os-nearby-v1` is a service-presence contract, not
hardware certification. Desktop depends on it for its sharing center.

The radio alone receives raw/network-admin privileges; `clawd.service` is not
broadened. The unsigned/active-user upstream radio policy is not installed.
The required optional physical-radio tools remain explicit for headless users,
without automatically replacing their network manager. Rootfs profiles and
targets consume this package rather than duplicating its payload.

The limited MIT lease-service fork and notices are described in
[`../crates/claw-nearby/PROVENANCE.md`](../crates/claw-nearby/PROVENANCE.md).
[`docs/updating.md`](../docs/updating.md#nearby-sharing-updates) covers checked
retirement and interrupted-window behavior.
