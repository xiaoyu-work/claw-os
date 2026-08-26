#!/usr/bin/env bash
# rootfs/features/cos-core/install.sh -- install the shared agent and the
# Claw OS distribution integration package into ROOTFS.
#
# The .deb is the canonical source of truth: this script just makes sure it
# exists (building it on the fly if needed) and lets dpkg/apt put the files
# in place. Use of `apt-get install /path/to/file.deb` lets required
# dependencies resolve from the chroot's apt sources.
#
# Inherited from environment: ROOTFS, PROJECT_DIR, COS_VERSION.

set -euo pipefail

DEBS_DIR="$PROJECT_DIR/build/debs"

AGENT_DEB="$DEBS_DIR/claw-os-agent_${COS_VERSION}_${DEB_ARCH:-amd64}.deb"
BASE_DEB="$DEBS_DIR/claw-os-base_${COS_VERSION}_all.deb"
# Build this exact version if missing — stale packages from an earlier local
# build must never satisfy the check.
if [ ! -f "$AGENT_DEB" ] || [ ! -f "$BASE_DEB" ]; then
    echo "  :: Claw OS agent/base packages not found -- building them"
    "$PROJECT_DIR/packaging/deb/build-debs.sh"
fi

for deb in "$AGENT_DEB" "$BASE_DEB"; do
    if [ ! -f "$deb" ]; then
        echo "error: expected package missing after build: $deb" >&2
        exit 1
    fi
done
echo "  :: installing $(basename "$AGENT_DEB") + $(basename "$BASE_DEB")"

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

# Stage both .debs inside the chroot and install them in one apt transaction.
mkdir -p "$ROOTFS/var/cache/cos-debs"
cp "$AGENT_DEB" "$BASE_DEB" "$ROOTFS/var/cache/cos-debs/"
chroot "$ROOTFS" apt-get update -qq
chroot "$ROOTFS" apt-get install -y --no-install-recommends \
    "/var/cache/cos-debs/$(basename "$AGENT_DEB")" \
    "/var/cache/cos-debs/$(basename "$BASE_DEB")"
chroot "$ROOTFS" apt-get clean
rm -rf "$ROOTFS/var/lib/apt/lists"/*
