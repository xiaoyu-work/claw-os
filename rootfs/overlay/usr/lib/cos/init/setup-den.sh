#!/bin/bash
# /usr/lib/cos/init/setup-den.sh — Set up OverlayFS on /den.
#
# Used by:
#   - /usr/local/bin/cos-init                 (Docker PID 1 entrypoint)
#   - cos-den-setup.service (systemd unit)    (WSL / ISO / VM targets)
#
# Behaviour:
#   - Idempotent (no-op if /den is already a mount).
#   - On non-Linux or where mount(8) is missing, exits 0 without doing anything.
#   - When the backing filesystem is already an overlay (live ISOs ship a
#     squashfs+overlay rootfs), upper/work move to tmpfs under /run, since
#     overlay-on-overlay is not reliably supported and would otherwise fail.
#   - Always exits 0 — overlay-mount failure is non-fatal; cos still works
#     without checkpoint support, with a JSON warning emitted to stdout.

set -e

OVERLAY_DIR="/var/lib/cos/overlay"
BASE="$OVERLAY_DIR/base"
UPPER="$OVERLAY_DIR/upper"
WORK="$OVERLAY_DIR/work"

if mountpoint -q /den 2>/dev/null; then
    echo '{"overlay": "already-mounted", "path": "/den"}'
    exit 0
fi

if [ "$(uname)" != "Linux" ] || ! command -v mount >/dev/null 2>&1; then
    exit 0
fi

# Detect overlay-backed rootfs (Debian live media). On those, upper/work
# must live on a non-overlay filesystem — use tmpfs at /run/cos-overlay.
mkdir -p "$OVERLAY_DIR"
backing_fs=$(findmnt -no FSTYPE -T "$OVERLAY_DIR" 2>/dev/null || echo unknown)
if [ "$backing_fs" = "overlay" ] || [ "$backing_fs" = "overlayfs" ]; then
    UPPER="/run/cos-overlay/upper"
    WORK="/run/cos-overlay/work"
fi

mkdir -p "$BASE" "$UPPER" "$WORK"

# First boot: seed the base layer from whatever the image shipped at /den.
if [ -z "$(ls -A "$BASE" 2>/dev/null)" ] && [ -d /den ]; then
    cp -a /den/. "$BASE/" 2>/dev/null || true
fi

if mount -t overlay overlay \
    -o "lowerdir=$BASE,upperdir=$UPPER,workdir=$WORK" \
    /den 2>/tmp/cos-overlay-error; then
    echo '{"overlay": "mounted", "path": "/den", "upper": "'"$UPPER"'"}'
else
    echo '{"overlay": "failed", "path": "/den", "error": "'"$(cat /tmp/cos-overlay-error)"'", "warning": "checkpoints disabled — run with --privileged or --cap-add SYS_ADMIN"}'
fi

