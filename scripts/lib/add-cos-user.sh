#!/usr/bin/env bash
# shellcheck shell=bash

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    echo "error: scripts/lib/add-cos-user.sh must be sourced" >&2
    exit 1
fi

add_cos_user() {
    local rootfs="${1:?add_cos_user: rootfs path required}"
    chroot "$rootfs" /bin/bash -c '
        set -e
        if ! id cos >/dev/null 2>&1; then
            useradd -m -u 1000 -s /bin/bash -G sudo cos
            passwd --lock cos >/dev/null
        fi
        rm -f /etc/sudoers.d/cos
    '
}
