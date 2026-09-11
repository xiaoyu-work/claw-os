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
| `apps.lock.json` | Immutable clawos-app revision, distinct product/capability groups and exact App payload |
| `../scripts/app_sources.py` | Fetch pinned, explicitly kinded sources and stage their package-owned payload |
| `../scripts/app_sources.py --stage-shared <root>` | Invoke App-owned common support staging once into `usr/lib/cos/python`, separately from SDK/runtime and product payloads |
| External Files `python/claw_files/` | Shared document parsing/conversion staged in the Agent package; native Files embeds the same source, and both executables stay desktop-owned |
| `../tools/install-browser-agent.sh` | Manual Browser extension/Native Host deployment from the same product pin |
| `deb/tests/test_app_sources.py` | Immutable source/cache, payload identity, and OS native-launcher ownership |
| `deb/build-desktop-deb.sh`, `deb/tests/test_applet_provider.py` | Require the actual native Applet provider before declaring the Desktop-owned `claw-os-applet-services-v1` interface; no App/GUI grant or publication cutover |
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
| `../crates/claw-display-login/MODULE.md`, `../core/src/display_session/MODULE.md` | Native GNU PAM/display helpers, OS-only Agent-to-Desktop interface, private installation fixture and logout boundary |
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

Migrated products live in `xiaoyu-work/clawos-app`, not a second local App
implementation. `apps.lock.json` pins their published commit. Product-owned
staging builds the Mail XPI and Python payload; OS packaging retains the native
authority launcher and distributes these together in `claw-os-agent`.
External desktop Apps are excluded from Agent staging. `panel-calendar`, `panel-clipboard` and `widget-rail` are
resolved from the same immutable source pin by desktop package assembly;
their complete product-owned native UIs are linked into `cosmic-applets`.
The complete standalone Launcher source/resources are composed from that pin
too; `cosmic-launcher` remains a desktop-package descriptor and executable.
The complete native Editor is composed likewise; `cosmic-edit`, its localized
desktop/metainfo files and icons retain desktop-package ownership. Editor's
locked renderer/file-chooser graph is not replaced by the shell toolkit.
Its Python MCP code is embedded at compilation and imports the packaged
Agent SDK/runtime, already guaranteed by desktop → base → agent dependencies.
`deb/claw-os-desktop/apps.list` remains the package-ownership authority.
The complete native Store source and nested flathub-stats workspace are also
composed from that pin. Store's descriptor, binary, desktop/metainfo files and
icon remain desktop-owned; its read-only native queries do not inherit pkg
transaction grants. Native UI backend/user state is not imported into packages.
The complete nested Settings workspace is composed identically, retaining its
original default pages and toolkit patches. Its binary, page desktop entries,
translations, icons, polkit resources and default config schemas remain in the
desktop package, as does its separate `cosmic-settings` manifest. Neither
Settings Daemon/provider code nor user state moves with this product.
Settings permission management adds the `claw-os-app-permissions-v1` service
dependency from Desktop to Agent, without forcing synchronized package versions.
Its permission and fixed-launch clients consume the explicit-binary SDK
transport paired with the lifecycle-corrected Agent provider. Pin updates must
pass the App revision's product/native CI, including the installed Settings
fixture with sanitized PATH and no `CLAW_COS_BIN`; see [the package contract](README.md)
for durable restoration and independent user-session activation.
Capture's complete native client/resources follow the same immutable pin and
desktop ownership. `claw-os-capture-v1` is an additional Desktop-to-Agent service
dependency, not an App identity or a package-version lockstep. The provider
owns bounded owner-session capture and non-overwriting private output; the
native worker no longer holds the session bus. Screenshots/configuration and
the shared interactive portal are not copied into the product or packages.
Media Player's full native UI/MPRIS/resources and original renderer also follow
the immutable App pin. Its descriptor and executable stay desktop-owned;
`claw-os-media-player-v1` is an additional Desktop-to-Agent service dependency.
The provider controls only the authenticated owner's installed Player and
reads its live state under separate exact grants; no worker bus is granted.
Source/partition tests cover the generated native path, versioned service and
refusal of unsafe binary-only hot-swap.
Notifications' complete native source, private config/util libraries and descriptor
follow the same immutable pin. Desktop owns the binary and manifest and links
the product libraries into its panel/applets; Agent owns the versioned
`claw-os-notifications-v1` durable service. Worker intent retains `ui.notify`
without a session bus, file-icon access, or owner-wide notification control.
Its original installer supplies only the native binary; no fixture executable
or provider source is packaged. Native history/settings and legacy `notify`
JSON are not migrated. Binary-only hot-swap is refused.
The `notify` Python source/descriptor/client comes from the same product pin
but stays exclusively in Agent. It uses that package's SDK/service, not the
native App. The partition is 63 migrated Agent identities plus 12 desktop
identities; all 75 belong to 24 business product groups plus four explicitly
shared-capability source groups. Historical JSON is preserved in its
old namespace and excluded from new service lists, never copied into payloads
or automatically imported at installation/launch.
App partition accounting includes nested gateway manifests. App-owned
`shared/python` supplies `_shared`, `gateway._shared` and `canonical_argv`
through the pinned public common staging CLI. Agent stages them once in
`/usr/lib/cos/python`, separately from SDK/runtime and App payloads, preserving
the required `python3-idna` dependency. Tests/vectors and bytecode are excluded.
No OS `apps/` source or duplicate libraries under the installed Apps root remain.
The current Agent Debian package still owns the compatibility payload;
independent App APT/common-package ownership is a separate coordinated cutover.

