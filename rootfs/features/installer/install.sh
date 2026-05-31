#!/usr/bin/env bash
# rootfs/features/installer/install.sh — wire Calamares onto a Live ISO.
#
# What this script does:
#  - Copies the installer overlay (settings.conf, modules/*.conf, branding/)
#    into the rootfs at /etc/calamares/.
#  - Marks /etc/cos/installer-xstartup and /etc/profile.d/cos-installer.sh
#    executable; the profile.d hook is what auto-starts X on tty1 login.
#  - Gives the live "user" account passwordless sudo for the duration of
#    the install (live-config also does this, but we make it explicit so
#    Calamares can call privileged helpers via pkexec/sudo).
#
# Inherited from environment: ROOTFS, SCRIPT_DIR.

set -euo pipefail

FEATURE_DIR="$SCRIPT_DIR/features/installer"

# 1. Apply overlay.
if [ -d "$FEATURE_DIR/overlay" ]; then
    echo "  :: applying installer overlay"
    cp -a --no-preserve=ownership "$FEATURE_DIR/overlay/." "$ROOTFS/"
fi

# 2. Mark the autostart hook + Xstartup script executable.
chmod 755 "$ROOTFS/etc/profile.d/cos-installer.sh"
chmod 755 "$ROOTFS/etc/cos/installer-xstartup"

# 3. Passwordless sudo for the live user (live-config creates "user" at
#    boot, member of the sudo group). live-config also sets sudoers,
#    but installing our own override makes the dependency explicit and
#    survives live-config behavior changes.
mkdir -p "$ROOTFS/etc/sudoers.d"
cat > "$ROOTFS/etc/sudoers.d/cos-installer" <<'EOF'
# Allow the live user to run Calamares (which calls partitioning + grub
# helpers) without typing a password. Burned into the live image only;
# the target system Calamares creates has its own users + sudoers config.
user ALL=(ALL) NOPASSWD:ALL
EOF
chmod 0440 "$ROOTFS/etc/sudoers.d/cos-installer"

echo "  :: Calamares configured; X autostarts on tty1 login"
