# claw-icons-whitesur

Vendors [vinceliuice/WhiteSur-icon-theme](https://github.com/vinceliuice/WhiteSur-icon-theme)
(GPL-3.0-or-later) directly under `../icons-whitesur/` and wraps its
`install.sh` in a justfile + debian/ packaging so it can be built as a
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

    desktop/icons-whitesur/         vendored upstream sources (~67 MB)
    desktop/icons-whitesur-pkg/     this wrapper (justfile + debian/)

`desktop/icons-whitesur/UPSTREAM_COMMIT` records the exact upstream SHA
this snapshot was taken from.

## Build

    just install rootdir=/tmp/stage prefix=/usr

Calls upstream `install.sh -d /tmp/stage/usr/share/icons -n WhiteSur`,
which installs the default blue variant in light + dark to:

    /usr/share/icons/WhiteSur
    /usr/share/icons/WhiteSur-light
    /usr/share/icons/WhiteSur-dark

## Updating

Refresh from upstream by re-vendoring:

    rm -rf desktop/icons-whitesur
    git clone --depth 1 https://github.com/vinceliuice/WhiteSur-icon-theme \
        desktop/icons-whitesur
    UPSTREAM_SHA=$(git -C desktop/icons-whitesur rev-parse HEAD)
    echo "$UPSTREAM_SHA" > desktop/icons-whitesur/UPSTREAM_COMMIT
    rm -rf desktop/icons-whitesur/.git
    git add desktop/icons-whitesur
    git commit -m "icons-whitesur: bump to $UPSTREAM_SHA"

Then bump `debian/changelog` in this directory.

## License

GPL-3.0-or-later (matches upstream). See `debian/copyright`.

