#!/usr/bin/env bash
# rootfs/features/live/install.sh — Live media tweaks.
#
# What we DO:
#  - Enable NetworkManager so the live system gets a network on boot.
#  - Install openssh-server (via packages.txt) but DO NOT enable it.
#    Live-config creates a default user with a known password ("user"/"live"
#    by default), so auto-starting ssh on a public network would expose a
#    known-credential login. Users opt in with `systemctl enable --now ssh`.
#  - Configure tty1 autologin for the live-config "user" account so headless
#    boots (qemu -nographic, USB keyboard) reach a shell without a password
#    prompt.
#
# What we DON'T do:
#  - Create the live user (live-config does this at boot via the
#    `boot=live components` kernel cmdline).
#  - Touch /etc/sudoers (live-config configures passwordless sudo for the
#    live user only).
#
# Inherited from environment: ROOTFS.

set -euo pipefail

# Enable NetworkManager.
mkdir -p "$ROOTFS/etc/systemd/system/multi-user.target.wants"
ln -sf /usr/lib/systemd/system/NetworkManager.service \
    "$ROOTFS/etc/systemd/system/multi-user.target.wants/NetworkManager.service"

# Auto-login on tty1. live-config defaults the live user to username "user".
mkdir -p "$ROOTFS/etc/systemd/system/getty@tty1.service.d"
cat > "$ROOTFS/etc/systemd/system/getty@tty1.service.d/autologin.conf" <<'EOF'
[Service]
ExecStart=
ExecStart=-/sbin/agetty --autologin user --noclear %I $TERM
EOF

echo "  :: NetworkManager enabled, tty1 autologin configured"
echo "  :: ssh installed but NOT enabled (run 'systemctl enable --now ssh' to expose it)"
