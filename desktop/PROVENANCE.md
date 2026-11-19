# Desktop source provenance

[`hyprland/`](hyprland/) is the complete BSD-3-Clause
[`hyprwm/Hyprland`](https://github.com/hyprwm/Hyprland) source at
`f3e076844484255b77629acc5c75379817c405a8`, original tree
`85b7b316d0b4104f23b1a7e73b9def3564b9b6db`. The
[`source lock`](hyprland-source.lock.json) records the expanded Tracy and
udis86 sources and their exact original trees. The imported `.gitmodules`
is provenance, not a Claw OS submodule dependency.

The [GPL-3.0-only Root authority adapter](hyprland-claw/MODULE.md) is linked
unconditionally. Native dependency revisions and archive hashes are recorded
in [`dependencies.lock.json`](hyprland-claw/dependencies.lock.json).
[`toolchain.lock.json`](hyprland-claw/toolchain.lock.json) pins the GCC
compiler layer of the Docker Official Image for each native architecture.
The installed private C++ runtime carries the original GCC license and
runtime exception; its compiler is not included.
The [Desktop Shell App](https://github.com/xiaoyu-work/clawos-app/tree/main/products/desktop-shell)
records its own sources and licenses. No Omarchy source is used.

The independent native locker builds the MIT-licensed Swaylock 1.8.6
source pinned in [`build-locker.sh`](hyprland-claw/build-locker.sh). Its
original notice ships with the binary. The local patch requires the
dedicated PAM service for unlock and removes non-PAM recovery paths.

The [original Claw OS mountain wallpaper](../assets/brand/wallpaper/NOTICE)
is restored unchanged for setup and sign-in under its existing CC BY-SA 4.0
license. No COSMIC greeter or compositor code is used for these pages.

## Shared App framework

These are retained to support existing standalone products and the Agent UI.
They do not provide a compositor, login, or desktop session.

| Directory | Upstream | Revision | License |
|---|---|---|---|
| `toolkit/` | pop-os/libcosmic | `4fab6c777dbd` | MPL-2.0 |
| `toolkit/iced/` | pop-os/iced | `347d91f7ead2` | MIT |
| `icons/`, `toolkit/cosmic-icons/` | pop-os/cosmic-icons | `2c697e8e97cf` | CC-BY-SA-4.0 |
| `text/` | pop-os/cosmic-text | `c24886c2471e` | Apache-2.0 |
| `theme/` | pop-os/cosmic-theme | `ce3a63a10638` | MPL-2.0 |
| `time/` | pop-os/cosmic-time | `257aecae2aa4` | MIT |
| `launcher-backend/` | pop-os/launcher | `5b8685107166` | MPL-2.0; binary terms in its manifest |

The forked [Tela icons](icons-tela/README.md) keep their original license.
System76's trademark notice remains in [TRADEMARK.md](TRADEMARK.md).
The [shared Applet services](services/claw-applet-services/MODULE.md) were
extracted from the former applet host under their original GPL terms.
The installed Settings/AccountsService policy from the `703a934b096b`
Settings fork retains its GPL-3.0-only license in the Desktop package.
The relocated [Smithay protocol fixture](../crates/claw-display-login/test/vendor/README.md)
retains its own upstream pin and MIT notice; no Desktop package builds it.
