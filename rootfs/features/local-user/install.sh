#!/usr/bin/env bash
set -euo pipefail

source "$PROJECT_DIR/scripts/lib/add-cos-user.sh"

if [[ ! -L "$ROOTFS/etc/systemd/system/getty.target.wants/serial-getty@ttyS0.service" ]]; then
    echo "error: feature 'local-user' requires feature 'vm' before it" >&2
    exit 1
fi

add_cos_user "$ROOTFS"
test "$(LC_ALL=C chroot "$ROOTFS" passwd --status cos | awk '{print $2}')" = L

if [[ -f "$ROOTFS/etc/greetd/claw-desktop.toml" ]]; then
    awk -F: '$1 == "cos" && $2 == "!" { found = 1 } END { exit !found }' "$ROOTFS/etc/shadow"
    install -Dm0600 /dev/null "$ROOTFS/var/lib/cos/local-desktop-password.pending"
    install -Dm0644 /dev/null "$ROOTFS/etc/cos/first-run"
else
    (umask 022; cp -a --no-preserve=mode,ownership "$SCRIPT_DIR/features/local-user/overlay/." "$ROOTFS/")
    chmod 0755 "$ROOTFS/usr/lib/cos/init/local-first-login"
fi

echo "  :: configured one-time password setup for the locked local cos account"
