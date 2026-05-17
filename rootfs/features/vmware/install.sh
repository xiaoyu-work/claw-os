#!/usr/bin/env bash
# rootfs/features/vmware/install.sh — VMware-specific guest integration.
#
# Provides VMware Tools guest services used for host/guest coordination such
# as automatic display resizing in VMware Fusion / Workstation / ESXi.
# Inherited from environment: ROOTFS.

set -euo pipefail

if [ ! -x "$ROOTFS/usr/bin/systemctl" ] && [ ! -x "$ROOTFS/bin/systemctl" ]; then
    echo "  error: vmware feature requires the systemd feature to run before it" >&2
    exit 1
fi

chroot "$ROOTFS" systemctl enable open-vm-tools.service

echo "  :: enabled VMware Tools guest service"
