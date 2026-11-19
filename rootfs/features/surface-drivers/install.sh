#!/usr/bin/env bash
set -euo pipefail

[[ $DEB_ARCH == amd64 && -z ${CLAW_PLATFORM:-} ]] || exit 0

cache=/usr/share/cos/driver-cache/surface-laptop-7-intel
work=$(mktemp -d "$ROOTFS/tmp/claw-surface.XXXXXX")
trap 'rm -rf -- "$work"' EXIT
inside=${work#"$ROOTFS"}
chmod 0755 "$work"
install -d "$ROOTFS$cache"

version=$(find "$ROOTFS/boot" -maxdepth 1 -name 'vmlinuz-*' -printf '%f\n' | sort -V | tail -n 1)
version=${version#vmlinuz-}
revision=$(chroot "$ROOTFS" dpkg-query -W -f='${Version}' "linux-image-$version")
apt=(chroot "$ROOTFS" env DEBIAN_FRONTEND=noninteractive apt-get -o Dpkg::Use-Pty=0)
"${apt[@]}" update -qq
"${apt[@]}" install -y --no-install-recommends \
    "linux-headers-$version=$revision" bc libelf-dev libssl-dev systemd-dev libudev-dev patch ca-certificates \
    debhelper dh-sequence-python3 libdrm-dev libdw-dev libevent-dev libgnutls28-dev \
    libgstreamer-plugins-base1.0-dev libgstreamer1.0-dev libjpeg-dev liblttng-ust-dev \
    libpython3-dev libsdl2-dev libtiff-dev libyaml-dev libyuv-dev openssl \
    python3-jinja2 python3-ply python3-pybind11 python3-yaml qt6-base-dev </dev/null
for file in platform_kernels.py surface_laptop7.py; do
    install -Dm0644 "$PROJECT_DIR/scripts/$file" "$work/scripts/$file"
done
install -Dm0644 "$PROJECT_DIR/packaging/platform-kernels.json" "$work/packaging/platform-kernels.json"
chroot "$ROOTFS" env PYTHONDONTWRITEBYTECODE=1 python3 "$inside/scripts/platform_kernels.py" \
    x86-surface-laptop-7-intel --debian-kernel "$version" --output "$cache"
install -d "$ROOTFS/usr/lib/modules/$version/updates/claw-surface"
dpkg-deb --fsys-tarfile "$ROOTFS$cache"/claw-os-surface-laptop-7_*.deb |
    tar --extract --to-stdout "./usr/lib/modules/$version/updates/claw-surface/surface_aggregator_registry.ko" \
    >"$ROOTFS/usr/lib/modules/$version/updates/claw-surface/surface_aggregator_registry.ko"
# This file survives media initramfs regeneration, but is removed on installation.
install -d "$ROOTFS/etc/modprobe.d"
printf '%s\n' 'softdep surface_aggregator pre: pinctrl_intel_platform' \
    >"$ROOTFS/etc/modprobe.d/60-claw-surface-media.conf"
printf '%s\n' "$version" >"$ROOTFS$cache/kernel-release"
chroot "$ROOTFS" depmod -a "$version"
chroot "$ROOTFS" update-initramfs -u -k "$version"
initrd=$(chroot "$ROOTFS" lsinitramfs "/boot/initrd.img-$version")
grep -Eq "(^|/)lib/modules/$version/updates/claw-surface/surface_aggregator_registry\.ko$" <<<"$initrd"
