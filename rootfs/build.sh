#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
ROOTFS="$PROJECT_DIR/build/claw-os-rootfs"
SUITE="bookworm"

if [ "$(id -u)" -ne 0 ]; then
    echo "error: must run as root" >&2
    exit 1
fi

# 0. Build Rust cos binary (cross-compile for Linux x86_64)
echo ":: building cos binary"
cd "$PROJECT_DIR/core"
cargo build --release --target x86_64-unknown-linux-gnu
COS_BIN="$PROJECT_DIR/core/target/x86_64-unknown-linux-gnu/release/cos"
if [ ! -f "$COS_BIN" ]; then
    echo "error: cos binary not found at $COS_BIN" >&2
    echo "hint: install cross-compilation target: rustup target add x86_64-unknown-linux-gnu" >&2
    exit 1
fi
cd "$PROJECT_DIR"

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

# 3. Apply overlay (config files, cos-init, etc.)
echo ":: applying overlay"
cp -a "$SCRIPT_DIR/overlay/." "$ROOTFS/"

# 4. Install Rust cos binary
echo ":: installing cos binary"
install -m 755 "$COS_BIN" "$ROOTFS/usr/local/bin/cos"

# 5. Install apps
echo ":: installing apps"
mkdir -p "$ROOTFS/usr/lib/cos/apps"
cp -a "$PROJECT_DIR/apps/." "$ROOTFS/usr/lib/cos/apps/"

# 6. Install Jina Reader (browser engine)
echo ":: installing Jina Reader"
chroot "$ROOTFS" bash -c '
    cd /opt && git clone --depth 1 https://github.com/jina-ai/reader.git jina-reader
    cd /opt/jina-reader
    export PUPPETEER_CACHE_DIR=/opt/jina-reader/.cache
    npm install --production 2>&1 | tail -5
    npm cache clean --force
'

# 7. Create runtime directories
mkdir -p "$ROOTFS/workspace"
mkdir -p "$ROOTFS/var/lib/cos"

# 8. Source COS profile on login
if ! grep -q 'cos/profile.sh' "$ROOTFS/etc/bash.bashrc" 2>/dev/null; then
    echo '[ -f /etc/cos/profile.sh ] && . /etc/cos/profile.sh' >> "$ROOTFS/etc/bash.bashrc"
fi

echo ":: done — rootfs at $ROOTFS"
