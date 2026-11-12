# Scripts Module

## Purpose

`scripts/` contains repository maintenance, installation, image identity, and
shared build helpers.

## Responsibilities

- Provide reusable target/rootfs helper functions.
- Keep architecture, image profile, package version, and identity logic
  centralized.
- Fail explicitly on unsupported hosts or unsafe state.

## Key Files

| Path | Role |
| --- | --- |
| `lib/image-profiles.sh` | Target feature-set source of truth |
| `lib/arch.sh` | Architecture/target mapping |
| `lib/package-version.sh` | Monotonic Debian package version |
| `lib/image-identity.sh` | Image user/identity assertions |
| `lib/git-readonly.sh` | Read-only Git wrapper for privileged builds |
| `ci/check-provisioning.sh` | Shell syntax and LF checks shared with the test workflow |
| `ci/test-boundaries.sh` | App bind, Calendar, extension/owner-memory isolation, authenticated display and Applet kernel checks; invoked as separate visible CI steps |
| `ci/native_editor_payload.py` | Editor-only private acceptance input: resolve one immutable App snapshot and invoke its public-pin, original-installer native build; no local platform, signing, installation or publication |
| `ci/prepare-release.sh` | Freeze package version and authenticated App APT inputs; only `all` separately freezes an immutable test-only source fixture |
| `ci/import-signing-key.sh` | Require the configured signing identity and export its exact verification keyring through `GITHUB_ENV` |
| `ci/verify-package.sh` | Check package identity, payload-free OS ownership, authenticated App APT inputs and signed release manifest before upload or images |
| `ci/test-agent-package.sh` | Agent TUI and Ubuntu install/reinstall, terminal/Web polkit/PAM, broker and cgroup checks; `installed-core` validates the exact Agent/Base pair in one transaction |
| `app_platform.py` | Reproducible, versioned SDK/runtime/toolkit development artifact; excludes App helpers and OS providers |
| `app_packages.py` | Reauthenticate official fresh App APT indexes and final `.deb` bytes; select independent package closure and enforce one-owner OS staging |
| `app_sources.py` | Prepare explicit immutable App test/development fixtures; never a production package fallback |
| `app_sources.py --write-lock <path>` | Record the selected SHA for later build steps through `CLAW_APP_SOURCE_LOCK`; never changes the repository selection |
| `app_sources.py --stage-shared <root>` | Forward to the selected snapshot's public App common staging CLI; stage only App-owned support under `usr/lib/cos/python` |
| `app_sources.py --native` | Refresh product-declared native inputs and record their source snapshot alongside stable ignored build paths |

## Dependencies

Rootfs, targets, and packaging source these helpers rather than reimplementing
their logic. Scripts must be LF-only, non-interactive in CI, and safe when run
under sudo.

CI scripts run from `GITHUB_WORKSPACE` and use `RUNNER_TEMP` for private scratch
state. Release preparation writes outputs through `GITHUB_OUTPUT`; signing-key
import writes only paths and key identifiers through `GITHUB_ENV`, never the
private key or passphrase. Desktop rootfs builds inherit signing environment
variables through `sudo --preserve-env`, not process arguments.

## Tests

```bash
bash -n scripts/*.sh scripts/lib/*.sh
python3 -m pytest -q scripts/tests/test_app_platform.py
python3 -m pytest -q packaging/deb/tests/test_app_sources.py
python3 -B -m pytest -q scripts/tests/test_native_editor_payload.py
```

Also run the narrowest consuming target/package command.

The proposed `test-boundaries.sh native-editor` selection is a separate real
product lane, not another native probe. Its preparation calls the resolved App
snapshot's `tools/native_build.py editor build --release --app-root` as an
ordinary build user. The complete payload inventory, source SHA, public platform
pin and original licenses/resources remain bound to the private fixture receipt.
Only the existing isolated PAM/display bootstrap signs and runs that payload.
Run its Root acceptance only on a clean disposable GHA runner, never a shared
developer host. Parser tests, compilation and ELF preparation alone are not a
passing native window or file-open result.

Production package/release/rootfs paths consume `app_packages.py`, not
`app_sources.py`. Saved APT evidence is signature/freshness/hash checked again,
App support/named libraries have separate dpkg owners, and OS staging refuses
their payloads. Runtime-v1 implementation version comes from the platform
metadata; this is not an unpublished platform/App pin adoption.
Desktop private UI/config/util links are removed; the unchanged source guard
rejects their reintroduction. Actual native/installed acceptance remains gated.

The remaining source-staging documentation describes explicit fixtures.
`--stage <root>` validates all locked App identities; `--package agent` or
`--package desktop` preserves the explicit Debian package partition.
`--app-path <id>` resolves a single source directory for desktop package
assembly. None of these commands fetches application code at runtime.
`--stage-shared <root>` is separate from product/native/source selectors. It
requires the same exact clean source pin, invokes public
`tools/stage.py --shared --root <root>` and verifies its declared Python-root result. App
support is never sourced from OS `apps/` or duplicated beside App payloads.
Source fixtures call it separately; production Agent stages only its own
SDK/runtime, and independent App APT support/library ownership is separate.
The version-2 selection tracks `main` without a fixed revision and retains
nonempty `products`/`apps` lists plus optional `capabilities`. Fresh preparation
must resolve the branch even when a cache exists; failure never selects stale
source. Resolved version-1 snapshots keep one exact commit across package
steps, and remain usable for explicit build reproduction. They must match the
configured repository and ownership lists. Group names are unique across kinds. Each kind
resolves only its declared root and matching package metadata; missing sources,
duplicate identities, escaping paths and unlocked Python-library owners fail.
`doc` resolves to `capabilities/document-engine/apps/doc`, never `apps/doc`.
`net` resolves to `capabilities/http/apps/net`; App-owned common HTTP support
and OS policy/transport remain separate runtime inputs, not copied into its payload.
`summarize` resolves to `capabilities/ai-helpers/apps/summarize`; the complete
client comes from the App pin while AI/policy/memory authority stays OS-owned.
The lock now selects 63 retained identities (51 Agent, 12 desktop) after the
eleven messaging connectors and legacy Email Gateway were retired, with no
App production or helper source under local `apps/`.
`db` and `kv` resolve to `capabilities/storage-sdk/apps/<id>`, never their deleted
local sources or the distinct Storage business product. The source kind changes
no installed identity, grant ownership or data partition.
Native preparation validates capability metadata but composes only products,
so shared clients cannot invent desktop/native package ownership.
Native preparation validates all declared names and product-local source paths
before replacement, rejects duplicate exports and only replaces exact declared
component paths; unrelated build caches are preserved.
Nested `native_libraries` exports require exact Cargo identity and a path
inside their declared product component. Duplicate names and escapes are
rejected before replacement. `native-libraries.json` records the resolved
library paths and full source revision for consumers/assembly validation.
`source-lock.json` retains the same resolved selection for desktop packaging.
Notifications' private config/util crates remain whole-component App fixture
inputs, not OS applet/panel dependencies or public library exports.
The original daemon and all its build inputs are composed, not recreated under
the deleted production `desktop/notifications` path.
Its `notify` Python facade is staged separately into Agent, never into the
native/Desktop payload. No `apps/notify` production copy or history-import
step is recreated by source preparation.
Media Player is refused by the binary-only VMware hot-swap helper: its signed
manifest and versioned Agent service must advance through paired package updates.
Notifications is refused for the same reason and also requires its matching
native presenter/desktop bridge to restart together.
