#!/usr/bin/env bash
# Claw OS — docker target.
#
# Builds a Docker image from the rootfs at build/claw-os-rootfs.  The rootfs
# must be built first via `sudo ./rootfs/build.sh`.
#
# Environment variables:
#   PROFILE   base|openclaw|deerflow|ironclaw   (default: base)
#   TAG       Docker image tag                  (default: claw-os[:profile])

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

PROFILE="${PROFILE:-base}"

case "$PROFILE" in
    base)
        DOCKERFILE="$SCRIPT_DIR/Dockerfile"
        DEFAULT_TAG="claw-os"
        ;;
    openclaw|deerflow|ironclaw)
        DOCKERFILE="$SCRIPT_DIR/Dockerfile.$PROFILE"
        DEFAULT_TAG="claw-os:$PROFILE"
        ;;
    *)
        echo "error: unknown PROFILE '$PROFILE' (expected: base|openclaw|deerflow|ironclaw)" >&2
        exit 1
        ;;
esac

TAG="${TAG:-$DEFAULT_TAG}"

if [ ! -f "$DOCKERFILE" ]; then
    echo "error: dockerfile not found: $DOCKERFILE" >&2
    exit 1
fi

# Profile variants (openclaw/deerflow/ironclaw) FROM ghcr base image, so they
# don't require the local rootfs.  Only the base profile copies it directly.
if [ "$PROFILE" = "base" ] && [ ! -d "$PROJECT_DIR/build/claw-os-rootfs" ]; then
    echo "error: rootfs not found at build/claw-os-rootfs" >&2
    echo "       run: sudo ./rootfs/build.sh" >&2
    exit 1
fi

cd "$PROJECT_DIR"
exec docker build -f "$DOCKERFILE" -t "$TAG" "$@" .
