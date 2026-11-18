# Hyprland Integration

## Responsibility

Own the Claw-specific integration boundary around the complete
[Hyprland source](../hyprland/). The replacement stack uses Hyprland,
Quickshell and the independently source-owned
[Caelestia frontend](https://github.com/xiaoyu-work/clawos-app/tree/main/products/desktop-shell).
It does not replace the system Agent.

Desktop source builds select only the Root-integrated Hyprland executable.
Installed acceptance requires the corresponding Agent package, the signed
Desktop Shell App and a real authenticated graphical session.

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
actual client/listener retirement callbacks. The compositor links this adapter
unconditionally and authenticates its Root control descriptor before creating
the display.
The standalone `CMakeLists.txt` builds this adapter and its actual C++ ABI
test without requiring the complete renderer. It requires the built private
Rust static library through `CLAW_COMPOSITOR_LIBRARY`; there is no
unauthenticated or no-authority build mode.
Linking that bridge requires preserving its GPL terms alongside the original
Hyprland and dependency notices; it does not change the Agent's license.

`Selection.cpp` retains Root-owned sealed selection payloads independently of
the calling GUI. Narrow hooks in Hyprland cover client creation, security-context
listeners, standard/primary/data-control selections, layer-shell, focus grabs
and checked shutdown. Selection and drag-and-drop offers retain distinct roles.
XWayland focus never supplies a selection grant: its unleased clipboard bridge
is refused, independently of drag-and-drop and window rendering.
`Desktop.cpp` handles only the closed Root list/focus/close operations against
native stable window IDs. Neither the App nor the Agent receives a raw
Hyprland control socket. The existing display supervisor forwards authenticated
response descriptors without interpreting them as grants. The
[public window provider](../../core/src/clawd/desktop/windows.rs) uses this
channel, binds opaque IDs to the display epoch and observes effects without
replaying mutations. It provides the native workspace, output and keyboard
presentation model without an App-side Hyprland IPC client.

[`session/`](session/) contains the downstream session entry and user target.
The installed subscriber's `--desktop` mode attaches to the Root login binding, starts the existing
owner FileManager1 facade and invoke `cos app desktop-shell --gui`. Normal
signed App review and capability decisions still apply, and the launcher does
not start an Agent task or a second compositor. `stage-desktop.sh` installs
the session wiring and compiled compositor into the Desktop package.
The installed `/etc/xdg/hypr/hyprland.lua` supplies monitor discovery and
native window/workspace bindings for a new owner. It does not launch a
second panel, wallpaper process or terminal. Existing owner configuration
continues to take precedence.

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
This establishes source completeness. The native build additionally compiles
and links the complete compositor and `hyprctl`; the actual compositor refuses
startup without Root display activation. These checks do not establish an
authenticated rendered desktop session.
The six-input activation-only mode of the existing private PAM runner now
passes with the actual Hyprland executable on a temporary VKMS device. A
signature-verified Ubuntu Base 26.04.1 runtime supplies the matching userspace
inside private mount/network/IPC/UTS namespaces on Linux 6.17; the host system
and its login configuration are not upgraded or replaced. A chroot used this
way must itself be a bind mount before entering it, so the runner can make its
root mount recursively private.

The real PAM/kernel login, protected supervisor, native compositor readiness
and checked logout/workload retirement passed. The fixture-installed compositor
was byte-identical to the complete native build. Its helper cgroups were empty
and removed, and the temporary VKMS module was unloaded. The render-node-only
WSL trial remains insufficient for this acceptance.

This activation-only run does not capture rendered frames or establish signed
GUI App, XWayland startup, physical-display/greetd/logind or installed-package
acceptance. A build or an authority bypass cannot substitute for those checks.

The existing protocol baseline runs from the OS repository root:

```bash
cargo test -p claw-display-control --locked -- --test-threads=1
```

Native build, actual isolated GUI/display acceptance and package/update checks
remain mandatory before production activation changes.

The selected source builds with GCC 15. Two narrow C++ library substitutions
retain allocation-free case-insensitive prefix comparison and string slicing
without requiring newer `ranges::starts_with` or `string::subview` APIs.

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
The scanner is pinned with Wayland 1.25.0 because the selected protocol XML
requires its DTD support. Hyprwire uses the revision selected by the
compositor's own `flake.lock`, including its native protocol scanner.
The same lock supplies Hyprwayland Scanner 0.4.6, built before Aquamarine.
The native CMake builds retain pkg-config's ordinary system-directory
filtering so pinned `.pc` headers also select the matching library. Standard
Debian libraries are still resolved by their normal linker paths.
Aquamarine uses that same compositor lock's 0.16.0 revision: the older 0.15.0
release lacks the output bit-depth API used by this compositor.

From the OS repository root, after installing the declared development tools
and libraries:

```bash
python3 desktop/hyprland-claw/build-dependencies.py \
  --cache "$HOME/.cache/claw-desktop-native"
```

The build requires Git, CMake, Ninja, Meson, pkg-config, a C++26 compiler and
the development libraries declared by the upstream manifests. For Debian 13,
`stage-desktop.sh` uses the pinned official GCC 15.3 Trixie build:
`build-toolchain.py` verifies and extracts only its compiler layer. Debian's
GCC 14 standard library does not support Hyprwire's `append_range`.
Only the matching private C++ runtime enters the Desktop payload; neither
the compiler nor the system's C++ libraries are replaced. Distribution
packages include glslang tools/development files, Wayland, DRM/GBM, libinput,
libseat, libdisplay-info, Cairo/Pango, libeis, Lua 5.5, librsvg, libzip and
tomlplusplus. Source caches retain licenses and refuse changed tracked files.
The output is a development dependency prefix, not an installed or signed
OS payload; release builds must still use the target Debian ABI.
