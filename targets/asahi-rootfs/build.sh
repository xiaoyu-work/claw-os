#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$PROJECT_DIR/scripts/lib/arch.sh"
source "$PROJECT_DIR/scripts/lib/image-profiles.sh"

if [[ $DEB_ARCH != arm64 || $(id -u) != 0 ]]; then
    echo "error: build the Asahi rootfs as root on a native arm64 Linux host" >&2
    exit 1
fi

export CLAW_PLATFORM=aarch64-apple
"$PROJECT_DIR/rootfs/build.sh" --features "$IMAGE_FEATURES_ASAHI"
ROOTFS="$PROJECT_DIR/build/claw-os-rootfs"
test -s "$ROOTFS/usr/lib/cos/asahi/boot.bin"
test -s "$ROOTFS/usr/lib/cos/asahi/boot-j700.bin"

OUTPUT="$PROJECT_DIR/build/claw-os-asahi-arm64.tar.zst"
archive=$(mktemp "$OUTPUT.XXXXXX")
trap 'rm -f -- "$archive"' EXIT
tar --one-file-system --numeric-owner --xattrs --acls -I 'zstd -T0' \
    -cpf "$archive" -C "$ROOTFS" .
chmod 0644 "$archive"
mv -f -- "$archive" "$OUTPUT"
echo ":: wrote $OUTPUT"
