#!/usr/bin/env bash
# Add a local login account for artifacts without platform provisioning.

set -euo pipefail

source "$PROJECT_DIR/scripts/lib/add-cos-user.sh"

# A graphical image uses its first-boot wizard to create the real user.
# Headless local VM images create a locked account, then require an
# interactive password to be established on the serial console before the
# normal getty is allowed to start.
if [ -f "$ROOTFS/etc/greetd/cosmic-greeter.toml" ] \
        && grep -q '^\[initial_session\]' "$ROOTFS/etc/greetd/cosmic-greeter.toml" 2>/dev/null; then
    echo "  :: desktop first-boot wizard present - skipping local 'cos' user"
else
    if [ ! -L "$ROOTFS/etc/systemd/system/getty.target.wants/serial-getty@ttyS0.service" ]; then
        echo "error: feature 'local-user' requires feature 'vm' before it" >&2
        exit 1
    fi

    add_cos_user "$ROOTFS"

    password_status="$(LC_ALL=C chroot "$ROOTFS" passwd --status cos | awk '{print $2}')"
    if [ "$password_status" != "L" ]; then
        echo "error: local 'cos' account must be locked in the image" >&2
        exit 1
    fi

    feature_dir="$SCRIPT_DIR/features/local-user"
    cp -a --no-preserve=ownership "$feature_dir/overlay/." "$ROOTFS/"
    chmod 0755 "$ROOTFS/usr/lib/cos/init/local-first-login"

    for path in \
        usr/lib/cos/init/local-first-login \
        usr/lib/systemd/system/cos-local-first-login.service \
        etc/systemd/system/serial-getty@ttyS0.service.d/cos-first-login.conf; do
        if [ ! -f "$ROOTFS/$path" ]; then
            echo "error: local-user first-login asset missing: $path" >&2
            exit 1
        fi
    done

    echo "  :: created locked local 'cos' user"
    echo "  :: serial console will require password setup before first login"
fi
