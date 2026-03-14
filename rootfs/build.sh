#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
ROOTFS="$PROJECT_DIR/build/agent-os-rootfs"
SUITE="bookworm"

if [ "$(id -u)" -ne 0 ]; then
    echo "error: must run as root" >&2
    exit 1
fi

# 1. Bootstrap minimal Debian rootfs
echo ":: debootstrap $SUITE -> $ROOTFS"
mkdir -p "$ROOTFS"
debootstrap --extractor=ar "$SUITE" "$ROOTFS"

# 2. Install packages from packages.txt
PACKAGES=$(grep -v '^\s*#' "$SCRIPT_DIR/packages.txt" | grep -v '^\s*$' | tr '\n' ' ')
echo ":: installing packages: $PACKAGES"
chroot "$ROOTFS" apt-get update -qq
chroot "$ROOTFS" apt-get install -y --no-install-recommends $PACKAGES
chroot "$ROOTFS" apt-get clean
rm -rf "$ROOTFS/var/lib/apt/lists"/*

# 3. Apply overlay
echo ":: applying overlay"
cp -a "$SCRIPT_DIR/overlay/." "$ROOTFS/"

# 4. Install apps
echo ":: installing apps"
mkdir -p "$ROOTFS/usr/lib/aos/apps"
cp -a "$PROJECT_DIR/apps/." "$ROOTFS/usr/lib/aos/apps/"

# 5. Create runtime directories
mkdir -p "$ROOTFS/workspace"
mkdir -p "$ROOTFS/var/lib/aos"

# 6. Source AOS profile on login
if ! grep -q 'aos/profile.sh' "$ROOTFS/etc/bash.bashrc" 2>/dev/null; then
    echo '[ -f /etc/aos/profile.sh ] && . /etc/aos/profile.sh' >> "$ROOTFS/etc/bash.bashrc"
fi

echo ":: done — rootfs at $ROOTFS"
