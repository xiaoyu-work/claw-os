# claw-icons-whitesur

Vendors [vinceliuice/WhiteSur-icon-theme](https://github.com/vinceliuice/WhiteSur-icon-theme)
(GPL-3.0-or-later) as a git submodule under `../icons-whitesur/` and wraps
its `install.sh` in a justfile + debian/ packaging so it can be built as a
`.deb` alongside `cosmic-icons`.

## Why

claw-os ships with the `Cosmic` icon theme, which inherits via:

    Inherits=WhiteSur,Pop,hicolor   # see ../icons/index.theme

so any icon name (e.g. `go-previous-symbolic`, `view-grid-symbolic`,
`folder`, mime icons, …) that's not found in `Cosmic` falls through to
WhiteSur before hitting Pop's defaults. Result: file managers, toolbars
and the system tray all render with macOS Big Sur–style glyphs without
per-app code changes.

## Layout

    desktop/icons-whitesur/         submodule (upstream sources, ~60 MB)
    desktop/icons-whitesur-pkg/     this wrapper (justfile + debian/)

The submodule is *not* fetched by default. Initialise once after cloning:

    git submodule update --init desktop/icons-whitesur

## Build

    just install rootdir=/tmp/stage prefix=/usr

Calls upstream `install.sh -d /tmp/stage/usr/share/icons -n WhiteSur`,
which installs the default blue variant in light + dark to:

    /usr/share/icons/WhiteSur
    /usr/share/icons/WhiteSur-light
    /usr/share/icons/WhiteSur-dark

## Updating

Bump the submodule pointer:

    cd desktop/icons-whitesur
    git fetch && git checkout <new-sha>
    cd ../..
    git add desktop/icons-whitesur
    git commit -m "icons-whitesur: bump to <new-sha>"

Then bump `debian/changelog` in this directory.

## License

GPL-3.0-or-later (matches upstream). See `debian/copyright`.
