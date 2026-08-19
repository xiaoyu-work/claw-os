#!/usr/bin/env bash
# Build an Azure Compute Gallery-compatible generalized fixed VHD.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

source "$PROJECT_DIR/scripts/lib/image-profiles.sh"

IMAGE_FLAVOR="${IMAGE_FLAVOR:-headless}"
case "$IMAGE_FLAVOR" in
    headless)
        DEFAULT_FEATURES="$IMAGE_FEATURES_AZURE"
        DEFAULT_SIZE=16G
        ;;
    desktop)
        DEFAULT_FEATURES="$IMAGE_FEATURES_AZURE_DESKTOP"
        DEFAULT_SIZE=50G
        ;;
    *)
        echo "error: unsupported IMAGE_FLAVOR='$IMAGE_FLAVOR' (expected headless or desktop)" >&2
        exit 1
        ;;
esac

if [ -n "${FORMATS:-}" ] && [ "$FORMATS" != "vhd" ]; then
    echo "error: the Azure target only produces a fixed VHD; do not override FORMATS='$FORMATS'" >&2
    exit 1
fi

export FEATURES="${FEATURES:-$DEFAULT_FEATURES}"
export FORMATS=vhd
export SIZE="${SIZE:-$DEFAULT_SIZE}"
export IMAGE_BASENAME="${IMAGE_BASENAME:-claw-os-azure}"
export IMAGE_PLATFORM=azure
export IMAGE_ROOTFS_FINALIZER="$SCRIPT_DIR/finalize-rootfs.sh"

echo ":: Azure image profile: $IMAGE_FLAVOR"
echo ":: generalized image: users and SSH keys are provisioned by Azure cloud-init"

exec bash "$PROJECT_DIR/targets/common/disk-image.sh"
