#!/usr/bin/env bash
# Verify the same installed Helium runtime for desktop and headless rendering.
#
# Inherited from environment: ROOTFS, PROJECT_DIR.

set -euo pipefail

if ! chroot "$ROOTFS" dpkg-query -W -f='${db:Status-Status}' helium-bin | grep -Fxq installed ||
    [ ! -x "$ROOTFS/opt/helium/helium" ]; then
    echo "error: the signed Helium runtime is not installed" >&2
    exit 1
fi
echo "  :: Helium browser runtime ready"
