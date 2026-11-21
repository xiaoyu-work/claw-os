#!/usr/bin/env bash
set -euo pipefail
umask 022

rootfs=$1
live=$2
profile=$3
assets=$(dirname "$0")
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
layer=$work/layer
payload=$work/payload
cache=$rootfs/usr/share/cos/driver-cache

copy_payload() {
    install -Dm"${2:-0644}" "$payload/$1" "$layer/$1"
}

case "$profile" in
    amd64)
        package=claw-os-surface-laptop-7
        dpkg-deb -x "$cache"/surface-laptop-7-intel/"${package}"_*.deb "$payload"
        copy_payload etc/iptsd.d/99-surface-laptop-7-touchpad.conf
        copy_payload usr/share/libinput/65-claw-surface.quirks
        ;;
    aarch64-qualcomm)
        package=claw-os-surface-snapdragon-iptsd
        dpkg-deb -x "$cache"/surface-laptop-7-snapdragon/"${package}"_*.deb "$payload"
        dpkg-deb -x "$cache"/surface-laptop-7-snapdragon/claw-os-surface-snapdragon-touchpad_*.deb "$payload"
        ;;
    x86-surface-laptop-7-intel)
        test -x "$rootfs/usr/bin/iptsd"
        test -f "$rootfs/usr/lib/udev/rules.d/50-iptsd.rules"
        ;;
    *) exit 1 ;;
esac

if [[ $profile != "x86-surface-laptop-7-intel" ]]; then
    copy_payload usr/bin/iptsd 0755
    copy_payload usr/bin/iptsd-check-device 0755
    copy_payload usr/lib/systemd/system/iptsd@.service
    install -d "$layer/usr/lib/udev/rules.d" "$layer/usr/share/doc"
    cp -a "$payload/usr/share/iptsd" "$layer/usr/share/"
    cp -a "$payload/usr/share/doc/$package" "$layer/usr/share/doc/"
    # Use the packaged probe rule, but only after the live service has matched the board.
    {
        printf '%s\n' 'TEST!="/run/claw-installer-surface-input", GOTO="claw_live_iptsd_end"'
        cat "$payload/usr/lib/udev/rules.d/50-iptsd.rules"
        printf '\n%s\n' 'LABEL="claw_live_iptsd_end"'
    } >"$layer/usr/lib/udev/rules.d/50-iptsd.rules"
fi

service=claw-installer-surface-input.service
units=$layer/usr/lib/systemd/system
install -Dm0644 "$assets/$service" "$units/$service"
install -d "$units/getty@tty1.service.wants"
ln -s "../$service" "$units/getty@tty1.service.wants/$service"
install -Dm0755 "$assets/claw-installer-surface-input" "$layer/usr/lib/cos/init/claw-installer-surface-input"
install -Dm0644 "$assets/90-claw-installer-surface-input.rules" "$layer/usr/lib/udev/rules.d/90-claw-installer-surface-input.rules"

# Calamares unpacks only filesystem.squashfs; live-boot mounts this layer above it.
mksquashfs "$layer" "$live/surface-input.squashfs" -comp zstd -noappend -all-root
printf '%s\n' filesystem.squashfs surface-input.squashfs >"$live/filesystem.module"
