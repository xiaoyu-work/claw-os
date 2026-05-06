#!/usr/bin/env bash
# rootfs/features/browser/install.sh — install the cos-browser binary
# (vendored Obscura, Rust). Chromium and its runtime libs are installed
# from this feature's packages.txt before this script runs.
#
# Inherited from environment: ROOTFS, PROJECT_DIR.
#
# Note: cos-browser must be pre-built before invoking this feature. CI
# builds it with `cargo build --release -p cos-browser --target
# x86_64-unknown-linux-gnu` (V8 needs glibc, so we don't use musl here).

set -euo pipefail

COS_BROWSER_BIN=""
for candidate in \
    "$PROJECT_DIR/target/x86_64-unknown-linux-gnu/release/cos-browser" \
    "$PROJECT_DIR/target/release/cos-browser"; do
    if [ -f "$candidate" ]; then
        COS_BROWSER_BIN="$candidate"
        break
    fi
done

if [ -z "$COS_BROWSER_BIN" ]; then
    echo "  error: cos-browser binary not found. Build it first:" >&2
    echo "    cargo build --release -p cos-browser" >&2
    exit 1
fi

echo "  :: installing cos-browser from $COS_BROWSER_BIN"
install -m 755 "$COS_BROWSER_BIN" "$ROOTFS/usr/local/bin/cos-browser"

# cos-browser-worker is an optional helper produced alongside cos-browser.
COS_BROWSER_WORKER="$(dirname "$COS_BROWSER_BIN")/cos-browser-worker"
if [ -f "$COS_BROWSER_WORKER" ]; then
    install -m 755 "$COS_BROWSER_WORKER" "$ROOTFS/usr/local/bin/cos-browser-worker"
fi
