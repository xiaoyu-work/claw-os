#!/usr/bin/env bash
# targets/wsl/build.sh — Build a WSL2 importable rootfs tarball.
#
# Output:  build/claw-os-wsl-<arch>.tar.gz  (arch from $ARCH).
#
# Usage:   sudo ./build.sh wsl
#
# Steps:
#   1. Build a Debian rootfs with features: base, cos-core, browser, systemd,
#      qwen3-embedding where upstream ships the Linux runtime (browser is
#      bundled but its systemd unit is NOT enabled).
#   2. Apply the WSL-specific overlay (wsl.conf).
#   3. Create a default 'cos' user (uid 1000, passwordless sudo).
#   4. Tar the rootfs into a tarball that `wsl --import` can consume.
#
# Note: WSL2 supports arm64 on Windows-on-ARM hosts (e.g. Surface Pro X,
# Snapdragon X, Apple Silicon Mac via Parallels). The arm64 tarball is
# imported the same way as amd64 — Windows picks the right tarball based
# on the host arch. It still runs Linux arm64 userland, not native Windows
# arm64 binaries, so the Qwen3 embedding stack waits for a Linux arm64
# ort-genai runtime.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
ROOTFS="$PROJECT_DIR/build/claw-os-rootfs"

source "$PROJECT_DIR/scripts/lib/arch.sh"
source "$PROJECT_DIR/scripts/lib/add-cos-user.sh"
source "$PROJECT_DIR/scripts/lib/image-profiles.sh"

FEATURES="${FEATURES:-$IMAGE_FEATURES_HEADLESS_RUNTIME}"

OUTPUT="$PROJECT_DIR/build/claw-os-wsl-${ARCH_SUFFIX}.tar.gz"
WSL_ROOTFS="$PROJECT_DIR/build/claw-os-wsl-rootfs-${ARCH_SUFFIX}"
WSL_UPPER="$PROJECT_DIR/build/.claw-os-wsl-upper-${ARCH_SUFFIX}"
WSL_WORK="$PROJECT_DIR/build/.claw-os-wsl-work-${ARCH_SUFFIX}"

if [ "$(id -u)" -ne 0 ]; then
    echo "error: must run as root (debootstrap, chroot and tarball creation need it)" >&2
    exit 1
fi

if mountpoint -q "$WSL_ROOTFS" 2>/dev/null; then
    umount "$WSL_ROOTFS" 2>/dev/null || umount -l "$WSL_ROOTFS"
fi
rm -rf "$WSL_ROOTFS" "$WSL_UPPER" "$WSL_WORK"

# 1. Build or strictly reuse the immutable base rootfs.
#    apt-source pre-configures the Claw OS apt repo so users can later run
#    `sudo apt update && sudo apt upgrade` to pull newer claw-os-* packages.
#    This same feature set is shared with the Docker target. The stamped
#    base tree may be reused, but all WSL changes happen in private staging.
"$PROJECT_DIR/rootfs/build.sh" --reuse-if-matching --features "$FEATURES"

echo ":: creating WSL staging rootfs at $WSL_ROOTFS"
mkdir -p "$WSL_ROOTFS" "$WSL_UPPER" "$WSL_WORK"
mount -t overlay overlay \
    -o "lowerdir=$ROOTFS,upperdir=$WSL_UPPER,workdir=$WSL_WORK" \
    "$WSL_ROOTFS"
cleanup_wsl_staging() {
    if mountpoint -q "$WSL_ROOTFS" 2>/dev/null; then
        umount "$WSL_ROOTFS" 2>/dev/null || umount -l "$WSL_ROOTFS" 2>/dev/null || true
    fi
    rm -rf "$WSL_ROOTFS" "$WSL_UPPER" "$WSL_WORK"
}
trap cleanup_wsl_staging EXIT

# 2. Apply WSL-specific overlay (wsl.conf, etc.).
if [ -d "$SCRIPT_DIR/overlay" ]; then
    echo ":: applying WSL overlay"
    cp -a --no-preserve=ownership "$SCRIPT_DIR/overlay/." "$WSL_ROOTFS/"
fi

# 3. Create the default 'cos' user.
#    UID 1000 is conventional for the first non-system user; matches the
#    'default=cos' line in /etc/wsl.conf. Shared with the VM and Docker
#    targets via scripts/lib/add-cos-user.sh.
echo ":: creating default 'cos' user"
add_cos_user "$WSL_ROOTFS"

# 4. Tar up the rootfs. /proc, /sys and /dev are populated by WSL at boot;
#    excluding them keeps the tarball smaller and avoids permission issues.
echo ":: packaging $OUTPUT"
mkdir -p "$(dirname "$OUTPUT")"
tar -C "$WSL_ROOTFS" \
    --exclude='./proc/*' \
    --exclude='./sys/*' \
    --exclude='./dev/*' \
    --exclude='./run/*' \
    --exclude='./tmp/*' \
    -czf "$OUTPUT" .

SIZE=$(du -h "$OUTPUT" | cut -f1)
echo ":: done — $OUTPUT ($SIZE)"
echo
echo "To install on Windows:"
echo "  wsl --import claw-os C:\\WSL\\claw-os $OUTPUT --version 2"
echo "  wsl -d claw-os"
