# scripts/lib/add-cos-user.sh — Headless local-VM development account helper.
# shellcheck shell=bash
#
# Usage:
#   source "$PROJECT_DIR/scripts/lib/add-cos-user.sh"
#   add_cos_user "$ROOTFS"
#
# Idempotent: skips creation if a user named 'cos' already exists in the rootfs.
#
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

# Creates the `cosmic-initial-setup` system user used by greetd's
# [initial_session] to run the first-boot wizard. This user is created
# without a password — login is only via greetd's initial_session,
# which authenticates by uid, not by typed-in password.
#
# Why /var/lib/cosmic-initial-setup (not /run): cosmic-session writes
# transient state under $HOME during the wizard. /run is tmpfs which
# is fine in principle, but bind-mounting + cleanup is fussy across
# logout/login boundaries. /var/lib is the path Pop!_OS uses too.
#
# Idempotent.
add_cosmic_initial_setup_user() {
    local rootfs="${1:?add_cosmic_initial_setup_user: rootfs path required}"

    if [ ! -d "$rootfs" ]; then
        echo "add_cosmic_initial_setup_user: rootfs not a directory: $rootfs" >&2
        return 1
    fi

    chroot "$rootfs" /bin/bash -c '
        set -e
        if ! id cosmic-initial-setup >/dev/null 2>&1; then
            # --system → uid in system range (<1000), filtered out of the
            #            cosmic-greeter user list automatically.
            # --shell /bin/bash → cosmic-session runs via the user shell,
            #            and the UserFilter skips /usr/sbin/nologin.
            adduser --system --force-badname --quiet \
                --home /var/lib/cosmic-initial-setup \
                --shell /bin/bash \
                cosmic-initial-setup
            # adduser --system creates the home dir; ensure perms are
            # something cosmic-session is happy with (it writes
            # ~/.config + ~/.cache).
            install -d -o cosmic-initial-setup -g nogroup -m 0755 \
                /var/lib/cosmic-initial-setup
        fi
    '
}
