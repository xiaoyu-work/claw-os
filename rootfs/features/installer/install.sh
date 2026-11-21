#!/usr/bin/env bash
# rootfs/features/installer/install.sh — wire Calamares onto a Live ISO.
#
# What this script does:
#  - Copies the installer overlay and branding assets
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
branding="$ROOTFS/etc/calamares/branding/claw-os"
install -m0644 "$PROJECT_DIR/assets/brand/clawos-icon-192.png" "$branding/logo.png"
install -m0644 "$PROJECT_DIR/assets/brand/clawos-wordmark.png" "$branding/welcome.png"

# 2. Mark the autostart hook + Xstartup script executable.
chmod 755 "$ROOTFS/etc/profile.d/cos-installer.sh"
chmod 755 "$ROOTFS/etc/cos/installer-xstartup"
chmod 755 "$ROOTFS/usr/lib/cos/init/cleanup-installer-target.sh"

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

if [ -x "$ROOTFS/usr/libexec/claw-oobe" ]; then
    if [ "$DEB_ARCH" = amd64 ]; then
        hardware=$(mktemp -d "$ROOTFS/tmp/claw-amd64-hardware.XXXXXX")
        trap 'rm -rf -- "$hardware"' EXIT
        install -d "$hardware"/{scripts,packaging}
        install -m0644 "$PROJECT_DIR"/scripts/{amd64_hardware,platform_kernels,surface_laptop7}.py "$hardware/scripts/"
        install -m0644 "$PROJECT_DIR/packaging/platform-kernels.json" "$hardware/packaging/"
        install -d "$hardware/packaging/"{camlink,ipu7}
        install -m0644 "$PROJECT_DIR"/packaging/camlink/* "$hardware/packaging/camlink/"
        install -m0644 "$PROJECT_DIR"/packaging/ipu7/* "$hardware/packaging/ipu7/"
        chroot "$ROOTFS" apt-get -qq update
        chroot "$ROOTFS" env DEBIAN_FRONTEND=noninteractive apt-get -o Dpkg::Use-Pty=0 \
            install -y --no-install-recommends build-essential pkg-config autoconf automake libtool patch \
            libglib2.0-dev libxml2-dev libsystemd-dev libnl-3-dev libnl-genl-3-dev libupower-glib-dev \
            cmake ninja-build "libdw-dev/$SUITE-backports" "libelf-dev/$SUITE-backports" \
            libjsoncpp-dev libdrm-dev libexpat1-dev libjpeg62-turbo-dev libyuv-dev \
            libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
            "cargo/$SUITE-backports" "rustc/$SUITE-backports" libudev-dev libusb-1.0-0-dev </dev/null
        inside=${hardware#"$ROOTFS"}
        chroot "$ROOTFS" python3 -B "$inside/scripts/amd64_hardware.py" \
            --cache "$inside/cache" --output /usr/share/cos/driver-cache
    fi
    chroot "$ROOTFS" /usr/lib/cos/hw/claw-driver-setup --stage-offline
fi

echo "  :: Calamares configured; X autostarts on tty1 login"
