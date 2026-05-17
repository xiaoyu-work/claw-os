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
#   1. Builds the rootfs at $PROJECT_DIR/build/claw-os-rootfs (if missing).
#   2. Adds the default cos user and pre-creates /var/lib/cos/overlay/{base,
#      upper,work} owned by cos so cos-home-setup.service can prepare the
#      agent home at boot.
#   3. Invokes `docker build` on the Dockerfile.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
ROOTFS="$PROJECT_DIR/build/claw-os-rootfs"
FEATURES="${FEATURES:-base,cos-core,browser,systemd,apt-source,qwen3-embedding}"

source "$PROJECT_DIR/scripts/lib/add-cos-user.sh"

DOCKERFILE="$SCRIPT_DIR/Dockerfile"
TAG="${TAG:-claw-os}"

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

if [ ! -d "$ROOTFS" ]; then
    echo ":: rootfs missing — building (this takes a few minutes)"
    "$PROJECT_DIR/rootfs/build.sh" --features "$FEATURES"
else
    echo ":: using existing rootfs at $ROOTFS"
    echo "   (rebuild from scratch: sudo rm -rf $ROOTFS && sudo $0)"
fi

if [[ ",$FEATURES," == *,qwen3-embedding,* ]]; then
    echo ":: ensuring qwen3-embedding feature"
    ROOTFS="$ROOTFS" PROJECT_DIR="$PROJECT_DIR" \
        "$PROJECT_DIR/rootfs/features/qwen3-embedding/install.sh"
fi

echo ":: prepping rootfs — cos user + overlay scratch dirs"
add_cos_user "$ROOTFS"

# Pre-create the overlay scratch dirs and chown them to cos so setup-home.sh
# can prepare the non-root agent home at boot. The overlay mount itself still
# needs CAP_SYS_ADMIN and is skipped with a JSON warning when absent — that's
# the documented behaviour for unprivileged containers.
chroot "$ROOTFS" /bin/bash -c '
    set -e
    mkdir -p /var/lib/cos/overlay/base \
             /var/lib/cos/overlay/upper \
             /var/lib/cos/overlay/work
    chown -R cos:cos /var/lib/cos/overlay
'

cd "$PROJECT_DIR"
exec docker build -f "$DOCKERFILE" -t "$TAG" "$@" .
