#!/usr/bin/env bash
# rootfs/features/cos-core/install.sh — install claw-os-base.deb into ROOTFS.
#
# The .deb is the canonical source of truth: this script just makes sure it
# exists (building it on the fly if needed) and lets dpkg/apt put the files
# in place. Use of `apt-get install /path/to/file.deb` lets dependencies
# (Recommends: nodejs, python3, etc.) resolve from the chroot's apt sources.
#
# Inherited from environment: ROOTFS, PROJECT_DIR, COS_VERSION.

set -euo pipefail

DEBS_DIR="$PROJECT_DIR/build/debs"

BASE_DEB="$DEBS_DIR/claw-os-base_${COS_VERSION}_${DEB_ARCH:-amd64}.deb"
# Build this exact version if missing — stale packages from an earlier local
# build must never satisfy the check.
if [ ! -f "$BASE_DEB" ]; then
    echo "  :: claw-os-base.deb not found — building it"
    "$PROJECT_DIR/packaging/deb/build-debs.sh"
fi

if [ ! -f "$BASE_DEB" ]; then
    echo "error: expected package missing after build: $BASE_DEB" >&2
    exit 1
fi
echo "  :: installing $(basename "$BASE_DEB")"

# The base overlay (applied in step 2 of rootfs/build.sh) ships these
# same files; dpkg refuses to overwrite unowned files by default. Remove
# them so the package can take ownership.
rm -f \
    "$ROOTFS/usr/local/bin/cos-init" \
    "$ROOTFS/usr/local/bin/cos" \
    "$ROOTFS/usr/lib/cos/init/setup-home.sh" \
    "$ROOTFS/etc/cos/profile.sh"
rm -rf \
    "$ROOTFS/usr/lib/cos/apps" \
    "$ROOTFS/usr/lib/cos/skills"

# Stage the .deb inside the chroot, install via apt so Recommends pull in.
mkdir -p "$ROOTFS/var/cache/cos-debs"
cp "$BASE_DEB" "$ROOTFS/var/cache/cos-debs/"
chroot "$ROOTFS" apt-get update -qq
chroot "$ROOTFS" apt-get install -y --no-install-recommends \
    "/var/cache/cos-debs/$(basename "$BASE_DEB")"
chroot "$ROOTFS" apt-get clean
rm -rf "$ROOTFS/var/lib/apt/lists"/*
