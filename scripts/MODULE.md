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
| `app_platform.py` | Reproducible, versioned SDK/runtime/toolkit development artifact; excludes App helpers and OS providers |
| `app_sources.py` | Resolve the immutable App repository pin and stage explicitly kinded product/capability assets |
| `app_sources.py --stage-shared <root>` | Forward to the pinned public App common staging CLI; stage only App-owned support under `usr/lib/cos/python` |
| `app_sources.py --native` | Validate and refresh product-declared native libraries/assets, standalone Launcher/Editor/Files/Terminal/Store/Capture/Media Player/Notifications and nested Settings workspace at stable ignored build paths from the immutable pin |

## Dependencies

Rootfs, targets, and packaging source these helpers rather than reimplementing
their logic. Scripts must be LF-only, non-interactive in CI, and safe when run
under sudo.

## Tests

```bash
bash -n scripts/*.sh scripts/lib/*.sh
python3 -m pytest -q scripts/tests/test_app_platform.py
python3 -m pytest -q packaging/deb/tests/test_app_sources.py
```

Also run the narrowest consuming target/package command.

`--stage <root>` validates all locked App identities; `--package agent` or
`--package desktop` preserves the explicit Debian package partition.
`--app-path <id>` resolves a single source directory for desktop package
assembly. None of these commands fetches application code at runtime.
`--stage-shared <root>` is separate from product/native/source selectors. It
requires the same exact clean source pin, invokes public
`tools/stage.py --shared --root <root>` and verifies its declared Python-root result. App
support is never sourced from OS `apps/` or duplicated beside App payloads.
The current Agent compatibility build calls it once; SDK/runtime staging and
future independent App APT ownership remain separate contracts.
The version-1 lock retains nonempty `products`/`apps` lists and accepts an
optional `capabilities` list. Group names are unique across kinds. Each kind
resolves only its declared root and matching package metadata; missing sources,
duplicate identities, escaping paths and unlocked Python-library owners fail.
`doc` resolves to `capabilities/document-engine/apps/doc`, never `apps/doc`.
`net` resolves to `capabilities/http/apps/net`; App-owned common HTTP support
and OS policy/transport remain separate runtime inputs, not copied into its payload.
`summarize` resolves to `capabilities/ai-helpers/apps/summarize`; the complete
client comes from the App pin while AI/policy/memory authority stays OS-owned.
The lock now covers all 75 original identities (63 Agent, 12 desktop), with no
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
Notifications' private config/util crates remain whole-component build inputs
for the current OS applet/panel links, not declared public library exports.
The original daemon and all its build inputs are composed, not recreated under
the deleted production `desktop/notifications` path.
Its `notify` Python facade is staged separately into Agent, never into the
native/Desktop payload. No `apps/notify` production copy or history-import
step is recreated by source preparation.
Media Player is refused by the binary-only VMware hot-swap helper: its signed
manifest and versioned Agent service must advance through paired package updates.
Notifications is refused for the same reason and also requires its matching
native presenter/desktop bridge to restart together.
