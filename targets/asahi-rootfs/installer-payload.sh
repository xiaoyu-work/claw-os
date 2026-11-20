#!/usr/bin/env bash
set -euo pipefail
umask 022

rootfs=${1:?missing Asahi rootfs}
release=${2:?missing Aurora release}
output=${3:?missing output path}
directory=$(realpath -e -- "$(dirname -- "$output")")
output="$directory/$(basename -- "$output")"
[[ $release =~ ^[[:alnum:].+-]+$ ]]
for artifact in "boot/vmlinuz-$release" "boot/initrd.img-$release" \
    usr/lib/cos/asahi/{boot,boot-j700,stage1-j700}.bin; do
    test -s "$rootfs/$artifact"
done
test -f "$rootfs/etc/cos/local-desktop-setup"

work=$(mktemp -d "$directory/.claw-asahi-payload.XXXXXX")
trap 'rm -rf -- "$work"' EXIT
root="$work/root"
esp="$work/esp"
install -d "$root" "$esp"/{EFI/BOOT,m1n1}
rsync -aHAXx --numeric-ids "$rootfs/" "$root/"

uuid=$(cat /proc/sys/kernel/random/uuid)
rm -f -- "$root/etc/fstab"
printf 'UUID=%s / ext4 defaults,errors=remount-ro,x-systemd.growfs 0 1\n' "$uuid" > "$root/etc/fstab"
install -d "$root/boot/grub/arm64-efi"
cp -a "$root/usr/lib/grub/arm64-efi/." "$root/boot/grub/arm64-efi/"
cat > "$root/boot/grub/grub.cfg" <<EOF
set timeout=2
menuentry 'Claw OS' {
    linux /boot/vmlinuz-$release root=UUID=$uuid ro quiet splash
    initrd /boot/initrd.img-$release
}
EOF

# U-Boot starts the removable EFI loader. Keeping the menu on Linux root makes
# later update-grub runs independent of the ESP and Apple's boot policy.
grub_work=$(mktemp -d "$root/tmp/.claw-asahi-grub.XXXXXX")
inside=${grub_work#"$root"}
cat > "$grub_work/config" <<EOF
search --no-floppy --fs-uuid --set=root $uuid
set prefix=(\$root)/boot/grub
configfile (\$root)/boot/grub/grub.cfg
EOF
chroot "$root" /usr/bin/grub-script-check /boot/grub/grub.cfg
chroot "$root" /usr/bin/grub-script-check "$inside/config"
chroot "$root" /usr/bin/grub-mkstandalone --format=arm64-efi --locales='' --fonts='' --themes='' \
    --modules='part_gpt ext2 fat normal configfile linux search search_fs_uuid' \
    --output="$inside/BOOTAA64.EFI" "boot/grub/grub.cfg=$inside/config"
install -m0644 "$grub_work/BOOTAA64.EFI" "$esp/EFI/BOOT/"
cp "$root/usr/lib/cos/asahi"/boot{,-j700}.bin "$esp/m1n1/"
mv -- "$root/usr/lib/cos/asahi/stage1-j700.bin" "$work/stage1-j700.bin"
rm -f -- "$grub_work/config" "$grub_work/BOOTAA64.EFI"
rmdir -- "$grub_work"

# Build only a regular filesystem image, not a disk or partition table.
read -r bytes _ < <(du -sxB1 "$root")
read -r inodes _ < <(du -sx --inodes "$root")
mib=$(( (bytes + bytes / 4 + 1048575) / 1048576 + 128 ))
truncate -s "${mib}M" "$work/root.img"
mkfs.ext4 -q -F -U "$uuid" -N "$(( inodes + inodes / 4 + 1024 ))" -d "$root" "$work/root.img"
(
    cd "$work"
    zip -q -r payload.zip root.img esp stage1-j700.bin
)
chmod 0644 "$work/payload.zip"
mv -fT -- "$work/payload.zip" "$output"
echo ":: wrote $output"
