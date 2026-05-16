#!/usr/bin/env bash
# targets/iso-live/build.sh — Build a bootable Live ISO.
#
# Output: build/claw-os-live-<arch>.iso  (arch from $ARCH, default = host)
#
# Features: base, cos-core, systemd, kernel, live  (browser is omitted —
# saves ~300MB; users can install via apt later).
#
# Boot modes:
#   amd64 → hybrid BIOS+UEFI (grub-mkrescue emits both boot paths)
#   arm64 → UEFI-only (ARM has no legacy BIOS firmware)
#
# HOST REQUIREMENTS (the build host needs these — they are not in the rootfs):
#   apt install squashfs-tools xorriso mtools grub-common
#   apt install grub-efi-<arch>-bin       # /usr/lib/grub/<arch>-efi modules
#   apt install grub-pc-bin               # amd64 only — /usr/lib/grub/i386-pc

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
ROOTFS="$PROJECT_DIR/build/claw-os-rootfs"
ISO_BUILD="$PROJECT_DIR/build/iso-build"

source "$PROJECT_DIR/scripts/lib/arch.sh"

OUTPUT="$PROJECT_DIR/build/claw-os-live-${ARCH_SUFFIX}.iso"

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

# grub-mkrescue uses these at build time. Missing the UEFI dir => no
# UEFI boot. On amd64 we also need the BIOS modules for the hybrid ISO.
required_grub_dirs=("/usr/lib/grub/$GRUB_EFI_TARGET")
[ -n "$GRUB_BIOS_TARGET" ] && required_grub_dirs+=("/usr/lib/grub/$GRUB_BIOS_TARGET")
for moddir in "${required_grub_dirs[@]}"; do
    if [ ! -d "$moddir" ]; then
        echo "error: $moddir is missing — apt install $GRUB_EFI_PKG${GRUB_BIOS_PKG:+ $GRUB_BIOS_PKG}" >&2
        exit 1
    fi
done

# 1. Build the rootfs.
#    apt-source pre-configures the Claw OS apt repo so users who later
#    install the system (via M8 installer) get apt upgrade out of the box.
#
#    FEATURES is overridable so callers can add `desktop` (and any other
#    optional feature) without forking this script. Example, build a live
#    ISO that boots straight into the COSMIC desktop:
#       FEATURES=base,cos-core,systemd,kernel,desktop,copilot-cli,live,apt-source \
#           ./targets/iso-live/build.sh
#    Note: rootfs/features/live/install.sh detects whether `desktop` was
#    already applied and, if so, layers a greetd [initial_session] block
#    for autologin into a Wayland session.
FEATURES="${FEATURES:-base,cos-core,systemd,kernel,live,apt-source}"
echo ":: features: $FEATURES"
"$PROJECT_DIR/rootfs/build.sh" --features "$FEATURES"

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

# 8. grub-mkrescue produces a bootable ISO. On amd64 it auto-emits a
#    hybrid BIOS+UEFI image when both /usr/lib/grub/{i386-pc,x86_64-efi}
#    are present; on arm64 only the UEFI path exists, so the ISO boots
#    via UEFI only.
echo ":: building $ARCH ISO via grub-mkrescue"
grub-mkrescue \
    --output="$OUTPUT" \
    --product-name="Claw OS Live" \
    "$ISO_BUILD" \
    -- \
    -volid "CLAWOS_LIVE"

SIZE=$(du -h "$OUTPUT" | cut -f1)
echo ":: done — $OUTPUT ($SIZE)"
echo
if [ "$ARCH" = "amd64" ]; then
    echo "Test (BIOS):"
    echo "  qemu-system-x86_64 -m 2G -cdrom $OUTPUT -nographic"
    echo "Test (UEFI):"
    echo "  qemu-system-x86_64 -m 2G -bios /usr/share/ovmf/OVMF.fd -cdrom $OUTPUT"
else
    echo "Test (UEFI, requires AAVMF):"
    echo "  qemu-system-aarch64 -M virt -cpu max -m 2G \\"
    echo "      -bios /usr/share/AAVMF/AAVMF_CODE.fd -cdrom $OUTPUT -nographic"
fi
