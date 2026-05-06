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
#   - Always exits 0 — overlay-mount failure is non-fatal; cos still works
#     without checkpoint support, with a JSON warning emitted to stdout.

set -e

OVERLAY_DIR="/var/lib/cos/overlay"

if mountpoint -q /den 2>/dev/null; then
    echo '{"overlay": "already-mounted", "path": "/den"}'
    exit 0
fi

if [ "$(uname)" != "Linux" ] || ! command -v mount >/dev/null 2>&1; then
    exit 0
fi

mkdir -p "$OVERLAY_DIR/base" "$OVERLAY_DIR/upper" "$OVERLAY_DIR/work"

# First boot: seed the base layer from whatever the image shipped at /den.
if [ -z "$(ls -A "$OVERLAY_DIR/base" 2>/dev/null)" ] && [ -d /den ]; then
    cp -a /den/. "$OVERLAY_DIR/base/" 2>/dev/null || true
fi

if mount -t overlay overlay \
    -o "lowerdir=$OVERLAY_DIR/base,upperdir=$OVERLAY_DIR/upper,workdir=$OVERLAY_DIR/work" \
    /den 2>/tmp/cos-overlay-error; then
    echo '{"overlay": "mounted", "path": "/den"}'
else
    echo '{"overlay": "failed", "path": "/den", "error": "'"$(cat /tmp/cos-overlay-error)"'", "warning": "checkpoints disabled — run with --privileged or --cap-add SYS_ADMIN"}'
fi
