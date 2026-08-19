#!/usr/bin/env bash
# Claw OS — docker target.
#
# Produces the headless Claw OS Docker image: the full non-desktop OS runtime
# (`base,cos-core,browser,systemd,apt-source,qwen3-embedding`) with Claw's own
# cos/clawd agent, apps, skills, browser automation, upgrade source, and the
# local embedding stack on architectures where upstream ships a Linux
# ort-genai runtime. It intentionally does not include desktop UI,
# installer/boot/VM-only features, or third-party agent providers such as
# copilot-cli.
#
# systemd is PID 1, exactly like the WSL/VM targets, so clawd starts through
# the same enabled clawd.service boot path everywhere. The default login user
# remains `cos` (uid 1000, NOPASSWD sudo) matching WSL.
#
# Environment variables:
#   TAG        Docker image tag (default: claw-os)
#   FEATURES   Rootfs feature set (default: headless Claw OS runtime)
#
# This script:
#   1. Builds or strictly reuses the stamped immutable base rootfs.
#   2. Copies it to a Docker-only staging tree, then adds the default cos
#      user and overlay scratch directories there.
#   3. Invokes `docker build` on the Dockerfile.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
ROOTFS="$PROJECT_DIR/build/claw-os-rootfs"

source "$PROJECT_DIR/scripts/lib/arch.sh"
source "$PROJECT_DIR/scripts/lib/add-cos-user.sh"
source "$PROJECT_DIR/scripts/lib/image-profiles.sh"

FEATURES="${FEATURES:-$IMAGE_FEATURES_HEADLESS_RUNTIME}"

DOCKERFILE="$SCRIPT_DIR/Dockerfile"
TAG="${TAG:-claw-os}"
DOCKER_ROOTFS="$PROJECT_DIR/build/claw-os-docker-rootfs-${ARCH_SUFFIX}"
DOCKER_UPPER="$PROJECT_DIR/build/.claw-os-docker-upper-${ARCH_SUFFIX}"
DOCKER_WORK="$PROJECT_DIR/build/.claw-os-docker-work-${ARCH_SUFFIX}"

if [ ! -f "$DOCKERFILE" ]; then
    echo "error: dockerfile not found: $DOCKERFILE" >&2
    exit 1
fi

# Ensure the rootfs exists with the right features, then apply Docker-specific
# prep idempotently.
if [ "$(id -u)" -ne 0 ]; then
    echo "error: must run as root (debootstrap + chroot need it)" >&2
    echo "       run: sudo ./targets/docker/build.sh" >&2
    exit 1
fi

if mountpoint -q "$DOCKER_ROOTFS" 2>/dev/null; then
    umount "$DOCKER_ROOTFS" 2>/dev/null || umount -l "$DOCKER_ROOTFS"
fi
rm -rf "$DOCKER_ROOTFS" "$DOCKER_UPPER" "$DOCKER_WORK"

"$PROJECT_DIR/rootfs/build.sh" --reuse-if-matching --features "$FEATURES"

echo ":: creating Docker staging rootfs at $DOCKER_ROOTFS"
mkdir -p "$DOCKER_ROOTFS" "$DOCKER_UPPER" "$DOCKER_WORK"
mount -t overlay overlay \
    -o "lowerdir=$ROOTFS,upperdir=$DOCKER_UPPER,workdir=$DOCKER_WORK" \
    "$DOCKER_ROOTFS"
cleanup_docker_staging() {
    if mountpoint -q "$DOCKER_ROOTFS" 2>/dev/null; then
        umount "$DOCKER_ROOTFS" 2>/dev/null || umount -l "$DOCKER_ROOTFS" 2>/dev/null || true
    fi
    rm -rf "$DOCKER_ROOTFS" "$DOCKER_UPPER" "$DOCKER_WORK"
}
trap cleanup_docker_staging EXIT

echo ":: prepping rootfs — cos user + overlay scratch dirs"
add_cos_user "$DOCKER_ROOTFS"

# Pre-create the overlay scratch dirs and chown them to cos so setup-home.sh
# can prepare the non-root agent home at boot. The overlay mount itself still
# needs CAP_SYS_ADMIN and is skipped with a JSON warning when absent — that's
# the documented behaviour for unprivileged containers.
chroot "$DOCKER_ROOTFS" /bin/bash -c '
    set -e
    mkdir -p /var/lib/cos/overlay/base \
             /var/lib/cos/overlay/upper \
             /var/lib/cos/overlay/work
    chown -R cos:cos /var/lib/cos/overlay
'

cd "$PROJECT_DIR"
docker build \
    --build-arg "ROOTFS_DIR=build/$(basename "$DOCKER_ROOTFS")" \
    -f "$DOCKERFILE" -t "$TAG" "$@" .
