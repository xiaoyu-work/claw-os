#!/usr/bin/env bash
# rootfs/features/vm/install.sh — VM-specific tweaks for persistent disk images.
#
# What we DO:
#  - Create a default 'cos' user (uid 1000, /bin/bash, passwordless sudo).
#    Same convention as the WSL target; users can tighten via `passwd cos`.
#  - Configure GRUB for serial-console-friendly boot:
#      GRUB_TERMINAL="serial console"
#      GRUB_SERIAL_COMMAND="serial --speed=115200 ..."
#      GRUB_CMDLINE_LINUX_DEFAULT="quiet console=tty0 console=ttyS0,115200n8 video=1920x1080"
#    Without GRUB_TERMINAL the menu would render on tty0 only — qemu
#    -nographic would appear to hang at the GRUB countdown.
#    video=1920x1080 forces a sane default mode so the greeter fills the
#    window — VMware/QEMU otherwise default to 1024x768 and guest auto-resize
#    only kicks in after login, leaving the login screen letterboxed.
#  - Enable serial-getty@ttyS0 so headless deploys have a login on
#    the serial port.
#
# What we DON'T do:
#  - Install GRUB to disk (targets/vm/build.sh does that in the
#    losetup'd image).
#  - Generate /boot/grub/grub.cfg (build.sh runs update-grub in chroot).
#  - Install cloud-init (out of M6 scope; intended for local hypervisors).
#
# Inherited from environment: ROOTFS, PROJECT_DIR.

set -euo pipefail

source "$PROJECT_DIR/scripts/lib/add-cos-user.sh"

# 1. Create 'cos' user (shared helper — also used by WSL and Docker targets).
#    EXCEPT: skip when the desktop feature has wired up the first-boot
#    wizard (cosmic-initial-setup). On a graphical VM image the wizard
#    will create the *real* user; pre-creating a passwordless-locked
#    `cos` would just clutter cosmic-greeter's user list and confuse
#    everyone. Headless VM builds (no desktop feature) still get `cos`.
if [ -f "$ROOTFS/etc/greetd/cosmic-greeter.toml" ] \
        && grep -q '^\[initial_session\]' "$ROOTFS/etc/greetd/cosmic-greeter.toml" 2>/dev/null; then
    echo "  :: desktop first-boot wizard present — skipping default 'cos' user"
else
    add_cos_user "$ROOTFS"
fi

# 2. /etc/default/grub for serial-friendly boot.
#    sed is in-place; the package ships a default file from grub-common.
GRUB_DEFAULT="$ROOTFS/etc/default/grub"
if [ -f "$GRUB_DEFAULT" ]; then
    sed -i \
        -e 's|^GRUB_CMDLINE_LINUX_DEFAULT=.*|GRUB_CMDLINE_LINUX_DEFAULT="quiet console=tty0 console=ttyS0,115200n8 video=1920x1080"|' \
        -e 's|^#\?GRUB_TERMINAL=.*|GRUB_TERMINAL="serial console"|' \
        -e 's|^#\?GRUB_SERIAL_COMMAND=.*|GRUB_SERIAL_COMMAND="serial --speed=115200 --unit=0 --word=8 --parity=no --stop=1"|' \
        "$GRUB_DEFAULT"

    # If the file didn't have the lines at all, append them.
    grep -q '^GRUB_TERMINAL=' "$GRUB_DEFAULT" || \
        echo 'GRUB_TERMINAL="serial console"' >> "$GRUB_DEFAULT"
    grep -q '^GRUB_SERIAL_COMMAND=' "$GRUB_DEFAULT" || \
        echo 'GRUB_SERIAL_COMMAND="serial --speed=115200 --unit=0 --word=8 --parity=no --stop=1"' >> "$GRUB_DEFAULT"
fi

# 3. Enable serial-getty on ttyS0 for headless login.
mkdir -p "$ROOTFS/etc/systemd/system/getty.target.wants"
ln -sf /usr/lib/systemd/system/serial-getty@.service \
    "$ROOTFS/etc/systemd/system/getty.target.wants/serial-getty@ttyS0.service"

echo "  :: created 'cos' user, configured GRUB serial+console terminal, enabled serial-getty@ttyS0"
