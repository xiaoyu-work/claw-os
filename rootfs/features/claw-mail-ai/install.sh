#!/usr/bin/env bash
# rootfs/features/claw-mail-ai/install.sh — wire the Claw Mail AI
# Thunderbird MailExtension and its Python Native Messaging host into
# the rootfs.
#
# Lay-down:
#   /usr/lib/cos/apps/mail-ai/               — shared Python package (from agent package)
#   /usr/lib/cos/claw-mail-ai-host           — trusted native launcher (from agent package)
#   /etc/thunderbird/native-messaging-hosts/os.claw.mail_ai.json
#                                            — NM host manifest
#   /usr/lib/thunderbird/distribution/extensions/claw-mail-ai@claw.os.xpi
#                                            — package-owned UI protocol adapter
#   /etc/thunderbird/policies/policies.json  — pin/lock + privacy defaults
#
# Inherited: $ROOTFS, $PROJECT_DIR, $SCRIPT_DIR (features/), $COS_VERSION.

set -euo pipefail

EXT_SRC="$PROJECT_DIR/extensions/claw-mail-ai"
FEATURE_DIR="$SCRIPT_DIR/features/claw-mail-ai"
APP_DEST="$ROOTFS/usr/lib/cos/apps/mail-ai"

EXT_ID="claw-mail-ai@claw.os"
XPI_DEST="$ROOTFS/usr/lib/thunderbird/distribution/extensions/${EXT_ID}.xpi"

# ---------------------------------------------------------------------------
# 0. Sanity — make sure the source trees we expect exist.
# ---------------------------------------------------------------------------
for d in "$EXT_SRC" "$APP_DEST" \
    "$ROOTFS/usr/lib/cos/python/claw_os_sdk" \
    "$ROOTFS/usr/lib/cos/python/cos_runtime"; do
    if [ ! -d "$d" ]; then
        echo "  error: required Mail source/package directory missing: $d" >&2
        exit 1
    fi
done
if [ ! -f "$EXT_SRC/manifest.json" ]; then
    echo "  error: $EXT_SRC/manifest.json not found" >&2
    exit 1
fi
for file in app.json main.py server.py native_host.py; do
    if [ ! -f "$APP_DEST/$file" ]; then
        echo "  error: claw-os-agent Mail package is incomplete: $APP_DEST/$file" >&2
        exit 1
    fi
done
if [ ! -f "$XPI_DEST" ]; then
    echo "  error: claw-os-agent Mail extension is missing: $XPI_DEST" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# 1. Apply static overlay (NM manifest and policies).
# ---------------------------------------------------------------------------
if [ -d "$FEATURE_DIR/overlay" ] && [ -n "$(ls -A "$FEATURE_DIR/overlay" 2>/dev/null)" ]; then
    echo "  :: applying claw-mail-ai overlay"
    cp -a --no-preserve=ownership "$FEATURE_DIR/overlay/." "$ROOTFS/"
fi
if [ ! -x "$ROOTFS/usr/lib/cos/claw-mail-ai-host" ]; then
    echo "  error: trusted claw-mail-ai-host binary is missing from claw-os-agent" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# 2. Use the canonical App and SDK already installed by claw-os-agent.
#    Do not overwrite authenticated package contents or maintain a separate
#    Python copy for the UI that can drift from the Agent's implementation.
# ---------------------------------------------------------------------------
echo "  :: using package-owned Mail host and extension"

# ---------------------------------------------------------------------------
# 3. Drop the extension documentation.
# ---------------------------------------------------------------------------
README_DEST="$ROOTFS/usr/share/doc/claw-mail-ai"
install -d -m 0755 "$README_DEST"
cp "$EXT_SRC/README.md" "$README_DEST/README.md"

echo "  :: claw-mail-ai feature applied"
