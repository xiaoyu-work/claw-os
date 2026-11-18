# Desktop Module

The OS builds the Root-managed compositor, system login and the graphical
Agent. Quickshell and Caelestia belong to the independently signed Desktop
Shell App, which uses the common GUI Host and broker. There is no alternative
desktop session installed by this tree.

| Path | Responsibility |
| --- | --- |
| `hyprland/`, `hyprland-claw/` | Compositor, private Root adapter, target-native staging, and session assets |
| [`agent/`](agent/README.md) | Native Agent client and bridge; the system Agent stays in `core/` |
| [`services/claw-applet-services/`](services/claw-applet-services/MODULE.md) | Read-only Calendar/telemetry and existing App policy client, no desktop UI |
| `toolkit/`, `text/`, `theme/`, `time/`, `icons/`, `icons-tela/` | Shared rendering/toolkit libraries and assets for existing Apps |
| `launcher-backend/` | Retained shared App Launcher engine, not a desktop session |

Build the package using the [desktop image instructions](../docs/building-desktop.md).
Test-only Wayland protocol fixtures now live under
[`crates/claw-display-login/test/`](../crates/claw-display-login/test/); they
are not graphical-session backends.
