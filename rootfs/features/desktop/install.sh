#!/usr/bin/env bash
# rootfs/features/desktop/install.sh — build the claw-os desktop from
# source (vendored under PROJECT_DIR/desktop) and install it into the rootfs.
#
# The desktop source tree lives in-tree at $PROJECT_DIR/desktop/ — see
# desktop/README.md and desktop/PROVENANCE.md. install.sh runs `just build`
# + `just install` against that tree to populate $ROOTFS/usr with binaries,
# .desktop entries, systemd units, polkit rules, etc.
#
# Inputs (env):
#   ROOTFS       — target rootfs (from rootfs/build.sh)
#   PROJECT_DIR  — claw-os repo root (from rootfs/build.sh)
#   DESKTOP_SRC  — optional override; otherwise $PROJECT_DIR/desktop
#
# Prerequisites:
#   - Host Linux build tools + a rustup toolchain are installed inside the
#     chroot (this script bind-mounts the source into the chroot and builds
#     there so the produced binaries link against rootfs glibc, not the
#     host's).
#
# Skipping: set DESKTOP_SKIP=1 to scaffold-only (no build). Useful for fast
# iteration on packages.txt and overlay/ without paying the ~30-60min build.

set -euo pipefail

DESKTOP_SRC="${DESKTOP_SRC:-$PROJECT_DIR/desktop}"

if [ "${DESKTOP_SKIP:-0}" = "1" ]; then
    echo "  :: DESKTOP_SKIP=1 — overlay applied, build skipped"
    exit 0
fi

if [ ! -d "$DESKTOP_SRC" ] || [ ! -f "$DESKTOP_SRC/justfile" ]; then
    cat >&2 <<EOF
  error: desktop source not found at $DESKTOP_SRC
  Expected an in-tree vendored copy with a top-level justfile.
  Either:
    1. Run from a checked-out claw-os tree (desktop/ should exist).
    2. Set DESKTOP_SRC=/path/to/source-tree and re-run.
    3. Set DESKTOP_SKIP=1 to install runtime deps + overlay only (no DE binaries).
EOF
    exit 1
fi

# Validate the tree has the expected component dirs.
echo "  :: validating desktop source tree at $DESKTOP_SRC"
missing=0
for sub in comp session panel launcher settings greeter toolkit; do
    [ -e "$DESKTOP_SRC/$sub" ] || { echo "    missing: $sub"; missing=1; }
done
[ "$missing" = "0" ] || {
    echo "  error: desktop source tree is incomplete"
    exit 1
}

# Bind-mount the source into the chroot so we build against rootfs libs.
CHROOT_SRC="$ROOTFS/build/desktop-src"
mkdir -p "$CHROOT_SRC"
if ! mountpoint -q "$CHROOT_SRC"; then
    mount --bind "$DESKTOP_SRC" "$CHROOT_SRC"
fi

cleanup() {
    umount "$CHROOT_SRC" 2>/dev/null || true
    rmdir "$CHROOT_SRC" 2>/dev/null || true
    rmdir "$ROOTFS/build" 2>/dev/null || true
}
trap cleanup EXIT

# Install rustup toolchain inside the chroot (idempotent).
echo "  :: ensuring rustup toolchain in chroot"
chroot "$ROOTFS" bash -c '
    set -e
    if ! command -v rustup >/dev/null; then
        apt-get install -y --no-install-recommends rustup
    fi
    if ! rustup show active-toolchain >/dev/null 2>&1; then
        rustup toolchain install stable --profile minimal
        rustup default stable
    fi
    if ! command -v just >/dev/null; then
        cargo install --quiet just
    fi
'

# Build + install desktop binaries into /usr inside the chroot.
echo "  :: building desktop (this takes 30–60 minutes on a fresh tree)"
chroot "$ROOTFS" bash -c '
    set -e
    export CARGO_HOME=/root/.cargo
    export PATH="$CARGO_HOME/bin:$PATH"
    cd /build/desktop-src
    just build
    just install rootdir="" prefix=/usr
'

# Wire up the greeter as the default display manager. The upstream binary
# is still named cosmic-greeter; rename here once you fork+rename it.
echo "  :: enabling greeter as default display manager"
chroot "$ROOTFS" bash -c '
    set -e
    if [ -f /usr/lib/systemd/system/cosmic-greeter.service ]; then
        systemctl enable cosmic-greeter.service
        systemctl set-default graphical.target
    else
        echo "    warn: greeter service not installed; skipping enable"
    fi
'

echo "  :: desktop installed under $ROOTFS/usr"
