#!/usr/bin/env bash
# rootfs/features/systemd/install.sh — install claw-os-systemd.deb into ROOTFS.
#
# The .deb's postinst calls `deb-systemd-helper enable cos-den-setup.service`
# which creates the multi-user.target.wants symlink without needing a
# running systemd, so it works inside a chroot.
#
# Inherited from environment: ROOTFS, PROJECT_DIR, SCRIPT_DIR.

set -euo pipefail

DEBS_DIR="$PROJECT_DIR/build/debs"

if ! ls "$DEBS_DIR/claw-os-systemd_"*.deb >/dev/null 2>&1; then
    echo "  :: claw-os-systemd.deb not found — building it"
    "$PROJECT_DIR/packaging/deb/build-debs.sh"
fi

SYSTEMD_DEB="$(ls "$DEBS_DIR/claw-os-systemd_"*"_all.deb" | head -1)"
echo "  :: installing $(basename "$SYSTEMD_DEB")"

mkdir -p "$ROOTFS/var/cache/cos-debs"
cp "$SYSTEMD_DEB" "$ROOTFS/var/cache/cos-debs/"
chroot "$ROOTFS" apt-get install -y --no-install-recommends \
    "/var/cache/cos-debs/$(basename "$SYSTEMD_DEB")"
chroot "$ROOTFS" apt-get clean
rm -rf "$ROOTFS/var/lib/apt/lists"/*

# Verify the wants symlink (postinst should have created it).
if [ -L "$ROOTFS/etc/systemd/system/multi-user.target.wants/cos-den-setup.service" ]; then
    echo "  :: cos-den-setup.service enabled"
else
    echo "  :: WARNING — deb-systemd-helper did not enable cos-den-setup.service" >&2
    echo "     falling back to direct symlink"
    mkdir -p "$ROOTFS/etc/systemd/system/multi-user.target.wants"
    ln -sf /usr/lib/systemd/system/cos-den-setup.service \
        "$ROOTFS/etc/systemd/system/multi-user.target.wants/cos-den-setup.service"
fi
echo "  :: cos-browser.service installed (not enabled by default)"
