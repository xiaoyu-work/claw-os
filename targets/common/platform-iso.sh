# Shared by the live and installer ISO targets.

select_iso_platform() {
    local role=$1 identity architecture
    ISO_FEATURES=$2
    PLATFORM_RELEASE=
    PLATFORM_SUFFIX=
    PLATFORM_BOOT_ARGS=
    if [[ -z ${CLAW_PLATFORM:-} ]]; then
        return
    fi
    identity=$(python3 "$PROJECT_DIR/scripts/platform_kernels.py" "$CLAW_PLATFORM" --describe) || return 1
    read -r architecture PLATFORM_RELEASE PLATFORM_BOOT_ARGS <<<"$identity"
    if [[ $architecture != "$DEB_ARCH" ]]; then
        echo "error: $CLAW_PLATFORM requires $architecture, not $DEB_ARCH" >&2
        return 1
    fi
    if [[ $CLAW_PLATFORM == aarch64-apple && $role == installer ]]; then
        echo "error: Apple Silicon needs existing Asahi provisioning; use the live media and separate Asahi boot assets" >&2
        return 1
    fi
    if [[ $CLAW_PLATFORM == x86-surface-laptop-7-intel ]]; then
        ISO_FEATURES="$ISO_FEATURES,desktop"
        if [[ $role == installer ]]; then
            # Keep the live tty1 installer; the installed system uses greetd.
            PLATFORM_BOOT_ARGS="${PLATFORM_BOOT_ARGS:+$PLATFORM_BOOT_ARGS }systemd.mask=greetd.service"
        fi
    fi
    ISO_FEATURES="$ISO_FEATURES,kernel-platform"
    PLATFORM_SUFFIX="-$CLAW_PLATFORM"
}

select_iso_kernel() {
    if [[ -n $PLATFORM_RELEASE ]]; then
        KERNEL="$ROOTFS/boot/vmlinuz-$PLATFORM_RELEASE"
    else
        KERNEL=$(find "$ROOTFS/boot" -maxdepth 1 -name 'vmlinuz-*' | sort -V | tail -1)
    fi
    KERNEL_VERSION=${KERNEL##*/vmlinuz-}
    INITRD="$ROOTFS/boot/initrd.img-$KERNEL_VERSION"
    if [[ ! -s $KERNEL || ! -s $INITRD ]]; then
        echo "error: matching kernel and initramfs not found in $ROOTFS/boot" >&2
        return 1
    fi
    echo ":: kernel: $KERNEL"
    echo ":: initrd: $INITRD"
}

finalize_iso_platform() {
    if [[ -n $PLATFORM_BOOT_ARGS ]]; then
        sed -i "s|boot=live components|boot=live components $PLATFORM_BOOT_ARGS|" "$ISO_BUILD/boot/grub/grub.cfg"
    fi
    if [[ ${CLAW_PLATFORM:-} == aarch64-apple ]]; then
        install -d "$ISO_BUILD/asahi"
        cp -a "$ROOTFS/usr/lib/cos/asahi/." "$ISO_BUILD/asahi/"
    fi
}
