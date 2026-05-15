# scripts/lib/add-cos-user.sh — Shared 'cos' user creation for non-system targets.
# shellcheck shell=bash
#
# Source this from any target/feature script that needs a default unprivileged
# user (WSL, VM, Docker, …). The user matches the convention documented in
# targets/wsl/build.sh: uid 1000, /bin/bash login shell, passwordless sudo.
#
# Usage:
#   source "$PROJECT_DIR/scripts/lib/add-cos-user.sh"
#   add_cos_user "$ROOTFS"
#
# Idempotent: skips creation if a user named 'cos' already exists in the rootfs.
#
# Why passwordless sudo:
#   - WSL has no install-time password prompt.
#   - VM/Docker images ship without per-host secrets either.
#   Users can tighten via `sudo passwd cos` and editing /etc/sudoers.d/cos.

# Guard against being executed instead of sourced.
if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    echo "error: scripts/lib/add-cos-user.sh must be sourced, not executed" >&2
    exit 1
fi

add_cos_user() {
    local rootfs="${1:?add_cos_user: rootfs path required}"

    if [ ! -d "$rootfs" ]; then
        echo "add_cos_user: rootfs not a directory: $rootfs" >&2
        return 1
    fi

    chroot "$rootfs" /bin/bash -c '
        set -e
        if ! id cos >/dev/null 2>&1; then
            useradd -m -u 1000 -s /bin/bash -G sudo cos
            mkdir -p /etc/sudoers.d
            echo "cos ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/cos
            chmod 0440 /etc/sudoers.d/cos
        fi
    '
}
