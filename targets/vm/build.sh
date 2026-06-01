#!/usr/bin/env bash
# targets/vm/build.sh — Build a persistent VM disk image.
#
# Output: build/claw-os-vm-<arch>.<fmt>  for each fmt in $FORMATS
#         (default fmt: qcow2; arch: amd64 or arm64, host-detected).
#
# Environment:
#   ARCH     amd64 (default on x86_64 hosts) | arm64 (default on aarch64).
#            See scripts/lib/arch.sh for the full architecture mapping.
#            claw-os builds natively only — $ARCH must match the host arch.
#   FORMATS  Space-separated output formats. Default: "qcow2".
#            Supported: qcow2, vmdk, vhdx, raw
#   SIZE     Virtual disk size. Default: "8G". Image is sparse, so the
#            actual file is much smaller (qcow2 typically ~2 GB for an
#            8 GB disk with stock claw-os contents).
#
# Disk layout (GPT):
#   amd64 (hybrid BIOS+UEFI bootable):
#     1MiB-2MiB     bios_grub          (raw, no fs)
#     2MiB-258MiB   ESP                (fat32, /boot/efi)
#     258MiB-100%   root               (ext4, /)
#   arm64 (UEFI-only; ARM has no legacy BIOS path):
#     1MiB-257MiB   ESP                (fat32, /boot/efi)
#     257MiB-100%   root               (ext4, /)
#
# Host requirements (Debian/Ubuntu):
#   apt install qemu-utils parted dosfstools rsync util-linux
#   apt install grub-efi-<arch>-bin    # arch-specific UEFI grub modules
#   apt install grub-pc-bin            # amd64 only — BIOS grub modules
#   (mkfs.ext4 is in e2fsprogs which is always installed)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
ROOTFS="$PROJECT_DIR/build/claw-os-rootfs"
BUILD_DIR="$PROJECT_DIR/build"

# Architecture mapping ($ARCH, $DEB_ARCH, $GRUB_EFI_TARGET, …). Defaults
# to host arch when $ARCH is unset.
source "$PROJECT_DIR/scripts/lib/arch.sh"

# Output filenames are arch-suffixed so amd64 + arm64 builds coexist in
# build/. The intermediate raw image is also suffixed to avoid races.
RAW="$BUILD_DIR/claw-os-vm-${ARCH_SUFFIX}.raw"
MNT="$BUILD_DIR/vm-mnt"

FORMATS="${FORMATS:-qcow2}"
SIZE="${SIZE:-8G}"

if [ "$(id -u)" -ne 0 ]; then
    echo "error: must run as root (losetup, mount, mkfs need it)" >&2
    exit 1
fi

# Host tooling.
missing=""
for tool in qemu-img parted mkfs.ext4 mkfs.vfat losetup rsync blkid; do
    command -v "$tool" >/dev/null 2>&1 || missing="$missing $tool"
done
if [ -n "$missing" ]; then
    echo "error: missing host tools:$missing" >&2
    echo "install: apt install qemu-utils parted dosfstools rsync util-linux" >&2
    exit 1
fi

# 1. Build the rootfs (ARCH propagates via env).
#    apt-source pre-configures the Claw OS apt repo so users can run
#    `sudo apt update && sudo apt upgrade` to pull newer claw-os-* packages.
#
#    FEATURES is overridable. Example, build a desktop VMware image:
#       FEATURES=base,cos-core,systemd,kernel,desktop,vmware,copilot-cli,grub-disk,vm,apt-source \
#       SIZE=16G FORMATS=vhdx ./targets/vm/build.sh
FEATURES="${FEATURES:-base,cos-core,systemd,kernel,grub-disk,vm,gpu-drivers,apt-source}"
echo ":: features: $FEATURES"
"$PROJECT_DIR/rootfs/build.sh" --features "$FEATURES"

# 2. Reset previous build artefacts.
rm -f "$RAW"
rm -rf "$MNT"
mkdir -p "$MNT"

# 3. Create raw disk + partition table.
#
#    Layout depends on $ARCH:
#      amd64  → bios_grub (1-2MiB) + ESP (2-258MiB) + root (258MiB-100%)
#               so the same image boots on legacy BIOS *and* UEFI.
#      arm64  → ESP (1-257MiB) + root (257MiB-100%).  ARM has no BIOS
#               path; bios_grub would just waste a partition slot.
echo ":: creating $SIZE raw image ($ARCH)"
qemu-img create -f raw "$RAW" "$SIZE"

if [ -n "$GRUB_BIOS_TARGET" ]; then
    echo ":: partitioning (GPT: bios_grub + ESP + root)"
    parted -s "$RAW" \
        mklabel gpt \
        mkpart bios_grub 1MiB 2MiB \
        set 1 bios_grub on \
        mkpart ESP fat32 2MiB 258MiB \
        set 2 esp on \
        mkpart root ext4 258MiB 100%
    ESP_PART=2
    ROOT_PART=3
