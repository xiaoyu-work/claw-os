#!/usr/bin/env bash
# rootfs/features/grub-disk/install.sh — Claw OS grub defaults.
#
# What this DOES:
#  - Drops a Claw OS-branded /etc/default/grub.d/50-claw-os.cfg.
#    grub-mkconfig sources /etc/default/grub.d/*.cfg after the main
#    /etc/default/grub, so this dropin survives apt upgrades of the
#    grub-common package without conflict prompts.
#
# What this does NOT do (out of scope — the "make a bootable image"
# step is deliberately deferred, see rootfs/features/README):
#  - parted / mkfs on a target disk.
#  - grub-install onto a physical block device. Calamares handles
#    that at install time via /etc/calamares/modules/bootloader.conf;
#    a future iso-build script will do it for the live medium.
#  - Generate /boot/grub/grub.cfg — there is no kernel-on-target
#    until Calamares unpacks the squashfs, so update-grub here would
#    write an empty menu.
#
# Inherited from environment: ROOTFS.

set -euo pipefail

mkdir -p "$ROOTFS/etc/default/grub.d"
cat > "$ROOTFS/etc/default/grub.d/50-claw-os.cfg" <<'EOF'
# Claw OS grub defaults. Sourced by grub-mkconfig after
# /etc/default/grub, so values here win.
GRUB_DISTRIBUTOR="Claw OS"
GRUB_TIMEOUT=2
GRUB_TIMEOUT_STYLE=menu
GRUB_CMDLINE_LINUX_DEFAULT="quiet splash"
GRUB_CMDLINE_LINUX=""
# Cleaner first-boot menu: don't probe for Windows / other Linux
# installs on neighbouring partitions.
GRUB_DISABLE_OS_PROBER=true
# Recordfail timeout: avoid the 30s pause when the previous boot
# didn't shutdown cleanly (common in VMs after a forced power off).
GRUB_RECORDFAIL_TIMEOUT=2
EOF

echo "  :: /etc/default/grub.d/50-claw-os.cfg installed"