Document Engine owns `doc` under the external `capabilities/` root; the lock's
optional `capabilities` list is distinct from `products`. Its explicit named
Files parser dependency ships in Agent alongside Doc even for Doc-only staging.
Identical library co-staging is accepted; conflicting bytes/modes/symlinks or
extra files are refused, not merged. No public SDK/provider or installed
identity/state moves. The App stager excludes tests and bytecode; Agent
assembly no longer reads a local Apps source tree. Source and dependency
caches are left untouched.

Storage SDK supplies independent `db` and `kv` clients from
`capabilities/storage-sdk/apps/<id>`, separate from the Storage business product.
Complete runtime payloads install at `/usr/lib/cos/apps/<id>` in Agent, without
SDK/provider source, native assets or user state. DB's tools, grants and SQLite
namespace are unchanged. KV preserves `$COS_DATA_DIR/kv.json` and exact-key
read/write/delete grants; its list/dump authority correction and persistence
fixes are documented in [updating](../docs/updating.md#app-data-moves-into-per-app-directories).
No App data is imported or joined with Agent memory.
Agent still contains 63 total identities (all externally source-owned);
Desktop still contains 12. The real package-block fixture checks exact identities
and full payload bytes/modes/symlinks, then runs DB/KV manifest-selected MCP over
stdio through installed OS SDK/runtime and App-owned common support. It covers KV restart,
multiple writers, private atomic replacement, corrupt-state errors and namespace
isolation without importing App internals or fixing a list of private filenames.

HTTP supplies `net` from `capabilities/http/apps/net` to Agent only. The full
payload is compared with its immutable source, and the real Agent build-block
fixture invokes the manifest-selected MCP entrypoint through installed OS
libraries. The unchanged client keeps both command shapes and exact needs.
App common support owns `_shared.safe_http`; OS retains policy, egress enforcement and signing;
no provider, SDK, user state or independent updater enters the App payload.

AI Helpers supplies `summarize` from `capabilities/ai-helpers/apps/summarize`
to Agent only, completing all 75 original source identities. Its implementation
and MCP use SDK AI plus OS policy/memory exports; providers, credentials,
consent, budgets, safety, audit and memory storage never enter the payload.
The AI binding correction matches the unchanged runtime check and preserves
the AI consent snapshot, budgets, result shape and `self:summarize` namespace.
Real all/Agent/Desktop and Agent build-block fixtures check exact 75/63/12
identities, full payload bytes/modes/symlinks and public staged MCP/wire with
synthetic responses and private data. No App production or helper source
remains under OS `apps/`. Signed compatibility package delivery and declared
source staging remain distribution/build coupling.

## Tests

```bash
ARCH=amd64 ./packaging/deb/build-debs.sh
python3 -m pytest -q packaging/deb/tests/test_app_sources.py rootfs/features/claw-mail-ai/test_install.py
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
