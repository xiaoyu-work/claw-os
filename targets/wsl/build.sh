#!/usr/bin/env bash
# targets/wsl/build.sh — Build a WSL2 importable rootfs tarball.
#
# Output:  build/claw-os-wsl-<arch>.tar.gz  (arch from $ARCH).
#
# Usage:   sudo ./build.sh wsl
#
# Steps:
#   1. Build a Debian rootfs with features: base, cos-core, browser, systemd
#      (browser is bundled but its systemd unit is NOT enabled — see plan §7).
#   2. Apply the WSL-specific overlay (wsl.conf).
#   3. Create a default 'cos' user (uid 1000, passwordless sudo).
#   4. Tar the rootfs into a tarball that `wsl --import` can consume.
#
# Note: WSL2 supports arm64 on Windows-on-ARM hosts (e.g. Surface Pro X,
# Snapdragon X, Apple Silicon Mac via Parallels). The arm64 tarball is
# imported the same way as amd64 — Windows picks the right tarball based
# on the host arch.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
ROOTFS="$PROJECT_DIR/build/claw-os-rootfs"

source "$PROJECT_DIR/scripts/lib/arch.sh"
source "$PROJECT_DIR/scripts/lib/add-cos-user.sh"

OUTPUT="$PROJECT_DIR/build/claw-os-wsl-${ARCH_SUFFIX}.tar.gz"

if [ "$(id -u)" -ne 0 ]; then
    echo "error: must run as root (debootstrap, chroot and tarball creation need it)" >&2
    exit 1
fi

# 1. Build the rootfs with the WSL feature set (if not already present).
#    apt-source pre-configures the Claw OS apt repo so users can later run
#    `sudo apt update && sudo apt upgrade` to pull newer claw-os-* packages.
#    This same feature set is shared with the Docker target so the WSL
#    tarball and the Docker image expose an identical terminal-version
#    surface; CI exploits this to build the rootfs once and run both target
#    scripts against it (the second invocation sees the rootfs and skips).
if [ ! -d "$ROOTFS" ]; then
    "$PROJECT_DIR/rootfs/build.sh" --features base,cos-core,browser,systemd,apt-source
else
    echo ":: using existing rootfs at $ROOTFS"
fi

# 2. Apply WSL-specific overlay (wsl.conf, etc.).
if [ -d "$SCRIPT_DIR/overlay" ]; then
    echo ":: applying WSL overlay"
    cp -a "$SCRIPT_DIR/overlay/." "$ROOTFS/"
fi

# 3. Create the default 'cos' user.
#    UID 1000 is conventional for the first non-system user; matches the
#    'default=cos' line in /etc/wsl.conf. Shared with the VM and Docker
#    targets via scripts/lib/add-cos-user.sh.
echo ":: creating default 'cos' user"
add_cos_user "$ROOTFS"

# 4. Tar up the rootfs. /proc, /sys and /dev are populated by WSL at boot;
#    excluding them keeps the tarball smaller and avoids permission issues.
echo ":: packaging $OUTPUT"
mkdir -p "$(dirname "$OUTPUT")"
tar -C "$ROOTFS" \
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
