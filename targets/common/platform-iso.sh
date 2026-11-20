# Shared by the live and installer ISO targets.

select_iso_platform() {
    local role=$1 identity architecture
    local -a mode=()
    ISO_FEATURES=$2
    PLATFORM_RELEASE=
    PLATFORM_SUFFIX=
    PLATFORM_BOOT_ARGS=
    if [[ -z ${CLAW_PLATFORM:-} ]]; then
        if [[ $role == installer && $DEB_ARCH == amd64 && ,$ISO_FEATURES, == *,desktop,* ]]; then
            ISO_FEATURES=${ISO_FEATURES/,installer,/,surface-drivers,installer,}
        fi
        return
    fi
    if [[ $role == installer && $CLAW_PLATFORM == "aarch64-qualcomm" ]]; then
        mode=(--board-tools)
    fi
    identity=$(python3 "$PROJECT_DIR/scripts/platform_kernels.py" "$CLAW_PLATFORM" "${mode[@]}" --describe) || return 1
    read -r architecture PLATFORM_RELEASE PLATFORM_BOOT_ARGS <<<"$identity"
    if [[ $architecture != "$DEB_ARCH" ]]; then
        echo "error: $CLAW_PLATFORM requires $architecture, not $DEB_ARCH" >&2
        return 1
    fi
    if [[ $CLAW_PLATFORM == aarch64-apple && $role == installer ]]; then
        echo "error: Apple Silicon needs existing Asahi provisioning; use the live media and separate Asahi boot assets" >&2
        return 1
    fi
    if [[ $role == installer && $CLAW_PLATFORM == @(aarch64-qualcomm|x86-surface-laptop-7-intel) ]]; then
        ISO_FEATURES=${ISO_FEATURES/,installer,/,kernel-platform,installer,}
    else
        if [[ $CLAW_PLATFORM == x86-surface-laptop-7-intel && ,$ISO_FEATURES, != *,desktop,* ]]; then
            ISO_FEATURES="$ISO_FEATURES,desktop"
        fi
        ISO_FEATURES="$ISO_FEATURES,kernel-platform"
    fi
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
    local boot_args=$PLATFORM_BOOT_ARGS
    if [[ ${1:-live} == installer && ,$ISO_FEATURES, == *,desktop,* ]]; then
        # Calamares uses tty1 on the USB. Keep greetd enabled on the target.
        boot_args="${boot_args:+$boot_args }systemd.mask=greetd.service"
    fi
    if [[ ${1:-live} == installer && ${CLAW_PLATFORM:-} == aarch64-qualcomm ]]; then
        boot_args="$boot_args noresume nohibernate"
    fi
    if [[ -n $boot_args ]]; then
        sed -i "s|boot=live components|boot=live components $boot_args|" "$ISO_BUILD/boot/grub/grub.cfg"
    fi
    if [[ ${CLAW_PLATFORM:-} == aarch64-n1x ]]; then
        source "$PROJECT_DIR/rootfs/features/kernel-platform/n1x/n1x-boot.sh"
        local label="Claw OS Live"
        [[ ${1:-live} != installer ]] || label="Install Claw OS"
        cat >>"$ISO_BUILD/boot/grub/grub.cfg" <<EOF

menuentry "$label (N1x firmware framebuffer)" {
    linux /live/vmlinuz boot=live components $boot_args $claw_n1x_no_gpu
    initrd /live/initrd.img
}
EOF
    fi
    if [[ ${CLAW_PLATFORM:-} == aarch64-apple ]]; then
        install -d "$ISO_BUILD/asahi"
        cp -a "$ROOTFS/usr/lib/cos/asahi/." "$ISO_BUILD/asahi/"
    fi
}
