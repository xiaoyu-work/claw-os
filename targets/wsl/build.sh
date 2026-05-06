#!/usr/bin/env bash
# targets/wsl/build.sh — Build a WSL2 importable rootfs tarball.
#
# Output:  build/claw-os-wsl-amd64.tar.gz
#
# Usage:   sudo ./build.sh wsl
#
# Steps:
#   1. Build a Debian rootfs with features: base, cos-core, browser, systemd
#      (browser is bundled but its systemd unit is NOT enabled — see plan §7).
#   2. Apply the WSL-specific overlay (wsl.conf).
#   3. Create a default 'cos' user (uid 1000, passwordless sudo).
#   4. Tar the rootfs into a tarball that `wsl --import` can consume.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
ROOTFS="$PROJECT_DIR/build/claw-os-rootfs"
OUTPUT="$PROJECT_DIR/build/claw-os-wsl-amd64.tar.gz"

if [ "$(id -u)" -ne 0 ]; then
    echo "error: must run as root (debootstrap, chroot and tarball creation need it)" >&2
    exit 1
fi

# 1. Build the rootfs with the WSL feature set.
"$PROJECT_DIR/rootfs/build.sh" --features base,cos-core,browser,systemd

# 2. Apply WSL-specific overlay (wsl.conf, etc.).
if [ -d "$SCRIPT_DIR/overlay" ]; then
    echo ":: applying WSL overlay"
    cp -a "$SCRIPT_DIR/overlay/." "$ROOTFS/"
fi

# 3. Create the default 'cos' user.
#    UID 1000 is conventional for the first non-system user; matches the
#    'default=cos' line in /etc/wsl.conf.
echo ":: creating default 'cos' user"
chroot "$ROOTFS" /bin/bash -c '
    set -e
    if ! id cos >/dev/null 2>&1; then
        useradd -m -u 1000 -s /bin/bash -G sudo cos
        # Passwordless sudo. WSL has no install-time password prompt, so
        # this is the standard convention. Users can tighten later via
        # `sudo passwd cos` and editing /etc/sudoers.d/cos.
        mkdir -p /etc/sudoers.d
        echo "cos ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/cos
        chmod 0440 /etc/sudoers.d/cos
    fi
'

# 4. Tar up the rootfs. /proc, /sys and /dev are populated by WSL at boot;
#    excluding them keeps the tarball smaller and avoids permission issues.
echo ":: packaging $OUTPUT"
mkdir -p "$(dirname "$OUTPUT")"
tar -C "$ROOTFS" \
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
echo "  wsl --import claw-os C:\\WSL\\claw-os $OUTPUT --version 2"
echo "  wsl -d claw-os"
