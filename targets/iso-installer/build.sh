#!/usr/bin/env bash
# targets/iso-installer/build.sh — Build an installable ISO.
#
# Output: build/claw-os-installer-<arch>.iso  (arch from $ARCH).
#
# Differs from iso-live in three ways:
#  1. Adds `installer` feature → Calamares + minimal X + autostart-on-login.
#  2. Adds `apt-source` feature → installed system gets `apt upgrade` repo.
#  3. GRUB menu entry text is "Install Claw OS" (vs "Claw OS Live").
#
# Both ISOs share the same Live-boot mechanism. After install completes,
# Calamares writes a fresh /etc/fstab and grub.cfg, then reboots — the
# new system is a permanent disk install.
#
# Boot modes:
#   amd64 → hybrid BIOS+UEFI
#   arm64 → UEFI-only
#
# Host requirements (Debian/Ubuntu):
#   apt install squashfs-tools xorriso mtools grub-common
#   apt install grub-efi-<arch>-bin       # /usr/lib/grub/<arch>-efi modules
#   apt install grub-pc-bin               # amd64 only — BIOS modules

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
ROOTFS="$PROJECT_DIR/build/claw-os-rootfs"
ISO_BUILD="$PROJECT_DIR/build/iso-installer-build"

source "$PROJECT_DIR/scripts/lib/arch.sh"

OUTPUT="$PROJECT_DIR/build/claw-os-installer-${ARCH_SUFFIX}.iso"

if [ "$(id -u)" -ne 0 ]; then
    echo "error: must run as root" >&2
    exit 1
fi

# Host tooling.
missing=""
for tool in mksquashfs xorriso grub-mkrescue mformat; do
    command -v "$tool" >/dev/null 2>&1 || missing="$missing $tool"
done
if [ -n "$missing" ]; then
    echo "error: missing host tools:$missing" >&2
    echo "install: apt install squashfs-tools xorriso $GRUB_EFI_PKG ${GRUB_BIOS_PKG:-} mtools grub-common" >&2
    exit 1
fi

# grub-mkrescue uses these at build time. arm64 has no BIOS path so we
# only require the UEFI module dir there.
required_grub_dirs=("/usr/lib/grub/$GRUB_EFI_TARGET")
[ -n "$GRUB_BIOS_TARGET" ] && required_grub_dirs+=("/usr/lib/grub/$GRUB_BIOS_TARGET")
for moddir in "${required_grub_dirs[@]}"; do
    if [ ! -d "$moddir" ]; then
        echo "error: $moddir is missing — apt install $GRUB_EFI_PKG${GRUB_BIOS_PKG:+ $GRUB_BIOS_PKG}" >&2
        exit 1
    fi
done

# 1. Build the rootfs.
#    Includes apt-source so the installed system can apt upgrade out of
#    the box, AND grub-disk so Calamares finds grub-install/efibootmgr.
"$PROJECT_DIR/rootfs/build.sh" \
    --features base,cos-core,systemd,kernel,grub-disk,live,installer,apt-source

# 2. Apply iso-installer overlay if any.
if [ -d "$SCRIPT_DIR/overlay" ]; then
    cp -a "$SCRIPT_DIR/overlay/." "$ROOTFS/"
fi

# 3. Locate kernel + initrd.
KERNEL=$(find "$ROOTFS/boot" -name 'vmlinuz-*' | sort | tail -1)
INITRD=$(find "$ROOTFS/boot" -name 'initrd.img-*' | sort | tail -1)
if [ -z "$KERNEL" ] || [ -z "$INITRD" ]; then
    echo "error: kernel or initrd not found in $ROOTFS/boot" >&2
    exit 1
fi
echo ":: kernel: $KERNEL"
echo ":: initrd: $INITRD"

# 4. Reset ISO build dir.
rm -rf "$ISO_BUILD"
mkdir -p "$ISO_BUILD/live" "$ISO_BUILD/boot/grub"

# 5. Squashfs (exclude /boot to avoid double-shipping the kernel).
echo ":: mksquashfs (this takes a few minutes)"
mksquashfs "$ROOTFS" "$ISO_BUILD/live/filesystem.squashfs" \
    -comp zstd -Xcompression-level 19 \
    -e boot \
    -noappend

cp "$KERNEL" "$ISO_BUILD/live/vmlinuz"
cp "$INITRD" "$ISO_BUILD/live/initrd.img"

# 6. GRUB config — different menu text from the headless live ISO.
cat > "$ISO_BUILD/boot/grub/grub.cfg" <<'GRUBCFG'
set default=0
set timeout=10
insmod all_video

menuentry "Install Claw OS" {
    linux  /live/vmlinuz boot=live components quiet splash
    initrd /live/initrd.img
}

menuentry "Install Claw OS (debug)" {
    linux  /live/vmlinuz boot=live components debug
    initrd /live/initrd.img
}

menuentry "Try Claw OS Live (skip installer)" {
    linux  /live/vmlinuz boot=live components quiet splash systemd.unit=multi-user.target
    initrd /live/initrd.img
}

menuentry "Install Claw OS (failsafe)" {
    linux  /live/vmlinuz boot=live components noapic noapm acpi=off
    initrd /live/initrd.img
}
GRUBCFG

# 7. Sanity check.
for f in live/filesystem.squashfs live/vmlinuz live/initrd.img boot/grub/grub.cfg; do
    if [ ! -s "$ISO_BUILD/$f" ]; then
        echo "error: $ISO_BUILD/$f missing or empty" >&2
        exit 1
    fi
done

# 8. Build ISO.
echo ":: building $ARCH ISO via grub-mkrescue"
grub-mkrescue \
    --output="$OUTPUT" \
    --product-name="Claw OS Installer" \
    "$ISO_BUILD" \
    -- \
    -volid "CLAWOS_INST"

SIZE=$(du -h "$OUTPUT" | cut -f1)
echo ":: done — $OUTPUT ($SIZE)"
echo
if [ "$ARCH" = "amd64" ]; then
    echo "Test (BIOS, with an attached blank disk to install into):"
    echo "  qemu-img create -f qcow2 build/claw-os-target.qcow2 16G"
    echo "  qemu-system-x86_64 -m 4G \\"
    echo "      -cdrom $OUTPUT \\"
    echo "      -drive file=build/claw-os-target.qcow2,format=qcow2,if=virtio \\"
    echo "      -boot d"
    echo
    echo "Test (UEFI, requires ovmf):"
    echo "  qemu-system-x86_64 -m 4G \\"
    echo "      -bios /usr/share/ovmf/OVMF.fd \\"
    echo "      -cdrom $OUTPUT \\"
    echo "      -drive file=build/claw-os-target.qcow2,format=qcow2,if=virtio"
else
    echo "Test (UEFI, requires AAVMF):"
    echo "  qemu-img create -f qcow2 build/claw-os-target.qcow2 16G"
    echo "  qemu-system-aarch64 -M virt -cpu max -m 4G \\"
    echo "      -bios /usr/share/AAVMF/AAVMF_CODE.fd \\"
    echo "      -cdrom $OUTPUT \\"
    echo "      -drive file=build/claw-os-target.qcow2,format=qcow2,if=virtio \\"
    echo "      -nographic"
fi
