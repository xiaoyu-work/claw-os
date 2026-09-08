# claw-os desktop

The claw-os desktop environment. Source code is **vendored** (forked from
upstream COSMIC by System76), with directory names normalized to remove the
upstream `cosmic-` prefix. Each component still ships its original `LICENSE`
file and any upstream copyright notices — see `PROVENANCE.md` for the
component → upstream-repo + commit mapping.

> ⚠️ **Rebrand status.** The `com.system76.Cosmic*` App ID prefix has been
> migrated to `com.clawos.*` across `.desktop` / `.metainfo.xml` / D-Bus
> well-known names / GSettings schema names / wayland `app_id`s. Internal
> binary names (`cosmic-comp`, `cosmic-panel`, …), crate names, and systemd
> `.service` file basenames still carry upstream `cosmic-*` identifiers and
> can be renamed in a later pass. The `LICENSE` files inside each directory
> must remain (GPL / MPL / Apache / MIT requirement).

## Layout

```
desktop/
├── justfile                Build orchestrator (just build / just install)
├── TRADEMARK.md            Upstream System76 trademark policy
├── PROVENANCE.md           Origin repo + commit hash + license per directory
├── docs/  scripts/         Upstream packaging helpers
│
├── comp/                   Wayland compositor (kernel of the DE)
├── session/                Session manager; launches the rest
├── greeter/                Display manager (login screen)
├── panel/                  Top / bottom panel (dock + taskbar)
├── launcher-backend/        Shared launcher protocol/service (UI source is external)
├── applets/                Battery / wifi / volume / clock / ...
├── applibrary/             App grid (Launchpad equivalent)
├── workspaces/             Workspaces / Overview
├── bg/                     Wallpaper daemon
├── osd/                    On-screen display (volume/brightness toasts)
├── notifications/          Notification center
├── idle/                   Idle / lock manager
├── randr/                  Multi-monitor control
├── initial-setup/          First-run wizard
│
├── settings-daemon/        Backend for system settings
├── player/                 Media player
│
├── toolkit/                UI toolkit (iced-based, MPL-2.0; upstream libcosmic)
├── protocols/              Custom Wayland protocols
├── text/                   Text shaping (Apache-2.0)
├── theme/                  Theme engine (MPL-2.0)
├── theme-editor/           Theme editor
├── time/                   Animation lib (MIT)
│
├── xdg-desktop-portal/     Screen sharing, file picker portal
├── simple-wrapper/         Misc helper (MPL-2.0)
├── icons/                  Icon set (CC-BY-SA-4.0)
└── wallpapers/             Wallpaper assets (CC-BY-SA-4.0)
```

## Building

