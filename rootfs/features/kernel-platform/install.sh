#!/usr/bin/env bash
set -euo pipefail

platform="${CLAW_PLATFORM:?kernel-platform requires CLAW_PLATFORM}"
builder="$PROJECT_DIR/scripts/platform_kernels.py"
mode=()
if [[ $platform == aarch64-qualcomm && ,${FEATURES:-}, == *,installer,* ]]; then
    mode=(--board-tools)
fi
identity=$(python3 "$builder" "$platform" "${mode[@]}" --describe)
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
if (( ${#mode[@]} )); then
    install -d "$stage"/{scripts,packaging}
    for file in platform_kernels.py surface_laptop7.py; do
        install -m0644 "$PROJECT_DIR/scripts/$file" "$stage/scripts/"
    done
    install -m0644 "$PROJECT_DIR"/packaging/platform-kernels.json \
        "$PROJECT_DIR"/packaging/surface-romulus*.dtso \
        "$PROJECT_DIR"/packaging/surface-romulus-camss.patch "$stage/packaging/"
    chroot "$ROOTFS" apt-get update -qq
    chroot "$ROOTFS" env DEBIAN_FRONTEND=noninteractive apt-get -o Dpkg::Use-Pty=0 \
        install -y --no-install-recommends build-essential pkg-config systemd-dev libudev-dev \
        gpg gpgv zstd device-tree-compiler patch meson ninja-build cmake ca-certificates \
        bc bison flex cpio pahole libdw-dev libelf-dev libssl-dev zlib1g-dev cryptsetup-initramfs </dev/null
    chroot "$ROOTFS" env PYTHONDONTWRITEBYTECODE=1 python3 "${stage#"$ROOTFS"}/scripts/platform_kernels.py" \
        "$platform" "${mode[@]}" --output "${stage#"$ROOTFS"}/packages"
    artifacts="$stage/packages"
else
    python3 "$builder" "$platform" --output "$stage"
    artifacts=$stage
fi
chmod 0755 "$stage"
inside="${artifacts#"$ROOTFS"}"
packages=()
for file in "$artifacts"/linux-image-*.deb "$artifacts"/claw-os-nvidia-platform_*.deb; do
    [[ -f $file ]] && packages+=("$inside/${file##*/}")
done
if [[ $platform == x86-surface-laptop-7-intel ]]; then
    for file in "$artifacts"/{claw-os-surface-laptop-7,libcamera*,gstreamer1.0-libcamera}_*.deb; do
        test -f "$file"
        packages+=("$inside/${file##*/}")
    done
    packages+=("alsa-ucm-conf/$SUITE-backports" "libspa-0.2-libcamera/$SUITE-backports")
fi
if [[ $platform == aarch64-qualcomm ]]; then
    radio=("$artifacts"/claw-os-surface-snapdragon-radio_*.deb)
    test -f "${radio[0]}"
    packages+=("$inside/${radio[0]##*/}")
    if (( ${#mode[@]} )); then
        cache="$ROOTFS/usr/share/cos/driver-cache/surface-laptop-7-snapdragon"
        install -d "$cache"
        install -m0644 "$artifacts"/{claw-os-surface-snapdragon-touchpad,alsa-ucm-conf}_*.deb "$cache/"
    fi
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
if (( ${#mode[@]} )); then
    # Only the matching Surface entries can write or restore a hibernation image.
    printf '%s\n' 'export CLAW_QCOM_LINUX="${GRUB_CMDLINE_LINUX:-}"' \
        'GRUB_CMDLINE_LINUX="$CLAW_QCOM_LINUX noresume nohibernate"' \
        >>"$ROOTFS/etc/default/grub.d/60-claw-os-platform.cfg"
fi
if [[ -n $boot_args ]]; then
    printf 'GRUB_CMDLINE_LINUX="$GRUB_CMDLINE_LINUX %s"\n' "$boot_args" >>"$ROOTFS/etc/default/grub.d/60-claw-os-platform.cfg"
fi
test -s "$ROOTFS/boot/vmlinuz-$release"
test -s "$ROOTFS/boot/initrd.img-$release"
if [[ $platform == x86-surface-laptop-7-intel ]]; then
    chroot "$ROOTFS" lsinitramfs "/boot/initrd.img-$release" |
        grep -E '/updates/claw-surface/surface_aggregator_registry\.ko$' >/dev/null
fi
