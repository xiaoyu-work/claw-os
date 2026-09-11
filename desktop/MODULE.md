# Desktop Module

## Purpose

`desktop/` is the Claw OS desktop product fork, composed of independent COSMIC
component crates plus Claw-specific agent bridges and applets.

## Responsibilities

- Provide compositor, session, panel, launcher, settings, desktop apps, and UI
  toolkit components.
- Preserve component licensing and provenance while evolving the product fork.
- Keep privileged/AI implementation behind stable core process boundaries.
- Build each component through its own Cargo or just workspace.

## Key Files

| Path | Role |
| --- | --- |
| `README.md` | Component map, build instructions, product-fork boundary |
| `PROVENANCE.md` | Upstream origin and revision per component |
| `justfile` | Desktop build/install orchestration |
| `applets/claw-applet-services/` | Shared policy, read-only Calendar/task-list providers and system telemetry; no UI/App dependencies |
| `../scripts/app_sources.py --native` | Materialize product-declared native libraries, workspaces and assets under ignored build storage before direct Cargo use |
| `launcher-backend/` | Shared launcher library/service; native frontend is owned by external `clawos-app/products/launcher` |
| `applets/cosmic-applets/` | Host native libraries; inject Calendar agenda, Clipboard history-policy and Widget Rail typed data callbacks, and generate product-owned desktop entries |
| `agent/` | Native agent bridge and UI |
| `agent/bridge/src/notifications.rs` | Single desktop delivery consumer; installed owner/executable-bound presenter, safe plain-text projection, connection-local handles and genuine user state transitions |
| `agent/protocol/` | Versioned desktop Agent HTTP/SSE presentation contract |
| `agent/ui/MODULE.md` | Agent UI state ownership, effects, views, and test boundaries |
| `comp/`, `session/`, `panel/` | Shell/compositor/session surfaces |
| `settings-daemon/` | OS settings services and config schemas; native UI source is external |
| `toolkit/`, `text/`, `theme/` | Shared UI/rendering foundations |
| [`clawos-app/products/mail`](https://github.com/xiaoyu-work/clawos-app/tree/main/products/mail) | External Mail product source and paired Firefox build dependency; not part of the COSMIC build |
| [external Editor](https://github.com/xiaoyu-work/clawos-app/tree/main/products/editor) | Complete native UI/MCP/resources; controlled filesystem/desktop and SDK AI remain OS-provided; build with `just editor-build` |
| [external Files](https://github.com/xiaoyu-work/clawos-app/tree/main/products/files) | Complete native UI/library/companion and shared business sources; build both executables with `just files-build`; OS keeps authority and index service |
| [external Terminal](https://github.com/xiaoyu-work/clawos-app/tree/main/products/terminal) | Complete native terminal UI/MCP/resources; `just terminal-build`; OS keeps snapshots, fixed desktop activation and process authority |
| [external Store](https://github.com/xiaoyu-work/clawos-app/tree/main/products/store) | Complete native Store UI/MCP/resources and flathub-stats workspace; `just store-build`; OS keeps fixed activation, policy and package transaction authority |
| [external Settings](https://github.com/xiaoyu-work/clawos-app/tree/main/products/settings) | Complete nested Settings workspace/UI/MCP/resources; `just settings-build`; OS keeps services and fixed activation with the original separate grant |
| [external Capture](https://github.com/xiaoyu-work/clawos-app/tree/main/products/capture) | Complete native portal client/MCP/resources; `just capture-build`; OS retains the shared interactive portal, session/capture authority and durable output |
| [external Media Player](https://github.com/xiaoyu-work/clawos-app/tree/main/products/media-player) | Complete native UI/MPRIS/MCP/resources; `just player-build`; shared live native state, with exact owner-bound observation/control authority retained by OS |
| [external Notifications](https://github.com/xiaoyu-work/clawos-app/tree/main/products/notifications) | Complete Layer Shell UI/MCP/config/util/build; `just notifications-build`; authoritative owner/source state and the single delivery consumer stay OS-owned |

## Dependencies

Desktop processes communicate with core through stable CLI, HTTP/SSE, DBus,
Wayland, SDK, or MCP boundaries. Preserve licenses and avoid pulling privileged
agent logic into GPL desktop processes. Component workspaces remain independent
of the root Rust workspace.

Notifications config/util crates remain private native component inputs,
linked by the current `applets/cosmic-applet-notifications` and `panel/cosmic-panel-bin`
from `build/native-apps/cosmic-notifications`. They are no longer declared
`native_libraries` exports; preparation keeps the full component without
inventing public metadata. Their manual and chroot builds
prepare/validate the same immutable inputs. The original daemon graph is built
separately, not unified with the applet renderer. Connection-local handles and
popup retirement are presentation, never a second durable notification store.
Legacy `notify` JSON and all local settings remain separate.

`just player-build` prepares `build/native-apps/cosmic-player` from the same
immutable pin, preserving its original GStreamer/libcosmic renderer, lock and
optional features. UI/MCP share native playback, not a second cache or an
arbitrary external MPRIS player. Player MCP has no desktop transport and uses
the fixed scoped OS media service. Its binary, descriptor and resources remain
desktop-owned; native process/fixture tests are product-owned, and OS tests
cover owner/executable identity, grants, worker relay and dispatch gating.

`just launcher-build` prepares and builds the immutable external native
Launcher under `build/native-apps/cosmic-launcher`, linking OS toolkit,
SDK/runtime and launcher-backend dependencies. Normal desktop build/install
uses the same justfile and installs `/usr/bin/cosmic-launcher` for the packaged
native descriptor. Its compiled-in product MCP backend uses the OS Python
SDK and typed desktop launch service, never another App.

`just editor-build` prepares `build/native-apps/cosmic-edit` from the same pin.
It preserves the Editor's existing locked upstream toolkit/file-chooser
dependencies and resolves only SDK/runtime from this OS checkout. Normal
build/install keeps its executable, desktop identity, resources and grants.
The product's unit/process tests own descriptor/resource and UI/MCP coverage;
OS tests retain filesystem, AI and fixed desktop-target authority coverage.

`just terminal-build` prepares `build/native-apps/cosmic-term` from the same
immutable pin. Its native MCP embeds product-owned bounded command/PATH logic
while OS policy and the native Host sandbox retain authority. New Window and
MCP launch use the fixed OS Terminal target under `proc.spawn:cosmic-term`.
Interactive PTYs, native configuration and the separate `exec` registry are
not consolidated or copied. Original native renderer/toolkit dependencies,
licenses, resources and installed executable/desktop IDs remain unchanged.

Bundled apps launch Ask Claw through `cos_runtime::ask_claw`, with only typed
app-specific context adapters in their local `claw_glue` modules. The runtime
owns serialization bounds, executable discovery, activation arguments,
anonymous bounded stdin forwarding, supervised process errors, and the
activation type consumed by the Agent UI. Process argv, audit records,
registry entries, environment, and files contain no context payload.
Context-bearing launches use transient overlay instances rather than the
unauthenticated single-instance D-Bus activation path.

The Agent UI and bridge share `agent/protocol/`; endpoint and stream DTOs must
not be duplicated in either binary. Core/clawd models terminate at the bridge's
anti-corruption translation layer and never become UI state.

## Tests

Each desktop crate stores private-access unit-test bodies under its own
`test/unit/` tree, mirroring `src/`. Cargo integration tests remain in the
standard crate-level `tests/` directory.

Use the owning component/workspace manifest:

```bash
cargo test --manifest-path desktop/<component>/Cargo.toml -- --test-threads=1
```
