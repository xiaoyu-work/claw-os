#!/usr/bin/env bash
# Remove per-instance identity from a staged cloud image filesystem.

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    echo "error: scripts/lib/generalize-cloud-image.sh must be sourced, not executed" >&2
    exit 1
fi

generalize_cloud_image() {
    local rootfs="${1:?generalize_cloud_image: rootfs path required}"
    local users
    local greetd_config
    local greetd_tmp

    if [ ! -d "$rootfs" ]; then
        echo "generalize_cloud_image: not a directory: $rootfs" >&2
        return 1
    fi
    if [ ! -x "$rootfs/usr/bin/cloud-init" ]; then
        echo "generalize_cloud_image: cloud-init is not installed" >&2
        return 1
    fi

    # A generalized image must not contain a reusable human login account.
    users="$(
        awk -F: '$3 >= 1000 && $3 < 60000 && $7 !~ /(nologin|false)$/ { print $1 }' \
            "$rootfs/etc/passwd"
    )"
    if [ -n "$users" ]; then
        echo "error: generalized cloud image contains login users:" >&2
        printf '  %s\n' $users >&2
        echo "       remove the 'local-user' feature from the cloud profile" >&2
        return 1
    fi

    # cloud-init will generate these independently for every VM.
    install -d -m 0755 "$rootfs/etc"
    : > "$rootfs/etc/machine-id"
    install -d -m 0755 "$rootfs/var/lib/dbus"
    rm -f "$rootfs/var/lib/dbus/machine-id"
    ln -s /etc/machine-id "$rootfs/var/lib/dbus/machine-id"
    echo "localhost" > "$rootfs/etc/hostname"

    find "$rootfs/etc/ssh" -maxdepth 1 -type f -name 'ssh_host_*' -delete 2>/dev/null || true
    rm -f "$rootfs/root/.ssh/authorized_keys" "$rootfs/root/.bash_history"
    rm -f \
        "$rootfs/var/lib/systemd/random-seed" \
        "$rootfs/etc/resolv.conf" \
        "$rootfs/etc/machine-info" \
        "$rootfs/etc/udev/rules.d/70-persistent-net.rules"

    # This rootfs has never booted, but cleaning the directories explicitly
    # also makes SKIP_ROOTFS and future image-recapture workflows safe.
    rm -rf "$rootfs/var/lib/cloud"
    install -d -m 0755 \
        "$rootfs/var/lib/cloud" \
        "$rootfs/var/lib/cloud/data" \
        "$rootfs/var/lib/cloud/instances" \
        "$rootfs/var/lib/cloud/seed"

    for lease_dir in "$rootfs/var/lib/dhcp" "$rootfs/var/lib/NetworkManager"; do
        if [ -d "$lease_dir" ]; then
            find "$lease_dir" -maxdepth 1 -type f \
                \( -name '*lease*' -o -name 'timestamps' -o -name 'seen-bssids' \
                   -o -name 'secret_key' -o -name 'secret_key.tmp' \) \
                -delete
        fi
    done

    for log in \
        "$rootfs/var/log/cloud-init.log" \
        "$rootfs/var/log/cloud-init-output.log" \
        "$rootfs/var/log/waagent.log"; do
        rm -f "$log"
    done
    rm -rf "$rootfs/var/log/journal"
    for accounting_log in wtmp btmp lastlog; do
        if [ -e "$rootfs/var/log/$accounting_log" ]; then
            : > "$rootfs/var/log/$accounting_log"
        fi
    done
    for transient_dir in "$rootfs/tmp" "$rootfs/var/tmp"; do
        if [ -d "$transient_dir" ]; then
            find "$transient_dir" -mindepth 1 -delete
        fi
    done

    # Desktop cloud images receive their user from cloud-init. Remove the
    # local appliance wizard and order the greeter after provisioning.
    greetd_config="$rootfs/etc/greetd/cosmic-greeter.toml"
    if [ -f "$greetd_config" ]; then
        greetd_tmp="${greetd_config}.cloud"
        awk '
            /^\[initial_session\][[:space:]]*$/ { skip = 1; next }
            skip && /^\[/ { skip = 0 }
            !skip { print }
        ' "$greetd_config" > "$greetd_tmp"
        mv "$greetd_tmp" "$greetd_config"

        install -d -m 0755 "$rootfs/etc/systemd/system/cosmic-greeter.service.d"
        cat > "$rootfs/etc/systemd/system/cosmic-greeter.service.d/after-cloud-init.conf" <<'EOF'
[Unit]
Wants=cloud-init.target
After=cloud-init.target
EOF
    fi
}