The complete [Capture product](https://github.com/xiaoyu-work/clawos-app/tree/main/products/capture)
is built with `just capture-build` from the immutable App pin at
`build/native-apps/cosmic-screenshot`. All original native files, 72 locales,
eight icons, desktop entry, license and release settings move together.
The native client retains its independent ashpd/zbus/Tokio graph; its original
interactive UI is supplied by the unchanged OS desktop portal, not libcosmic.
Non-interactive CLI/MCP share the capability-gated OS capture service; workers
receive no session bus, clipboard access or arbitrary native-launch route.
The signed desktop package retains the executable/descriptor/resources and
depends on Agent's `claw-os-capture-v1`. User screenshots and configuration
are not moved. Private portal/broker fixtures do not claim live Wayland or
full-image acceptance.

The complete [Settings product](https://github.com/xiaoyu-work/clawos-app/tree/main/products/settings)
is built with `just settings-build` from the immutable App pin at
`build/native-apps/cosmic-settings`. The whole workspace, all default pages,
subscriptions, translations, config schemas and original toolkit patches are
preserved. Normal desktop installation uses its original executable/resource
paths. Native MCP offers page discovery, fixed activation and four owner-scoped
permission tools; the eleven manager Apps retain separate grants. Applications
UI and MCP share the OS permission service, but only the independent human UI
can invoke polkit confirmation. Fixed Settings activation uses the authenticated
owner's user service manager with a closed environment, not a child of the
daemon's irreversible `NoNewPrivileges` sandbox. The settings daemon and
privileged providers remain OS-owned. Source relocation does not migrate user
settings or claim interactive Wayland/device acceptance.

The complete [Store product](https://github.com/xiaoyu-work/clawos-app/tree/main/products/store)
is built with `just store-build` from the immutable App pin at
`build/native-apps/cosmic-store`. It retains its original default native graph,
Flatpak/PackageKit UI, flathub-stats workspace, translations and resources.
Native queries share product catalog logic without pkg App dispatch or
transaction authority. The OS retains fixed Store activation, policy/snapshots,
package execution and signed desktop-package ownership. This does not merge
the GUI and MCP catalogs or validate interactive package operations/visuals.

The complete [Terminal product](https://github.com/xiaoyu-work/clawos-app/tree/main/products/terminal)
is built with `just terminal-build` from the immutable App pin at
`build/native-apps/cosmic-term`. It retains its original locked renderer,
file-chooser/toolkit graph, password integration, translations, themes and
packaging. Native UI writes and fixed window activation use controlled OS
services; its MCP shares product command logic without invoking another App.
`cosmic-term` remains a desktop-package identity, separate from `exec` and its
process registry. This source move does not move user histories or PTY state.

The complete [Files product](https://github.com/xiaoyu-work/clawos-app/tree/main/products/files)
is built with `just files-build` from the immutable App pin. Generated source
under `build/native-apps/cosmic-files` retains the original toolkit graph,
file-chooser library, `cosmic-files` and `cosmic-files-applet`, translations and
resources. Normal install preserves both executable paths and desktop-package
ownership. UI/MCP share product libraries and controlled OS services, not App
calls; source relocation does not unify their data or validate visual behavior.

The [Mail product source](https://github.com/xiaoyu-work/clawos-app/tree/main/products/mail)
has moved to `clawos-app`, including Thunderbird provenance and its paired
Firefox platform dependency. Its Mozilla build is independent of this tree
and the COSMIC commands below.

Calendar's complete panel UI, translations, desktop entry and icon also live
in the [Calendar product](https://github.com/xiaoyu-work/clawos-app/tree/main/products/calendar).
`cosmic-applets` remains the shell host and injects the shared OS agenda
provider; neither Calendar nor Widget Rail calls another App.
Clipboard's complete popup, CopyQ history adapter, translations and desktop
entry live in the [Clipboard product](https://github.com/xiaoyu-work/clawos-app/tree/main/products/clipboard).
The shell injects its history-only policy callback through the shared policy
service, without a Widget Rail dependency or selection-grant expansion.
The existing `edit-paste-symbolic` icon and CopyQ behavior are unchanged;
CopyQ history and Wayland selection remain separate backends.
The complete Widget Rail UI, translations, desktop entry and UI tests live in
the [Desktop Widgets product](https://github.com/xiaoyu-work/clawos-app/tree/main/products/desktop-widgets).
The shell injects independent Calendar/task/telemetry callbacks. Shared OS
services retain policy, read-only task collection and telemetry sampling;
Agent Activity's UI and task mutations do not move. The rail keeps its original
visuals, refresh/error states, grants and desktop package identity.
Applet `just build-*` prepares the exact `packaging/apps.lock.json` source
under ignored `build/native-apps` before compiling. Image builds prepare it on
the host and bind the generated inputs into the chroot. The existing toolkit
patches and desktop Debian package ownership are preserved.

The complete [Text Editor product](https://github.com/xiaoyu-work/clawos-app/tree/main/products/editor)
is external too. `just editor-build` materializes the pinned native source
under `build/native-apps/cosmic-edit` and builds it with its original locked
toolkit/file-chooser graph and OS SDK/runtime. Normal desktop install retains
`/usr/bin/cosmic-edit`, `com.clawos.Edit`, localized resources and desktop
package ownership. Its UI and MCP use controlled filesystem/desktop services
and SDK AI; the product no longer invokes the Files, Terminal or Document Apps.

The desktop is built from this tree by `rootfs/features/desktop/install.sh`
as part of `rootfs/build.sh`. Manual local build:

```bash
cd desktop
just build              # ~30-60min on first run
sudo just install rootdir="" prefix=/usr
```

Dependencies (apt names) are declared in `rootfs/features/desktop/packages.txt`.

## Testing

Private-access Rust unit-test bodies live under each crate's `test/unit/`
directory, mirroring its `src/` path. Production source files contain only a
small `cfg(test)` include declaration. Existing Cargo integration tests remain
under crate-level `tests/` directories.

Run tests from the owning component or workspace manifest rather than assuming
the repository root workspace contains desktop crates:

```bash
cargo test --manifest-path desktop/<component>/Cargo.toml -- --test-threads=1
```

For direct applets Cargo commands, prepare native inputs first from the
repository root (repeat after changing the App source lock):

```bash
python3 scripts/app_sources.py --native
cargo test --manifest-path desktop/applets/Cargo.toml -p claw-applet-services --lib
cargo build --manifest-path desktop/applets/Cargo.toml -p cosmic-applets --locked
```

## Modifying

This is **your codebase** — there is no upstream sync. Refactor, rename,
delete components freely. Suggested first moves:

1. Pick one component to learn the toolkit patterns (`panel/` is small)
2. Add a `cos-agent` integration point in the external Launcher product or as a new applet
3. Wire AI features through the `cos` binary via DBus / pipe / subprocess so
   the AI layer stays isolated from GPL-3 propagation

## Where to put the Agent

Recommended: keep the AI logic in `crates/` or `core/` (existing Rust code in
claw-os) as a **separate process**. The desktop talks to it over DBus /
Wayland-protocol / `cos` CLI. This keeps the GPL boundary clean.