else
    echo ":: partitioning (GPT: ESP + root — UEFI-only, no BIOS path on $ARCH)"
    parted -s "$RAW" \
        mklabel gpt \
        mkpart ESP fat32 1MiB 257MiB \
        set 1 esp on \
        mkpart root ext4 257MiB 100%
    ESP_PART=1
    ROOT_PART=2
fi

# 4. Attach as loop device with partition scanning so /dev/loopXpN
#    appear automatically. -P is essential — without it grub-probe inside
#    the chroot will fail to find the partition layout.
LOOP=$(losetup -Pf --show "$RAW")
ESP_DEV="${LOOP}p${ESP_PART}"
ROOT_DEV="${LOOP}p${ROOT_PART}"
echo ":: attached $RAW at $LOOP (ESP=$ESP_DEV root=$ROOT_DEV)"

# Set up cleanup early so any failure unmounts and detaches.
cleanup() {
    set +e
    # Lazy fallback: /sys is rbind'd (cgroup2 etc.) and can be "busy".
    [ -d "$MNT" ] && { umount -R "$MNT" 2>/dev/null || umount -Rl "$MNT" 2>/dev/null; }
    [ -n "${LOOP:-}" ] && losetup -d "$LOOP" 2>/dev/null
}
trap cleanup EXIT

# Wait briefly for udev to settle (partition device nodes can lag).
for _ in 1 2 3 4 5; do
    [ -b "$ESP_DEV" ] && [ -b "$ROOT_DEV" ] && break
    sleep 1
done
if [ ! -b "$ROOT_DEV" ]; then
    echo "error: partition device $ROOT_DEV not present — losetup -P likely failed" >&2
    exit 1
fi

# 5. Format.
echo ":: mkfs.vfat (ESP) on $ESP_DEV"
mkfs.vfat -F32 -n ESP "$ESP_DEV"
echo ":: mkfs.ext4 (root) on $ROOT_DEV"
mkfs.ext4 -L ROOT -F "$ROOT_DEV"

# 6. Mount target.
mount "$ROOT_DEV" "$MNT"
mkdir -p "$MNT/boot/efi"
mount "$ESP_DEV" "$MNT/boot/efi"

# 7. Copy rootfs (preserve hardlinks, ACLs, xattrs; --numeric-ids keeps
#    file ownership stable across host UID schemes).
echo ":: rsync rootfs -> mounted image"
rsync -aHAX --numeric-ids "$ROOTFS/" "$MNT/"

# 8. Write /etc/fstab from blkid UUIDs (preserved through qemu-img convert).
ROOT_UUID=$(blkid -o value -s UUID "$ROOT_DEV")
ESP_UUID=$(blkid -o value -s UUID "$ESP_DEV")
cat > "$MNT/etc/fstab" <<EOF
# /etc/fstab — generated by targets/vm/build.sh
UUID=$ROOT_UUID  /          ext4  defaults,errors=remount-ro  0  1
UUID=$ESP_UUID   /boot/efi  vfat  umask=0077                  0  1
EOF

# 9. Bind mounts for chroot grub-install.
#    /dev must be visible so grub-probe can scan /dev/loop*p* devices.
mount --rbind /dev  "$MNT/dev"
mount --rbind /sys  "$MNT/sys"
mount -t proc proc  "$MNT/proc"
# Confine these binds to this chroot's propagation. /dev (and its /dev/pts
# submount) is `shared` on the host; the recursive `umount -Rl` in the unwind
# below would otherwise propagate the unmount back to the host peer group and
# detach the host's /dev/pts, breaking PTY allocation host-wide ("sudo: unable
# to open pty") until `wsl --shutdown`. Make them private (recursively) so the
# cleanup only affects this image.
mount --make-rprivate "$MNT/dev"
mount --make-rprivate "$MNT/sys"
mount --make-rprivate "$MNT/proc"

# 10. Install GRUB.
#     amd64 → BIOS + UEFI (hybrid). The bios_grub partition makes the
#             same image bootable on legacy firmware.
#     arm64 → UEFI only (--removable writes /EFI/BOOT/BOOTAA64.EFI).
if [ -n "$GRUB_BIOS_TARGET" ]; then
    echo ":: grub-install --target=$GRUB_BIOS_TARGET $LOOP (BIOS)"
    chroot "$MNT" grub-install --target="$GRUB_BIOS_TARGET" "$LOOP"
fi

echo ":: grub-install --target=$GRUB_EFI_TARGET --removable --no-nvram (UEFI fallback path)"
# --removable writes the EFI fallback binary (BOOTX64.EFI / BOOTAA64.EFI)
# that every firmware tries. --no-nvram skips efibootmgr — essential when
# building on a BIOS host or in a chroot without /sys/firmware/efi.
chroot "$MNT" grub-install \
    --target="$GRUB_EFI_TARGET" \
    --efi-directory=/boot/efi \
    --bootloader-id=claw-os \
    --removable \
    --no-nvram

