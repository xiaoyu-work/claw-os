#!/usr/bin/env bash
# rootfs/build.sh — Build a Debian rootfs by composing features.
#
# 1. Bootstraps a minimal Debian rootfs into build/claw-os-rootfs (always).
# 2. Copies rootfs/overlay/* on top (always).
# 3. Applies each feature in order: apt-installs its packages.txt, then runs
#    its install.sh.
#
# See rootfs/features/README.md for the feature contract.
#
# Usage:
#   sudo ./rootfs/build.sh [--features f1,f2,f3]
#
# Default features: base,cos-core,browser  (matches the legacy behaviour).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
ROOTFS="$PROJECT_DIR/build/claw-os-rootfs"
SUITE="trixie"

DEFAULT_FEATURES="base,cos-core,browser"
FEATURES="$DEFAULT_FEATURES"

usage() {
    cat <<EOF
Usage: $0 [--features <list>]

Build a Debian rootfs at $ROOTFS by composing features.

Options:
  --features <list>   Comma-separated feature names (default: $DEFAULT_FEATURES)
  -h, --help          Show this help

Available features:
EOF
    if [ -d "$SCRIPT_DIR/features" ]; then
        for d in "$SCRIPT_DIR"/features/*/; do
            [ -d "$d" ] || continue
            echo "  $(basename "$d")"
        done
    fi
}

while [ $# -gt 0 ]; do
    case "$1" in
        --features)
            FEATURES="$2"
            shift 2
            ;;
        --features=*)
            FEATURES="${1#--features=}"
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "error: unknown argument '$1'" >&2
            usage >&2
            exit 1
            ;;
    esac
done

# Parse features and validate before doing anything else (works without root).
IFS=',' read -ra FEATURE_LIST <<< "$FEATURES"
for f in "${FEATURE_LIST[@]}"; do
    if [ -z "$f" ]; then
        echo "error: empty feature name in '$FEATURES'" >&2
        exit 1
    fi
    if [ ! -d "$SCRIPT_DIR/features/$f" ]; then
        echo "error: unknown feature '$f' (no $SCRIPT_DIR/features/$f directory)" >&2
        exit 1
    fi
done

if [ "$(id -u)" -ne 0 ]; then
    echo "error: must run as root" >&2
    exit 1
fi

# Read version from Cargo.toml (single source of truth).
COS_VERSION=$(grep '^version' "$PROJECT_DIR/core/Cargo.toml" | head -1 | sed 's/.*"\(.*\)".*/\1/')

export ROOTFS PROJECT_DIR SCRIPT_DIR SUITE COS_VERSION

echo ":: features: ${FEATURE_LIST[*]}"

# 1. Bootstrap minimal Debian rootfs.
echo ":: debootstrap $SUITE -> $ROOTFS"
mkdir -p "$ROOTFS"
debootstrap --extractor=ar "$SUITE" "$ROOTFS"

# 2. Apply global overlay (config files, cos-init, etc.).
echo ":: applying global overlay"
cp -a "$SCRIPT_DIR/overlay/." "$ROOTFS/"

# 2b. Bind-mount kernel pseudofs and propagate resolv.conf into the chroot.
# Needed by chroot operations more involved than a plain `apt-get install`:
# - systemctl enable (wants /proc/1/comm to detect systemd)
# - plymouth-set-default-theme (reads /proc/cmdline)
# - rustup/cargo (read /proc/self/exe, /proc/cpuinfo; spawn child procs)
# - any package's postinst that runs `update-initramfs`, `ldconfig`, etc.
# debootstrap leaves /etc/resolv.conf empty; copy the host's so apt + curl
# can resolve names inside the chroot.
echo ":: setting up chroot bind mounts"
mkdir -p "$ROOTFS/proc" "$ROOTFS/sys" "$ROOTFS/dev" "$ROOTFS/dev/pts" "$ROOTFS/run"
mount --bind /proc "$ROOTFS/proc"
mount --bind /sys "$ROOTFS/sys"
mount --bind /dev "$ROOTFS/dev"
mount --bind /dev/pts "$ROOTFS/dev/pts"
if [ -e /etc/resolv.conf ]; then
    cp -L /etc/resolv.conf "$ROOTFS/etc/resolv.conf"
fi

cleanup_chroot_mounts() {
    # Unmount in reverse order, lazy fallback for stray references.
    for mp in "$ROOTFS/dev/pts" "$ROOTFS/dev" "$ROOTFS/sys" "$ROOTFS/proc"; do
        if mountpoint -q "$mp"; then
            umount "$mp" 2>/dev/null || umount -l "$mp" 2>/dev/null || true
        fi
    done
}
trap cleanup_chroot_mounts EXIT

# 3. Apply each feature in order.
for f in "${FEATURE_LIST[@]}"; do
    feature_dir="$SCRIPT_DIR/features/$f"
    echo "===> feature: $f"

    # 3a. Install packages.txt entries via apt inside chroot.
    if [ -f "$feature_dir/packages.txt" ]; then
        # Strip comments and blank lines. Guard with `|| true` so a
        # packages.txt that is entirely comments (e.g. apt-source) does
        # not return exit 1 from grep and trip `set -o pipefail`.
        pkgs=$( { grep -vE '^\s*(#|$)' "$feature_dir/packages.txt" || true; } | tr '\n' ' ')
        if [ -n "$pkgs" ]; then
            echo "  :: apt install $pkgs"
            chroot "$ROOTFS" apt-get update -qq
            chroot "$ROOTFS" apt-get install -y --no-install-recommends $pkgs
            chroot "$ROOTFS" apt-get clean
            rm -rf "$ROOTFS/var/lib/apt/lists"/*
        fi
    fi

    # 3b. Run install.sh on the host.
    if [ -f "$feature_dir/install.sh" ]; then
        echo "  :: running install.sh"
        if [ -x "$feature_dir/install.sh" ]; then
            "$feature_dir/install.sh"
        else
            bash "$feature_dir/install.sh"
        fi
    fi
done

echo ":: done — rootfs at $ROOTFS"
