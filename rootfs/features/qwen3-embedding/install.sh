#!/usr/bin/env bash
# Bundle the validated Qwen3 embedding stack into the rootfs.

set -euo pipefail

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
    if [ -e /etc/resolv.conf ]; then
        cp -L /etc/resolv.conf "$ROOTFS/etc/resolv.conf"
    fi

    ORT_GENAI_VERSION="$(
        sed -n 's/^pub const ORT_GENAI_KNOWN_GOOD_VERSION: &str = "\(.*\)";/\1/p' \
            "$PROJECT_DIR/core/src/engine_pkg/mod.rs" | head -1
    )"
    ORT_GENAI_TAG="v${ORT_GENAI_VERSION:-0.12.2}"

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
