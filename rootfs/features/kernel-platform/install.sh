#!/usr/bin/env bash
set -euo pipefail

platform="${CLAW_PLATFORM:?kernel-platform requires CLAW_PLATFORM}"
builder="$PROJECT_DIR/scripts/platform_kernels.py"
identity=$(python3 "$builder" "$platform" --describe)
read -r architecture release boot_args <<<"$identity"
if [[ $architecture != "$DEB_ARCH" ]]; then
    echo "error: $platform requires $architecture target packages, not $DEB_ARCH" >&2
    exit 1
fi
if [[ ! -x "$ROOTFS/usr/sbin/update-initramfs" ]]; then
    echo "error: kernel-platform requires the kernel feature first" >&2
    exit 1
fi

stage=$(mktemp -d "$ROOTFS/tmp/claw-platform.XXXXXX")
trap 'rm -rf -- "$stage"' EXIT
python3 "$builder" "$platform" --output "$stage"
chmod 0755 "$stage"
inside="${stage#"$ROOTFS"}"
packages=()
for file in "$stage"/linux-image-*.deb "$stage"/claw-os-nvidia-platform_*.deb; do
    [[ -f $file ]] && packages+=("$inside/${file##*/}")
done
if [[ $platform == x86-surface-laptop-7-intel ]]; then
    for file in "$stage"/{claw-os-surface-laptop-7,libcamera*,gstreamer1.0-libcamera}_*.deb; do
        test -f "$file"
        packages+=("$inside/${file##*/}")
    done
    packages+=("alsa-ucm-conf/$SUITE-backports" "libspa-0.2-libcamera/$SUITE-backports")
fi
chroot "$ROOTFS" apt-get update -qq
chroot "$ROOTFS" env DEBIAN_FRONTEND=noninteractive apt-get -o Dpkg::Use-Pty=0 \
    install -y --no-install-recommends "${packages[@]}" </dev/null
chroot "$ROOTFS" apt-get clean

if [[ $platform == aarch64-apple ]]; then
    install -d "$ROOTFS/usr/lib/cos/asahi"
    install -m0644 "$stage"/boot{,-j700}.bin "$stage/platform-sources.json" "$ROOTFS/usr/lib/cos/asahi/"
    cp -a "$stage/boot-licenses" "$ROOTFS/usr/lib/cos/asahi/"
fi

if [[ $platform == aarch64-gb10 ]]; then
    install -d "$ROOTFS/etc/systemd/sleep.conf.d"
    cat > "$ROOTFS/etc/systemd/sleep.conf.d/60-claw-os-gb10.conf" <<'EOF'
[Sleep]
AllowSuspend=no
AllowHibernation=no
AllowSuspendThenHibernate=no
AllowHybridSleep=no
EOF
fi

install -d "$ROOTFS/etc/default/grub.d"
printf 'GRUB_TOP_LEVEL="/boot/vmlinuz-%s"\n' "$release" >"$ROOTFS/etc/default/grub.d/60-claw-os-platform.cfg"
if [[ -n $boot_args ]]; then
    printf 'GRUB_CMDLINE_LINUX="$GRUB_CMDLINE_LINUX %s"\n' "$boot_args" >>"$ROOTFS/etc/default/grub.d/60-claw-os-platform.cfg"
fi
test -s "$ROOTFS/boot/vmlinuz-$release"
test -s "$ROOTFS/boot/initrd.img-$release"
if [[ $platform == x86-surface-laptop-7-intel ]]; then
    chroot "$ROOTFS" lsinitramfs "/boot/initrd.img-$release" |
        grep -E '/updates/claw-surface/surface_aggregator_registry\.ko$' >/dev/null
fi
