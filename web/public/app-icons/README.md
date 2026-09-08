# Claw OS application icons

These are the same first-party icons shipped by the native Claw OS desktop:

| Web asset | Source |
| --- | --- |
| `agent.svg` | `rootfs/features/desktop/overlay/usr/share/icons/hicolor/scalable/apps/clawos-agent.svg` |
| `files.svg` | `desktop/files/res/icons/hicolor/scalable/apps/com.clawos.Files.svg` |
| `edit.svg` | [Editor product native resource](https://github.com/xiaoyu-work/clawos-app/blob/main/products/editor/native/cosmic-edit/res/icons/hicolor/scalable/apps/com.clawos.Edit.svg), pinned by `packaging/apps.lock.json` |
| `store.svg` | `clawos-app/products/store/native/cosmic-store/res/icons/hicolor/scalable/apps/com.clawos.Store.svg` (external source pinned by `packaging/apps.lock.json`) |
| `settings.svg` | [Settings product icon](https://github.com/xiaoyu-work/clawos-app/blob/main/products/settings/native/cosmic-settings/resources/icons/scalable/apps/com.clawos.Settings.svg) |
| `player.svg` | `desktop/player/res/icons/hicolor/scalable/apps/com.clawos.Player.svg` |
| `screenshot.svg` | [Capture product icon](https://github.com/xiaoyu-work/clawos-app/blob/main/products/capture/native/cosmic-screenshot/resources/icons/hicolor/scalable/apps/com.clawos.Screenshot.svg), pinned by `packaging/apps.lock.json` |

The Agent icon follows the repository Apache-2.0 license. The other icons keep
the GPL-3.0 terms and provenance of their owning desktop/product components;
see `desktop/PROVENANCE.md` for their source ownership.
