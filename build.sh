#!/usr/bin/env bash
# Claw OS — top-level build dispatcher.
#
# Delegates to targets/<target>/build.sh.  Each target produces a single
# distribution artifact (Docker image, ISO, WSL tar, VM image, etc.) from
# the same shared rootfs in build/claw-os-rootfs.
#
# Usage:
#   ./build.sh <target> [args...]
#
# Environment variables are forwarded to the target's build.sh.  Run
# `./build.sh <target> --help` (when supported) for target-specific options.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

usage() {
    cat <<EOF
Usage: $0 <target> [args...]

Available targets:
EOF
    if [ -d "$SCRIPT_DIR/targets" ]; then
        for d in "$SCRIPT_DIR"/targets/*/; do
            name="$(basename "$d")"
            [ "$name" = "common" ] && continue
            [ -f "$d/build.sh" ] && echo "  $name"
        done
    fi
    cat <<EOF

Examples:
  ./build.sh docker                          # docker image
  sudo ./build.sh vm                         # qcow2 / vmdk / vhdx
  sudo ./build.sh azure                      # generalized fixed VHD
  sudo ./build.sh iso-live                   # live ISO
  sudo ./build.sh wsl                        # WSL tarball
EOF
}

if [ $# -lt 1 ] || [ "$1" = "-h" ] || [ "$1" = "--help" ]; then
    usage
    [ $# -lt 1 ] && exit 1 || exit 0
fi

TARGET="$1"; shift

if [ "$TARGET" = "common" ]; then
    echo "error: 'common' is a shared library, not a build target" >&2
    exit 1
fi

TARGET_DIR="$SCRIPT_DIR/targets/$TARGET"
TARGET_BUILD="$TARGET_DIR/build.sh"

if [ ! -d "$TARGET_DIR" ] || [ ! -f "$TARGET_BUILD" ]; then
    echo "error: unknown target '$TARGET'" >&2
    echo >&2
    usage >&2
    exit 1
fi

exec bash "$TARGET_BUILD" "$@"
