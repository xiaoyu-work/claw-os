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
├── launcher/               Spotlight-style command launcher (UI)
├── launcher-backend/       Launcher search backend (pop-launcher upstream)
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
├── files/                  File manager
├── edit/                   Text editor
├── term/                   Terminal
├── store/                  App store
├── settings/               System settings
├── settings-daemon/        Backend for system settings
├── player/                 Media player
├── screenshot/             Screenshot tool
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

## Modifying

This is **your codebase** — there is no upstream sync. Refactor, rename,
delete components freely. Suggested first moves:

1. Pick one component to learn the toolkit patterns (`panel/` is small)
2. Add a `cos-agent` integration point in `launcher/` or as a new applet
3. Wire AI features through the `cos` binary via DBus / pipe / subprocess so
   the AI layer stays isolated from GPL-3 propagation

## Where to put the Agent

Recommended: keep the AI logic in `crates/` or `core/` (existing Rust code in
claw-os) as a **separate process**. The desktop talks to it over DBus /
Wayland-protocol / `cos` CLI. This keeps the GPL boundary clean.