echo ":: update-grub (writes /boot/grub/grub.cfg)"
chroot "$MNT" update-grub

# 11. Unwind.
sync
# Unmount the bind-mounted pseudo-filesystems first. /sys is rbind'd and so
# carries cgroup2 (and other submounts) into $MNT/sys; those can report
# "target is busy" under a plain `umount -R`, which would abort the build
# (set -e) right before the raw->vmdk conversion. Detach them with a lazy
# fallback. The real filesystems (root ext4 + ESP) are then unmounted
# normally — NOT lazily — so the image is fully flushed before conversion.
for pseudo in dev sys proc; do
    umount -R "$MNT/$pseudo" 2>/dev/null || umount -Rl "$MNT/$pseudo" 2>/dev/null || true
done
# Restore host /dev/pts/ptmx mode in case the chroot reset it (see make-rprivate
# note above) — keeps host PTY allocation working after the build.
[ -e /dev/pts/ptmx ] && chmod 666 /dev/pts/ptmx 2>/dev/null || true
umount "$MNT/boot/efi"
umount "$MNT"
losetup -d "$LOOP"
LOOP=""
trap - EXIT
rmdir "$MNT"

# 12. Convert raw -> requested formats. qemu-img convert preserves
#     filesystem UUIDs (block-level copy), so fstab UUID= entries stay
#     valid in every output. qcow2 is auto-sparse. All outputs include
#     the arch suffix so amd64 + arm64 builds coexist in build/.
for fmt in $FORMATS; do
    case "$fmt" in
        raw)
            # $RAW already lives at the canonical arch-suffixed name; nothing
            # to do, just ensure we don't unlink it below.
            :
            ;;
        qcow2|vmdk|vhdx)
            OUT="$BUILD_DIR/claw-os-vm-${ARCH_SUFFIX}.$fmt"
            echo ":: qemu-img convert -f raw -O $fmt -> $OUT"
            qemu-img convert -f raw -O "$fmt" -S 65536 "$RAW" "$OUT"
            ;;
        *)
            echo "warning: unknown format '$fmt', skipping" >&2
            ;;
    esac
done

# If only non-raw formats requested, drop the intermediate raw.
case " $FORMATS " in
    *" raw "*) : ;;
    *) rm -f "$RAW" ;;
esac

echo
echo ":: done ($ARCH)"
ls -lh "$BUILD_DIR"/claw-os-vm-${ARCH_SUFFIX}.* 2>/dev/null

if [ "$ARCH" = "amd64" ]; then
    cat <<'EOF'

Test in QEMU (BIOS, default):
  qemu-system-x86_64 -m 2G -nographic \
      -drive file=build/claw-os-vm-amd64.qcow2,format=qcow2,if=virtio

Test in QEMU (UEFI, requires ovmf):
  qemu-system-x86_64 -m 2G -bios /usr/share/ovmf/OVMF.fd \
      -drive file=build/claw-os-vm-amd64.qcow2,format=qcow2,if=virtio

Hyper-V Gen 2 note:
  Gen 2 enables Secure Boot by default. Disable it before first boot:
    Set-VMFirmware -VMName claw-os -EnableSecureBoot Off
  Alternatively, attach to a Gen 1 VM (BIOS) — same .vhdx works.

VMware:
  Build with the optional 'vmware' feature for guest resize / clipboard:
    sudo FEATURES=base,cos-core,systemd,kernel,desktop,vmware,copilot-cli,grub-disk,vm,apt-source \
      FORMATS=vmdk ./build.sh vm
  Then create a new VM, choose "I will install the OS later", point the
  existing virtual disk at build/claw-os-vm-amd64.vmdk.
EOF
else
    cat <<'EOF'

Test in QEMU (UEFI, requires AAVMF):
  qemu-system-aarch64 -M virt -cpu max -m 2G \
      -bios /usr/share/AAVMF/AAVMF_CODE.fd \
      -drive file=build/claw-os-vm-arm64.qcow2,format=qcow2,if=virtio \
      -nographic

UTM (Apple Silicon):
  New → Virtualize → Linux → "Import VHD/QCOW2/IMG/RAW image" →
  point at build/claw-os-vm-arm64.qcow2. Architecture: ARM64.
  Use UEFI boot (default).

Parallels / VMware Fusion (Apple Silicon):
  For VMware guest resize / clipboard, build with the optional 'vmware' feature:
    sudo FEATURES=base,cos-core,systemd,kernel,desktop,vmware,copilot-cli,grub-disk,vm,apt-source \
      FORMATS=vmdk ./build.sh vm
  Then create a new ARM Linux VM, attach the .vmdk as the existing disk.
EOF
fi
