#!/usr/bin/env bash
# rootfs/features/browser/install.sh -- verify the browser runtime shipped by
# claw-os-agent. Feature packages still provide target-specific dependencies.
#
# Inherited from environment: ROOTFS, PROJECT_DIR.

set -euo pipefail

for binary in cos-browser cos-browser-worker; do
    if [ ! -x "$ROOTFS/usr/local/bin/$binary" ]; then
        echo "error: claw-os-agent browser binary missing: $binary" >&2
        exit 1
    fi
done
if ! chroot "$ROOTFS" sh -c \
    'command -v chromium >/dev/null 2>&1 || command -v chromium-browser >/dev/null 2>&1'; then
    echo "error: no supported Chromium executable installed" >&2
    exit 1
fi
echo "  :: claw-os-agent browser runtime ready"
