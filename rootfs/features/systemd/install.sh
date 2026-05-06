#!/usr/bin/env bash
# rootfs/features/systemd/install.sh — Install systemd unit files and
# enable cos-den-setup.service by default.
#
# cos-browser.service is installed but NOT enabled — callers opt in via
# `systemctl enable cos-browser.service` on the running system, or by
# adding a target-specific drop-in.
#
# Inherited from environment: ROOTFS, SCRIPT_DIR.

set -euo pipefail

FEATURE_DIR="$SCRIPT_DIR/features/systemd"

# 1. Copy this feature's overlay (systemd unit files).
if [ -d "$FEATURE_DIR/overlay" ]; then
    echo "  :: applying systemd overlay (unit files)"
    cp -a "$FEATURE_DIR/overlay/." "$ROOTFS/"
fi

# 2. Enable cos-den-setup.service by symlinking into multi-user.target.wants.
#    Doing this directly (instead of `chroot systemctl enable`) avoids the
#    fragility of running systemctl inside a chroot without dbus.
WANTS_DIR="$ROOTFS/etc/systemd/system/multi-user.target.wants"
mkdir -p "$WANTS_DIR"
ln -sf /usr/lib/systemd/system/cos-den-setup.service \
    "$WANTS_DIR/cos-den-setup.service"
echo "  :: cos-den-setup.service enabled"
echo "  :: cos-browser.service installed (not enabled by default)"
