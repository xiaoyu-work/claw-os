#!/usr/bin/env bash
set -euo pipefail

source "$PROJECT_DIR/scripts/lib/add-cos-user.sh"

if [[ ! -L "$ROOTFS/etc/systemd/system/getty.target.wants/serial-getty@ttyS0.service" ]]; then
    echo "error: feature 'local-user' requires feature 'vm' before it" >&2
    exit 1
fi

if [[ -f "$ROOTFS/etc/greetd/claw-desktop.toml" ]]; then
    if awk -F: '$3 >= 1000 && $3 < 60000 { found = 1 } END { exit !found }' "$ROOTFS/etc/passwd"; then
        echo "error: fresh local desktop setup requires an image without a human account" >&2
        exit 1
    fi
    install -Dm0644 /dev/null "$ROOTFS/etc/cos/local-desktop-setup"
    echo "  :: graphical first run will create the local owner"
else
    add_cos_user "$ROOTFS"
    test "$(LC_ALL=C chroot "$ROOTFS" passwd --status cos | awk '{print $2}')" = L
    (umask 022; cp -a --no-preserve=mode,ownership "$SCRIPT_DIR/features/local-user/overlay/." "$ROOTFS/")
    chmod 0755 "$ROOTFS/usr/lib/cos/init/local-first-login"
    echo "  :: configured one-time password setup for the locked local cos account"
fi
