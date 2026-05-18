#!/usr/bin/env bash
# packaging/deb/build-desktop-deb.sh — wrap a staged desktop root into
# claw-os-desktop_<version>_<arch>.deb.
#
# The desktop feature builds inside the target rootfs, then installs the
# workspace into a staging root under $ROOTFS/build. This script adds Debian
# metadata and emits the package into build/debs/.

set -euo pipefail

if [ $# -ne 1 ]; then
    echo "usage: $0 <desktop-package-root>" >&2
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
source "$PROJECT_DIR/scripts/lib/arch.sh"

STAGE_ROOT="$1"
OUT_DIR="$PROJECT_DIR/build/debs"
VERSION="$(grep '^version' "$PROJECT_DIR/core/Cargo.toml" | head -1 | sed 's/.*"\(.*\)".*/\1/')"

if [ ! -d "$STAGE_ROOT" ]; then
    echo "error: desktop package root not found: $STAGE_ROOT" >&2
    exit 1
fi

DPKG_DEB="$(command -v dpkg-deb || true)"
if [ -z "$DPKG_DEB" ]; then
    echo "error: dpkg-deb not found. Install it with: apt-get install dpkg-dev" >&2
    exit 1
fi

if [ "$(id -u)" -eq 0 ]; then
    FAKEROOT=""
elif command -v fakeroot >/dev/null 2>&1; then
    FAKEROOT="fakeroot --"
else
    echo "warning: not root and fakeroot not available — files in .deb will" >&2
    echo "         be owned by uid=$(id -u). Install 'fakeroot' to fix." >&2
    FAKEROOT=""
fi

mkdir -p "$STAGE_ROOT/DEBIAN" "$OUT_DIR"
sed -e "s/__VERSION__/$VERSION/g" -e "s/__ARCH__/$DEB_ARCH/g" \
    "$SCRIPT_DIR/claw-os-desktop/control" > "$STAGE_ROOT/DEBIAN/control"
install -m 644 "$SCRIPT_DIR/claw-os-desktop/conffiles" "$STAGE_ROOT/DEBIAN/conffiles"
install -m 755 "$SCRIPT_DIR/claw-os-desktop/postinst" "$STAGE_ROOT/DEBIAN/postinst"

OUT="$OUT_DIR/claw-os-desktop_${VERSION}_${DEB_ARCH}.deb"
echo ":: dpkg-deb --build claw-os-desktop -> $OUT"
$FAKEROOT "$DPKG_DEB" --root-owner-group --build "$STAGE_ROOT" "$OUT" >/dev/null
