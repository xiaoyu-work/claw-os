#!/usr/bin/env bash
# rootfs/features/gpu-drivers/install.sh — install the GPU driver helper.
#
# Ships /usr/lib/cos/hw/gpu-setup (env-aware GPU driver setup), a polkit
# policy so the desktop wizard can elevate the install action, and a
# first-boot oneshot that surfaces GPU status (MOTD + journal) on every
# target. The proprietary NVIDIA driver is installed on demand by the
# helper — never baked into the image — so a single image stays correct
# whether it lands on an NVIDIA laptop, an AMD/Intel machine, WSL, or Docker.
#
# Inherited from environment: ROOTFS, PROJECT_DIR, SCRIPT_DIR.

set -euo pipefail

FEATURE_DIR="$SCRIPT_DIR/features/gpu-drivers"

# 1. Apply overlay (helper script, polkit policy, systemd unit).
if [ -d "$FEATURE_DIR/overlay" ] && [ -n "$(ls -A "$FEATURE_DIR/overlay" 2>/dev/null)" ]; then
    echo "  :: applying gpu-drivers overlay"
    cp -a --no-preserve=ownership "$FEATURE_DIR/overlay/." "$ROOTFS/"
fi

# 2. The helper must be executable, and reachable as `claw-gpu-setup` on PATH.
chmod 0755 "$ROOTFS/usr/lib/cos/hw/gpu-setup"
ln -sf /usr/lib/cos/hw/gpu-setup "$ROOTFS/usr/bin/claw-gpu-setup"

# 3. Enable the first-boot detection service. We symlink directly (same
#    approach the systemd feature falls back to) so it works inside the
#    chroot without a running systemd.
mkdir -p "$ROOTFS/etc/systemd/system/multi-user.target.wants"
ln -sf /usr/lib/systemd/system/cos-gpu-setup.service \
    "$ROOTFS/etc/systemd/system/multi-user.target.wants/cos-gpu-setup.service"
echo "  :: cos-gpu-setup.service enabled (detect + MOTD only; never auto-installs)"
