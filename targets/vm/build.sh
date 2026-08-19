#!/usr/bin/env bash
# Local virtual-machine image target.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

source "$PROJECT_DIR/scripts/lib/image-profiles.sh"

export FEATURES="${FEATURES:-$IMAGE_FEATURES_VM}"
export IMAGE_BASENAME="${IMAGE_BASENAME:-claw-os-vm}"
export IMAGE_PLATFORM=vm

exec bash "$PROJECT_DIR/targets/common/disk-image.sh"
