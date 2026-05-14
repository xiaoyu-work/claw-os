#!/usr/bin/env bash
# rootfs/features/browser/install.sh — install claw-os-browser.deb into ROOTFS.
#
# Depends on claw-os-base (installed by the cos-core feature). The .deb
# Depends on chromium which is installed via this feature's packages.txt
# first, so apt's resolution finishes without network roundtrips inside
# the chroot.
#
# Inherited from environment: ROOTFS, PROJECT_DIR.

set -euo pipefail

DEBS_DIR="$PROJECT_DIR/build/debs"

if ! ls "$DEBS_DIR/claw-os-browser_"*.deb >/dev/null 2>&1; then
    echo "  :: claw-os-browser.deb not found — building it"
    "$PROJECT_DIR/packaging/deb/build-debs.sh"
fi

BROWSER_DEB="$(ls "$DEBS_DIR/claw-os-browser_"*"_${DEB_ARCH:-amd64}.deb" | head -1)"
echo "  :: installing $(basename "$BROWSER_DEB")"

# Remove the overlay copy so dpkg can claim the file.
rm -f "$ROOTFS/usr/lib/cos/services/browser/service.json"

mkdir -p "$ROOTFS/var/cache/cos-debs"
cp "$BROWSER_DEB" "$ROOTFS/var/cache/cos-debs/"
chroot "$ROOTFS" apt-get install -y --no-install-recommends \
    "/var/cache/cos-debs/$(basename "$BROWSER_DEB")"
chroot "$ROOTFS" apt-get clean
rm -rf "$ROOTFS/var/lib/apt/lists"/*
