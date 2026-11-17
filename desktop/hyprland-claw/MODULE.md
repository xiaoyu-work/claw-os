# Hyprland Integration

## Responsibility

Own the Claw-specific integration boundary around the complete
[Hyprland source](../hyprland/). The replacement stack uses Hyprland,
Quickshell and the independently source-owned
[Caelestia frontend](https://github.com/xiaoyu-work/clawos-app/tree/main/products/desktop-shell).
It does not replace the system Agent.

The current change imports the source baseline only. The existing Desktop
build and authenticated display activation have not switched to it.
Upstream executables and installation scripts are not a production launch
path until the existing display and GUI authority contracts are preserved
and verified.

## Source ownership

[`hyprland-source.lock.json`](../hyprland-source.lock.json) records the exact
upstream commit and original Git tree. Its two pinned Git submodules are
expanded into complete ordinary source directories under `subprojects/`.
No nested Git repository or runtime submodule download is required.
Upstream code, file modes, licenses and notices are retained; existing
upstream whitespace is not reformatted during import.

Claw-specific integration belongs here or in the owning OS definition/provider,
not in the App repository. Any hooks added to upstream compositor files
must remain narrow and preserve the original source license.

The [native authority bridge](../../crates/claw-compositor/MODULE.md) now
provides a GPL-3.0-only static library and the C ABI in
`include/claw_compositor.h`. `Claw.cpp` implements the native Wayland client
watcher, Root-origin assignment, standard-global filtering, lease dispatch and
actual client/listener retirement callbacks. It is compiled and linked in the
isolated ABI test, but not yet hooked into the upstream compositor.
The standalone `CMakeLists.txt` builds this adapter and its actual C++ ABI
test without requiring the complete renderer. It requires the built private
Rust static library through `CLAW_COMPOSITOR_LIBRARY`; there is no
unauthenticated or no-authority build mode.
Linking that bridge requires preserving its GPL terms alongside the original
Hyprland and dependency notices; it does not change the Agent's license.

## Required contracts

- Preserve the [Root display/PAM lifetime](../../core/src/display_session/MODULE.md)
  and the closed [display control protocol](../../crates/claw-display-control/MODULE.md).
- Preserve the [GUI Host](../../core/src/clawd/gui/MODULE.md), exact instance
  identity, independent clipboard read/write and panel rights, expiry and
  checked client/listener retirement.
- Keep window/display services behind the existing broker capability and audit
  boundary. Standard Wayland security-context labels are not grants.
- Keep headless Agent and CLI use independent of a display.
- Consume the desktop frontend through the existing signed App packaging and
  runtime contracts; do not activate mutable source or create an App exemption.

## Validation

The import checks the original Hyprland Git tree before expanding its
submodules, then checks each expanded source against its pinned Git tree.
This establishes source completeness, not a successful compositor build or
an authenticated desktop session.

The existing protocol baseline runs from the OS repository root:

```bash
cargo test -p claw-display-control --locked -- --test-threads=1
```

Native build, actual isolated GUI/display acceptance and package/update checks
remain mandatory before production activation changes.

Windows Git subtree exports must explicitly preserve Linux line endings:
`git -c core.autocrlf=false -c core.eol=lf archive`. A subtree archive uses
its own attributes rather than inheriting the repository root's `eol=lf`;
an implicit Windows checkout conversion makes the upstream shell generators
unexecutable on Linux.

## Pinned native dependencies

The compositor requires newer Hypr libraries and Wayland protocol XML than
the distribution baseline. `dependencies.lock.json` records exact source
commits in build order. `build-dependencies.py` builds only those commits
into an explicitly selected external Linux cache; it never installs into the
host system or chooses an older dependency.

From the OS repository root, after installing the declared development tools
and libraries:

```bash
python3 desktop/hyprland-claw/build-dependencies.py \
  --cache "$HOME/.cache/claw-desktop-native"
```

The build requires Git, CMake, Ninja, Meson, pkg-config, a C++26 compiler and
the development libraries declared by the upstream manifests. Distribution
packages include glslang tools/development files, Wayland, DRM/GBM, libinput,
libseat, libdisplay-info, Cairo/Pango, libeis, Lua 5.5, librsvg, libzip and
tomlplusplus. Source caches retain licenses and refuse changed tracked files.
The output is a development dependency prefix, not an installed or signed
OS payload; release builds must still use the target Debian ABI.
