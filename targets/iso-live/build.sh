#!/usr/bin/env bash
# targets/iso-live/build.sh — Build a hybrid BIOS+UEFI Live ISO.
#
# Output: build/claw-os-live-amd64.iso
#
# Features: base, cos-core, systemd, kernel, live  (browser is omitted —
# saves ~300MB; users can install via apt later).
#
# HOST REQUIREMENTS (the build host needs these — they are not in the rootfs):
#   apt install squashfs-tools xorriso mtools \
#               grub-pc-bin grub-efi-amd64-bin grub-common
#
# grub-mkrescue uses host-side GRUB module directories at build time
# (/usr/lib/grub/i386-pc and /usr/lib/grub/x86_64-efi). We check for both
# so the resulting ISO is bootable on BIOS *and* UEFI.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
ROOTFS="$PROJECT_DIR/build/claw-os-rootfs"
ISO_BUILD="$PROJECT_DIR/build/iso-build"
OUTPUT="$PROJECT_DIR/build/claw-os-live-amd64.iso"

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
    echo "install: apt install squashfs-tools xorriso grub-pc-bin grub-efi-amd64-bin mtools grub-common" >&2
    exit 1
fi

# grub-mkrescue uses these at build time. Missing one => single-boot ISO.
for moddir in /usr/lib/grub/i386-pc /usr/lib/grub/x86_64-efi; do
    if [ ! -d "$moddir" ]; then
        echo "error: $moddir is missing — apt install grub-pc-bin grub-efi-amd64-bin" >&2
        exit 1
    fi
done

# 1. Build the rootfs.
#    apt-source pre-configures the Claw OS apt repo so users who later
#    install the system (via M8 installer) get apt upgrade out of the box.
"$PROJECT_DIR/rootfs/build.sh" --features base,cos-core,systemd,kernel,live,apt-source

# 2. Apply iso-live overlay if any.
if [ -d "$SCRIPT_DIR/overlay" ]; then
    cp -a "$SCRIPT_DIR/overlay/." "$ROOTFS/"
fi

# 3. Locate kernel + initrd in the rootfs (linux-image-amd64 installs both).
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

# 5. Build squashfs. Excluding /boot avoids double-shipping the kernel
#    (it lives at /live/vmlinuz on the ISO, alongside filesystem.squashfs).
echo ":: mksquashfs (this takes a few minutes)"
mksquashfs "$ROOTFS" "$ISO_BUILD/live/filesystem.squashfs" \
    -comp zstd -Xcompression-level 19 \
    -e boot \
    -noappend

cp "$KERNEL" "$ISO_BUILD/live/vmlinuz"
cp "$INITRD" "$ISO_BUILD/live/initrd.img"

# 6. GRUB config.
cat > "$ISO_BUILD/boot/grub/grub.cfg" <<'GRUBCFG'
set default=0
set timeout=5
insmod all_video

menuentry "Claw OS Live" {
    linux  /live/vmlinuz boot=live components quiet splash
    initrd /live/initrd.img
}

menuentry "Claw OS Live (debug)" {
    linux  /live/vmlinuz boot=live components debug
    initrd /live/initrd.img
}

menuentry "Claw OS Live (failsafe)" {
    linux  /live/vmlinuz boot=live components noapic noapm acpi=off
    initrd /live/initrd.img
}
GRUBCFG

# 7. Sanity: verify the squashfs layout before invoking grub-mkrescue.
for f in live/filesystem.squashfs live/vmlinuz live/initrd.img boot/grub/grub.cfg; do
    if [ ! -s "$ISO_BUILD/$f" ]; then
        echo "error: $ISO_BUILD/$f missing or empty" >&2
        exit 1
    fi
done

# 8. grub-mkrescue produces a hybrid BIOS+UEFI bootable ISO in one shot.
echo ":: building hybrid ISO via grub-mkrescue"
grub-mkrescue \
    --output="$OUTPUT" \
    --product-name="Claw OS Live" \
    "$ISO_BUILD" \
    -- \
    -volid "CLAWOS_LIVE"

SIZE=$(du -h "$OUTPUT" | cut -f1)
echo ":: done — $OUTPUT ($SIZE)"
echo
echo "Test (BIOS):"
echo "  qemu-system-x86_64 -m 2G -cdrom $OUTPUT -nographic"
echo "Test (UEFI):"
echo "  qemu-system-x86_64 -m 2G -bios /usr/share/ovmf/OVMF.fd -cdrom $OUTPUT"
