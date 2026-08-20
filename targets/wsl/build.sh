#!/usr/bin/env bash
# Build the modern WSL package without a pre-created human account.
# Output:  build/claw-os-wsl-<arch>.wsl  (arch from $ARCH).
# Usage:   sudo ./build.sh wsl

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
ROOTFS="$PROJECT_DIR/build/claw-os-rootfs"

source "$PROJECT_DIR/scripts/lib/arch.sh"
source "$PROJECT_DIR/scripts/lib/image-identity.sh"
source "$PROJECT_DIR/scripts/lib/image-profiles.sh"

FEATURES="${FEATURES:-$IMAGE_FEATURES_HEADLESS_RUNTIME}"

OUTPUT="$PROJECT_DIR/build/claw-os-wsl-${ARCH_SUFFIX}.wsl"
WSL_ROOTFS="$PROJECT_DIR/build/claw-os-wsl-rootfs-${ARCH_SUFFIX}"
WSL_UPPER="$PROJECT_DIR/build/.claw-os-wsl-upper-${ARCH_SUFFIX}"
WSL_WORK="$PROJECT_DIR/build/.claw-os-wsl-work-${ARCH_SUFFIX}"

if [ "$(id -u)" -ne 0 ]; then
    echo "error: must run as root (debootstrap, chroot and package creation need it)" >&2
    exit 1
fi

if mountpoint -q "$WSL_ROOTFS" 2>/dev/null; then
    umount "$WSL_ROOTFS" 2>/dev/null || umount -l "$WSL_ROOTFS"
fi
rm -rf "$WSL_ROOTFS" "$WSL_UPPER" "$WSL_WORK"

"$PROJECT_DIR/rootfs/build.sh" --reuse-if-matching --features "$FEATURES"

echo ":: creating WSL staging rootfs at $WSL_ROOTFS"
mkdir -p "$WSL_ROOTFS" "$WSL_UPPER" "$WSL_WORK"
mount -t overlay overlay \
    -o "lowerdir=$ROOTFS,upperdir=$WSL_UPPER,workdir=$WSL_WORK" \
    "$WSL_ROOTFS"
cleanup_wsl_staging() {
    if mountpoint -q "$WSL_ROOTFS" 2>/dev/null; then
        umount "$WSL_ROOTFS" 2>/dev/null || umount -l "$WSL_ROOTFS" 2>/dev/null || true
    fi
    rm -rf "$WSL_ROOTFS" "$WSL_UPPER" "$WSL_WORK"
}
trap cleanup_wsl_staging EXIT

if [ -d "$SCRIPT_DIR/overlay" ]; then
    echo ":: applying WSL overlay"
    cp -a --no-preserve=ownership "$SCRIPT_DIR/overlay/." "$WSL_ROOTFS/"
fi
chmod 0755 "$WSL_ROOTFS/usr/lib/cos/init/wsl-oobe"
assert_no_human_login_users "$WSL_ROOTFS" "WSL package"

echo ":: packaging $OUTPUT"
mkdir -p "$(dirname "$OUTPUT")"
tar -C "$WSL_ROOTFS" \
    --exclude='./proc/*' \
    --exclude='./sys/*' \
    --exclude='./dev/*' \
    --exclude='./run/*' \
    --exclude='./tmp/*' \
    -czf "$OUTPUT" .

SIZE=$(du -h "$OUTPUT" | cut -f1)
echo ":: done — $OUTPUT ($SIZE)"
echo
echo "To install on Windows:"
echo "  wsl --install --from-file .\\$(basename "$OUTPUT") --name claw-os --location C:\\WSL\\claw-os --version 2"
echo "The first launch prompts for the UNIX username and password."
