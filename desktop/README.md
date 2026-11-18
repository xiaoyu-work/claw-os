# Claw OS desktop

Claw OS uses [Hyprland](hyprland-claw/MODULE.md) with the signed
[Desktop Shell App](https://github.com/xiaoyu-work/clawos-app/tree/main/products/desktop-shell),
which owns Quickshell, Caelestia, and their private runtime. Desktop builds
require that App from the authenticated package channel. They do not build or
install another desktop if it is unavailable.

The OS owns greetd/PAM login, display and App authority, system services,
and the native Agent interface. The Agent's terminal and Web use do not
depend on the desktop. Existing Apps keep their identities and shared
toolkit without requiring the previous shell.

- [Build a VM image](../docs/building-desktop.md)
- [Build an Azure image](../docs/building-azure.md)
- [Source origins and licenses](PROVENANCE.md)
- [Installed login and updates](../core/src/display_session/MODULE.md)
