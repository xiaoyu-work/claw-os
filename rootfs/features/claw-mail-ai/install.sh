#!/usr/bin/env bash
# rootfs/features/claw-mail-ai/install.sh — wire the Claw Mail AI
# Thunderbird MailExtension and its Python Native Messaging host into
# the rootfs.
#
# Lay-down:
#   /usr/lib/cos/apps/mail-ai/               — python host + verb impls
#   /usr/lib/cos/claw-mail-ai-host           — trusted native launcher (from base package)
#   /etc/thunderbird/native-messaging-hosts/os.claw.mail_ai.json
#                                            — NM host manifest
#   /usr/lib/thunderbird/distribution/extensions/claw-mail-ai@claw.os.xpi
#                                            — packed extension, distribution-installed
#   /etc/thunderbird/policies/policies.json  — pin/lock + privacy defaults
#
# Inherited: $ROOTFS, $PROJECT_DIR, $SCRIPT_DIR (features/), $COS_VERSION.

set -euo pipefail

EXT_SRC="$PROJECT_DIR/extensions/claw-mail-ai"
APP_SRC="$PROJECT_DIR/apps/mail-ai"
SDK_PY_SRC="$PROJECT_DIR/claw-os-sdk/python/src/claw_os_sdk"
RUNTIME_PY_SRC="$PROJECT_DIR/cos-runtime/python/src/cos_runtime"
FEATURE_DIR="$SCRIPT_DIR/features/claw-mail-ai"

EXT_ID="claw-mail-ai@claw.os"

# ---------------------------------------------------------------------------
# 0. Sanity — make sure the source trees we expect exist.
# ---------------------------------------------------------------------------
for d in "$EXT_SRC" "$APP_SRC" "$SDK_PY_SRC" "$RUNTIME_PY_SRC"; do
    if [ ! -d "$d" ]; then
        echo "  error: required source dir missing: $d" >&2
        exit 1
    fi
done
if [ ! -f "$EXT_SRC/manifest.json" ]; then
    echo "  error: $EXT_SRC/manifest.json not found" >&2
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
# 2. Copy the Python host + verb impls + the Python SDK into
#    /usr/lib/cos/apps/mail-ai/ and /usr/lib/cos/python/. The host adds its
#    own dir and /usr/lib/cos/python to sys.path so `from claw_os_sdk
#    import ai` (used by main.py) resolves without any global
#    PYTHONPATH munging.
# ---------------------------------------------------------------------------
CANONICAL_APP_DEST="$ROOTFS/usr/lib/cos/apps/mail-ai"
echo "  :: installing Python host  → /usr/lib/cos/apps/mail-ai"
# Older images carried a second unverified execution copy here.
rm -rf "$ROOTFS/usr/lib/cos/mail-ai"
install -d -m 0755 "$CANONICAL_APP_DEST"
cp -a --no-preserve=ownership "$APP_SRC/." "$CANONICAL_APP_DEST/"
# Drop test files from the system copy — they're not needed at runtime.
rm -f "$CANONICAL_APP_DEST/test_main.py"
# claw_os_sdk lives in a system-wide location so every app on the
# device can import it. We drop it under /usr/lib/cos/python/ so
# `from claw_os_sdk import ai` resolves once sys.path contains that
# directory (native_host.py adds it on startup). cos_runtime sits in
# the same directory so internal apps can `from cos_runtime import
# policy` the same way.
SDK_DEST="$ROOTFS/usr/lib/cos/python/claw_os_sdk"
if [ ! -d "$SDK_DEST" ]; then
    echo "  :: installing claw-os-sdk → /usr/lib/cos/python/claw_os_sdk"
    install -d -m 0755 "$SDK_DEST"
    cp -a --no-preserve=ownership "$SDK_PY_SRC/." "$SDK_DEST/"
fi
RUNTIME_DEST="$ROOTFS/usr/lib/cos/python/cos_runtime"
if [ ! -d "$RUNTIME_DEST" ]; then
    echo "  :: installing cos-runtime → /usr/lib/cos/python/cos_runtime"
    install -d -m 0755 "$RUNTIME_DEST"
    cp -a --no-preserve=ownership "$RUNTIME_PY_SRC/." "$RUNTIME_DEST/"
fi
chown -R 0:0 "$CANONICAL_APP_DEST" "$SDK_DEST" "$RUNTIME_DEST"
find "$CANONICAL_APP_DEST" "$SDK_DEST" "$RUNTIME_DEST" -type d -exec chmod 0755 {} +
find "$CANONICAL_APP_DEST" "$SDK_DEST" "$RUNTIME_DEST" -type f -exec chmod 0644 {} +
chmod 0755 "$CANONICAL_APP_DEST/native_host.py"

# ---------------------------------------------------------------------------
# 3. Pack the WebExtension as an XPI and drop it into Thunderbird's
#    distribution/extensions directory, where Thunderbird auto-installs
#    it for every profile on first launch.
#
#    The XPI is just a zip of the extension's contents with the .xpi
#    suffix. The filename **must** match the gecko id from manifest.json.
# ---------------------------------------------------------------------------
XPI_NAME="${EXT_ID}.xpi"
XPI_DEST_DIR="$ROOTFS/usr/lib/thunderbird/distribution/extensions"
echo "  :: packaging extension     → $XPI_DEST_DIR/$XPI_NAME"
install -d -m 0755 "$XPI_DEST_DIR"

# Build the .xpi inside the chroot so we use the chroot's `zip` binary
# (host may not have it). Mount the source dir read-only into the chroot
# via a bind mount over /tmp/claw-mail-ai-src.
SRC_MOUNT="$ROOTFS/tmp/claw-mail-ai-src"
mkdir -p "$SRC_MOUNT"
mount --bind "$EXT_SRC" "$SRC_MOUNT"
trap 'umount "$SRC_MOUNT" 2>/dev/null || true; rmdir "$SRC_MOUNT" 2>/dev/null || true' EXIT

chroot "$ROOTFS" /bin/sh -ec "
    cd /tmp/claw-mail-ai-src
    rm -f /usr/lib/thunderbird/distribution/extensions/${XPI_NAME}
    # The manifest must be at the root of the zip — that's why we cd into
    # the extension dir first. -X strips uid/gid/atime, -r recurses.
    zip -X -r -q /usr/lib/thunderbird/distribution/extensions/${XPI_NAME} . \\
        -x '*.git*' '*.DS_Store' 'test_*.py' '*.swp' '*.bak'
"
chmod 0644 "$XPI_DEST_DIR/$XPI_NAME"

umount "$SRC_MOUNT" 2>/dev/null || true
rmdir "$SRC_MOUNT" 2>/dev/null || true
trap - EXIT

# ---------------------------------------------------------------------------
# 4. Drop a marker README so admins can find the extension source on disk.
# ---------------------------------------------------------------------------
README_DEST="$ROOTFS/usr/share/doc/claw-mail-ai"
install -d -m 0755 "$README_DEST"
cp "$EXT_SRC/README.md" "$README_DEST/README.md"

echo "  :: claw-mail-ai feature applied"
