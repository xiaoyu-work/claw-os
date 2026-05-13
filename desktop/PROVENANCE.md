# Provenance

This directory is a **vendored fork** of the COSMIC Desktop Environment
by System76. Code was copied (not submodule-linked) on 2026-05-12, and
**directory names were normalized** to drop the upstream `cosmic-`
prefix. The original `LICENSE` file inside every directory is preserved
verbatim, as required by GPL / MPL / Apache / MIT / CC-BY-SA.

**The COSMIC name and logo are trademarks of System76** (see
`TRADEMARK.md`). Internal binary names, crate names, systemd unit
files, .desktop files, and `com.system76.*` App IDs still carry
upstream identifiers; rename them before any commercial release.

## Vendored components

| Local directory | Upstream repo | Upstream commit | License |
|---|---|---|---|
| `applets/` | github.com/pop-os/cosmic-applets | `89a149034d06` | GPL-3.0 |
| `applibrary/` | github.com/pop-os/cosmic-applibrary | `29972234789b` | GPL-3.0 |
| `bg/` | github.com/pop-os/cosmic-bg | `b1ca4c180ab2` | MPL-2.0 |
| `comp/` | github.com/pop-os/cosmic-comp | `b955789a4e79` | GPL-3.0 |
| `edit/` | github.com/pop-os/cosmic-edit | `7bbe82ec3f2b` | GPL-3.0 |
| `files/` | github.com/pop-os/cosmic-files | `accb9fd41866` | GPL-3.0 |
| `greeter/` | github.com/pop-os/cosmic-greeter | `1047333fbb97` | GPL-3.0 |
| `icons/` | github.com/pop-os/cosmic-icons | `2c697e8e97cf` | CC-BY-SA-4.0 |
| `idle/` | github.com/pop-os/cosmic-idle | `c95d066b5b64` | GPL-3.0 |
| `initial-setup/` | github.com/pop-os/cosmic-initial-setup | `24a9b1ee0d11` | GPL-3.0 |
| `launcher/` | github.com/pop-os/cosmic-launcher | `1e57708e5af9` | GPL-3.0 |
| `launcher-backend/` | github.com/pop-os/launcher | `5b8685107166` | MPL-2.0 |
| `notifications/` | github.com/pop-os/cosmic-notifications | `a899bfbc6715` | GPL-3.0 |
| `osd/` | github.com/pop-os/cosmic-osd | `c57df29816e9` | GPL-3.0 |
| `panel/` | github.com/pop-os/cosmic-panel | `2358f0473bf6` | GPL-3.0 |
| `player/` | github.com/pop-os/cosmic-player | `d1f63c570c76` | GPL-3.0 |
| `protocols/` | github.com/pop-os/cosmic-protocols | `c253ec1d6804` | ? |
| `randr/` | github.com/pop-os/cosmic-randr | `6e8e795970fa` | MPL-2.0 |
| `screenshot/` | github.com/pop-os/cosmic-screenshot | `b917c631d155` | GPL-3.0 |
| `session/` | github.com/pop-os/cosmic-session | `495e591dc659` | GPL-3.0 |
| `settings/` | github.com/pop-os/cosmic-settings | `703a934b096b` | GPL-3.0 |
| `settings-daemon/` | github.com/pop-os/cosmic-settings-daemon | `716da6d6af0b` | GPL-3.0 |
| `simple-wrapper/` | github.com/pop-os/simple-wrapper | `95db0daff42a` | MPL-2.0 |
| `store/` | github.com/pop-os/cosmic-store | `2c705e725e31` | GPL-3.0 |
| `term/` | github.com/pop-os/cosmic-term | `0a7fd0c26bf2` | GPL-3.0 |
| `text/` | github.com/pop-os/cosmic-text | `c24886c2471e` | Apache-2.0 |
| `theme/` | github.com/pop-os/cosmic-theme | `ce3a63a10638` | MPL-2.0 |
| `theme-editor/` | github.com/pop-os/cosmic-theme-editor | `024bd9b3e496` | GPL-3.0 |
| `time/` | github.com/pop-os/cosmic-time | `257aecae2aa4` | MIT |
| `toolkit/` | github.com/pop-os/libcosmic | `4fab6c777dbd` | MPL-2.0 |
| `wallpapers/` | github.com/pop-os/cosmic-wallpapers | `3c59953e7ee5` | CC-BY-SA-4.0 |
| `workspaces/` | github.com/pop-os/cosmic-workspaces-epoch | `cd729d045bd2` | GPL-3.0 |
| `xdg-desktop-portal/` | github.com/pop-os/xdg-desktop-portal-cosmic | `308da48a2790` | GPL-3.0 |

### Nested vendor (`toolkit/`'s own submodules)

`libcosmic` (now `toolkit/`) consumes two upstream sub-trees as path
dependencies. We vendor them in place rather than as git submodules:

| Local path | Upstream repo | Upstream commit | License | Why |
|---|---|---|---|---|
| `toolkit/iced/` | github.com/pop-os/iced | `347d91f7ead2` | MIT | path dep `path = "./iced"` in `toolkit/Cargo.toml` |
| `toolkit/cosmic-icons/` | github.com/pop-os/cosmic-icons | `2c697e8e97cf` | CC-BY-SA-4.0 | `toolkit/build.rs` reads icons from this exact path |

`toolkit/cosmic-icons/` is content-identical to the top-level `icons/`; it
is duplicated because libcosmic's `build.rs` hardcodes the relative
path. If the duplication ever becomes annoying, point `build.rs` at
`../../icons/` and drop one copy.

## Files from cosmic-epoch (meta-repo)

The following files at the root of `desktop/` came from
`github.com/pop-os/cosmic-epoch` commit `ed1607856b42`:

- `justfile` — build orchestration (paths in this file were rewritten
  to match the renamed directories above)
- `TRADEMARK.md` — System76 trademark policy (verbatim, required reading)
- `docs/` — upstream documentation
- `scripts/` — upstream packaging helpers

## Why vendoring (not submodules)

- claw-os will diverge significantly from upstream; submodule overhead
  has no upside when there is no plan to sync back.
- Atomic refactors across the desktop, `cos`, and AI integration layers
  are routine; a monorepo makes them one PR instead of many.
- Single CI, single release tag, single `git clone` for contributors.

## License obligations

This vendored tree contains GPL-3.0 code (most components), MPL-2.0
code (`toolkit/`, `bg/`, etc.), and a few MIT/Apache-2.0 crates.
Distributing any binary built from this tree requires offering the
corresponding source under those terms.
