#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$PROJECT_DIR/scripts/lib/arch.sh"
source "$PROJECT_DIR/scripts/lib/image-profiles.sh"

mode=${1:-}
if (( $# > 1 )) || [[ -n $mode && $mode != --installer-payload ]]; then
    echo "usage: ./build.sh asahi-rootfs [--installer-payload]" >&2
    exit 2
fi
if [[ $DEB_ARCH != arm64 || $(id -u) != 0 ]]; then
    echo "error: build the Asahi rootfs as root on a native arm64 Linux host" >&2
    exit 1
fi
if [[ $mode == --installer-payload ]]; then
    for tool in rsync mkfs.ext4 zip; do
        command -v "$tool" >/dev/null || { echo "error: --installer-payload requires $tool" >&2; exit 1; }
    done
fi

export CLAW_PLATFORM=aarch64-apple
"$PROJECT_DIR/rootfs/build.sh" --features "$IMAGE_FEATURES_ASAHI"
ROOTFS="$PROJECT_DIR/build/claw-os-rootfs"
test -s "$ROOTFS/usr/lib/cos/asahi/boot.bin"
test -s "$ROOTFS/usr/lib/cos/asahi/boot-j700.bin"

if [[ $mode == --installer-payload ]]; then
    identity=$(python3 "$PROJECT_DIR/scripts/platform_kernels.py" "$CLAW_PLATFORM" --describe)
    read -r _ release _ <<< "$identity"
    exec bash "$PROJECT_DIR/targets/asahi-rootfs/installer-payload.sh" "$ROOTFS" "$release" \
        "$PROJECT_DIR/build/claw-os-asahi-arm64-installer.zip"
fi

OUTPUT="$PROJECT_DIR/build/claw-os-asahi-arm64.tar.zst"
archive=$(mktemp "$OUTPUT.XXXXXX")
trap 'rm -f -- "$archive"' EXIT
tar --one-file-system --numeric-owner --xattrs --acls -I 'zstd -T0' \
    -cpf "$archive" -C "$ROOTFS" .
chmod 0644 "$archive"
mv -f -- "$archive" "$OUTPUT"
echo ":: wrote $OUTPUT"
