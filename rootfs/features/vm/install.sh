#!/usr/bin/env bash
# rootfs/features/vm/install.sh — VM-specific tweaks for persistent disk images.
#
# What we DO:
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
#  - Install GRUB to disk (targets/common/disk-image.sh does that in the
#    losetup'd image).
#  - Generate /boot/grub/grub.cfg (build.sh runs update-grub in chroot).
#  - Create a login user. The `local-user` feature does that for local
#    hypervisors; cloud profiles provision a per-instance user instead.
#  - Install cloud-init. That is a separate provider-neutral feature.
#
# Inherited from environment: ROOTFS, PROJECT_DIR.

set -euo pipefail

# 1. /etc/default/grub for serial-friendly boot.
#    sed is in-place; the package ships a default file from grub-common.
GRUB_DEFAULT="$ROOTFS/etc/default/grub"
if [ -f "$GRUB_DEFAULT" ]; then
    sed -i \
        -e 's|^#\?GRUB_TERMINAL=.*|GRUB_TERMINAL="serial console"|' \
        -e 's|^#\?GRUB_SERIAL_COMMAND=.*|GRUB_SERIAL_COMMAND="serial --speed=115200 --unit=0 --word=8 --parity=no --stop=1"|' \
        "$GRUB_DEFAULT"

    # If the file didn't have the lines at all, append them.
    grep -q '^GRUB_TERMINAL=' "$GRUB_DEFAULT" || \
        echo 'GRUB_TERMINAL="serial console"' >> "$GRUB_DEFAULT"
    grep -q '^GRUB_SERIAL_COMMAND=' "$GRUB_DEFAULT" || \
        echo 'GRUB_SERIAL_COMMAND="serial --speed=115200 --unit=0 --word=8 --parity=no --stop=1"' >> "$GRUB_DEFAULT"
fi

# 1b. Kernel command line, as a grub.d drop-in.
#
# It has to be a drop-in, and it has to sort after grub-disk's
# 50-claw-os.cfg: grub-mkconfig reads /etc/default/grub first and the
# drop-ins after, so anything this feature wrote to /etc/default/grub for
# GRUB_CMDLINE_LINUX_DEFAULT was silently overwritten by 50-claw-os.cfg —
# the serial console never actually reached the kernel.
#
# `splash` is kept from 50-claw-os.cfg: it is what starts plymouth, which
# covers the boot-to-greeter transition.
mkdir -p "$ROOTFS/etc/default/grub.d"
cat > "$ROOTFS/etc/default/grub.d/60-claw-os-vm.cfg" <<'EOF'
# Claw OS VM overrides. Sorts after 50-claw-os.cfg, so this wins.
GRUB_CMDLINE_LINUX_DEFAULT="quiet splash console=tty0 console=ttyS0,115200n8 video=1920x1080"
EOF

# 2. Enable serial-getty on ttyS0 for headless login.
mkdir -p "$ROOTFS/etc/systemd/system/getty.target.wants"
ln -sf /usr/lib/systemd/system/serial-getty@.service \
    "$ROOTFS/etc/systemd/system/getty.target.wants/serial-getty@ttyS0.service"

echo "  :: configured GRUB serial+console terminal, enabled serial-getty@ttyS0"
