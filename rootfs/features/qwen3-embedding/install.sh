#!/usr/bin/env bash
# Bundle the validated Qwen3 embedding stack into the rootfs.

set -euo pipefail

source "$PROJECT_DIR/scripts/lib/arch.sh"

qwen3_ort_genai_supported() {
    case "$DEB_ARCH" in
        amd64)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

ORT_GENAI_VERSION="$(
    sed -n 's/^pub const ORT_GENAI_KNOWN_GOOD_VERSION: &str = "\(.*\)";/\1/p' \
        "$PROJECT_DIR/core/src/engine_pkg/mod.rs" | head -1
)"
ORT_GENAI_TAG="v${ORT_GENAI_VERSION:-0.12.2}"

if ! qwen3_ort_genai_supported && [ "${COS_QWEN3_SKIP_ORT_GENAI:-0}" != "1" ]; then
    echo "  :: skipping qwen3-embedding on $DEB_ARCH: $ORT_GENAI_TAG has no Linux $DEB_ARCH CPU release asset"
    echo "     set COS_QWEN3_SKIP_ORT_GENAI=1 to install the model files without an engine runtime"
    exit 0
fi

"$PROJECT_DIR/scripts/download-qwen3-embedding-model.sh" --rootfs "$ROOTFS"

if [ "${COS_QWEN3_SKIP_ORT_GENAI:-0}" != "1" ]; then
    MOUNTED_BY_US=()
    cleanup_mounts() {
        local mp
        for (( idx=${#MOUNTED_BY_US[@]}-1 ; idx>=0 ; idx-- )); do
            mp="${MOUNTED_BY_US[$idx]}"
            umount "$mp" 2>/dev/null || umount -l "$mp" 2>/dev/null || true
        done
    }
    trap cleanup_mounts EXIT

    for mp in proc sys dev dev/pts; do
        mkdir -p "$ROOTFS/$mp"
    done
    if ! mountpoint -q "$ROOTFS/proc"; then
        mount --bind /proc "$ROOTFS/proc"
        MOUNTED_BY_US+=("$ROOTFS/proc")
    fi
    if ! mountpoint -q "$ROOTFS/sys"; then
        mount --bind /sys "$ROOTFS/sys"
        MOUNTED_BY_US+=("$ROOTFS/sys")
    fi
    if ! mountpoint -q "$ROOTFS/dev"; then
        mount --bind /dev "$ROOTFS/dev"
        MOUNTED_BY_US+=("$ROOTFS/dev")
    fi
    if ! mountpoint -q "$ROOTFS/dev/pts"; then
        mount --bind /dev/pts "$ROOTFS/dev/pts"
        MOUNTED_BY_US+=("$ROOTFS/dev/pts")
    fi
    # Detach our binds from the host's propagation peer groups so the lazy
    # `umount -l` in cleanup_mounts cannot propagate back and tear down the
    # host's shared /dev/pts (which would break host PTY allocation —
    # "sudo: unable to open pty" — until `wsl --shutdown`).
    for mp in proc sys dev; do
        mountpoint -q "$ROOTFS/$mp" && mount --make-rprivate "$ROOTFS/$mp" || true
    done
    if [ -e /etc/resolv.conf ]; then
        cp -L /etc/resolv.conf "$ROOTFS/etc/resolv.conf"
    fi

    if chroot "$ROOTFS" /usr/local/bin/cos engine list ort-genai \
        | grep -q "\"active\":\"$ORT_GENAI_TAG\""; then
        echo "  :: ort-genai $ORT_GENAI_TAG already active"
    else
        echo "  :: installing ort-genai $ORT_GENAI_TAG"
        chroot "$ROOTFS" /usr/local/bin/cos engine update ort-genai --to "$ORT_GENAI_TAG"
    fi

    cleanup_mounts
    trap - EXIT
fi
