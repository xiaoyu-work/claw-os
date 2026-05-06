#!/usr/bin/env bash
# rootfs/features/cos-core/install.sh — install the cos binary plus apps,
# plugins, and skills.
#
# Inherited from environment: ROOTFS, PROJECT_DIR.
#
# Note: cos must be pre-built before invoking this feature. CI builds it
# with `cargo build --release -p cos --target x86_64-unknown-linux-musl`.

set -euo pipefail

COS_BIN=""
for candidate in \
    "$PROJECT_DIR/target/x86_64-unknown-linux-musl/release/cos" \
    "$PROJECT_DIR/core/target/x86_64-unknown-linux-musl/release/cos" \
    "$PROJECT_DIR/target/release/cos" \
    "$PROJECT_DIR/core/target/release/cos" \
    "$PROJECT_DIR/target/x86_64-unknown-linux-gnu/release/cos" \
    "$PROJECT_DIR/core/target/x86_64-unknown-linux-gnu/release/cos"; do
    if [ -f "$candidate" ]; then
        COS_BIN="$candidate"
        break
    fi
done

if [ -z "$COS_BIN" ]; then
    echo "  error: cos binary not found. Build it first:" >&2
    echo "    cargo build --release -p cos" >&2
    exit 1
fi

echo "  :: installing cos binary from $COS_BIN"
install -m 755 "$COS_BIN" "$ROOTFS/usr/local/bin/cos"

echo "  :: installing apps"
mkdir -p "$ROOTFS/usr/lib/cos/apps"
cp -a "$PROJECT_DIR/apps/." "$ROOTFS/usr/lib/cos/apps/"

echo "  :: installing plugins and skills"
mkdir -p "$ROOTFS/usr/lib/cos/plugins"
mkdir -p "$ROOTFS/usr/lib/cos/skills"
if [ -d "$PROJECT_DIR/plugins" ]; then
    cp -a "$PROJECT_DIR/plugins/." "$ROOTFS/usr/lib/cos/plugins/"
fi
if [ -d "$PROJECT_DIR/skills" ]; then
    cp -a "$PROJECT_DIR/skills/." "$ROOTFS/usr/lib/cos/skills/"
fi
