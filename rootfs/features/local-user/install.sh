#!/usr/bin/env bash
set -euo pipefail

source "$PROJECT_DIR/scripts/lib/add-cos-user.sh"

if [[ ! -L "$ROOTFS/etc/systemd/system/getty.target.wants/serial-getty@ttyS0.service" ]]; then
    echo "error: feature 'local-user' requires feature 'vm' before it" >&2
    exit 1
fi

add_cos_user "$ROOTFS"
test "$(LC_ALL=C chroot "$ROOTFS" passwd --status cos | awk '{print $2}')" = L
(umask 022; cp -a --no-preserve=mode,ownership "$SCRIPT_DIR/features/local-user/overlay/." "$ROOTFS/")
chmod 0755 "$ROOTFS/usr/lib/cos/init/local-first-login"

if [[ -f "$ROOTFS/etc/greetd/claw-desktop.toml" ]]; then
    service_dir="$ROOTFS/etc/systemd/system"
    install -d "$service_dir/cos-local-first-login.service.d" "$service_dir/greetd.service.d"
    cat >"$service_dir/cos-local-first-login.service.d/graphical-console.conf" <<'EOF'
[Service]
TTYPath=/dev/tty7
ExecStartPre=/usr/bin/chvt 7
EOF
    cat >"$service_dir/greetd.service.d/local-password.conf" <<'EOF'
[Unit]
Requires=cos-local-first-login.service
After=cos-local-first-login.service
EOF
fi

echo "  :: configured one-time password setup for the locked local cos account"
