#!/usr/bin/env bash

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    echo "error: scripts/lib/image-identity.sh must be sourced, not executed" >&2
    exit 1
fi

human_login_users() {
    local rootfs="${1:?human_login_users: rootfs path required}"
    awk -F: \
        '$3 >= 1000 && $3 < 60000 && $7 !~ /(nologin|false)$/ { print $1 }' \
        "$rootfs/etc/passwd"
}

assert_no_human_login_users() {
    local rootfs="${1:?assert_no_human_login_users: rootfs path required}"
    local image_name="${2:-image}"
    local users

    if [ ! -f "$rootfs/etc/passwd" ]; then
        echo "error: $image_name has no /etc/passwd" >&2
        return 1
    fi
    users="$(human_login_users "$rootfs")"
    if [ -n "$users" ]; then
        echo "error: $image_name contains pre-provisioned login users:" >&2
        while IFS= read -r user; do
            [ -n "$user" ] && printf '  %s\n' "$user" >&2
        done <<< "$users"
        return 1
    fi
}
