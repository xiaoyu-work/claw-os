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

# 0. Locate pre-built cos binary (built by CI or manually before running this script)
COS_BIN="$PROJECT_DIR/core/target/release/cos"
if [ ! -f "$COS_BIN" ]; then
    # Try cross-compilation target path
    COS_BIN="$PROJECT_DIR/core/target/x86_64-unknown-linux-gnu/release/cos"
fi
if [ ! -f "$COS_BIN" ]; then
    echo "error: cos binary not found. Build it first: cd core && cargo build --release" >&2
    exit 1
fi

# 1. Bootstrap minimal Debian rootfs
echo ":: debootstrap $SUITE -> $ROOTFS"
mkdir -p "$ROOTFS"
debootstrap --extractor=ar "$SUITE" "$ROOTFS"

# 2. Install Node.js 24 (OpenClaw requirement)
NODE_MAJOR=24
echo ":: installing Node.js $NODE_MAJOR"
chroot "$ROOTFS" bash -c "
    apt-get update -qq
    apt-get install -y --no-install-recommends ca-certificates curl gnupg
    mkdir -p /etc/apt/keyrings
    curl -fsSL https://deb.nodesource.com/gpgkey/nodesource-repo.gpg.key | gpg --dearmor -o /etc/apt/keyrings/nodesource.gpg
    echo \"deb [signed-by=/etc/apt/keyrings/nodesource.gpg] https://deb.nodesource.com/node_${NODE_MAJOR}.x nodistro main\" > /etc/apt/sources.list.d/nodesource.list
    apt-get update -qq
    apt-get install -y --no-install-recommends nodejs
    corepack enable
    corepack prepare pnpm@latest --activate
    apt-get clean
    rm -rf /var/lib/apt/lists/*
"

# 3. Install packages from packages.txt
PACKAGES=$(grep -v '^\s*#' "$SCRIPT_DIR/packages.txt" | grep -v '^\s*$' | tr '\n' ' ')
echo ":: installing packages: $PACKAGES"
chroot "$ROOTFS" apt-get update -qq
chroot "$ROOTFS" apt-get install -y --no-install-recommends $PACKAGES
chroot "$ROOTFS" apt-get clean
rm -rf "$ROOTFS/var/lib/apt/lists"/*

# 4. Apply overlay (config files, cos-init, etc.)
echo ":: applying overlay"
cp -a "$SCRIPT_DIR/overlay/." "$ROOTFS/"

# 5. Install Rust cos binary
echo ":: installing cos binary"
install -m 755 "$COS_BIN" "$ROOTFS/usr/local/bin/cos"

# 6. Install apps
echo ":: installing apps"
mkdir -p "$ROOTFS/usr/lib/cos/apps"
cp -a "$PROJECT_DIR/apps/." "$ROOTFS/usr/lib/cos/apps/"

# 7. Install Jina Reader (browser engine)
echo ":: installing Jina Reader"
chroot "$ROOTFS" bash -c '
    cd /opt && git clone --depth 1 https://github.com/jina-ai/reader.git jina-reader
    cd /opt/jina-reader
    export PUPPETEER_CACHE_DIR=/opt/jina-reader/.cache
    npm install --production 2>&1 | tail -5
    npm cache clean --force
'

# 8. Create runtime directories
mkdir -p "$ROOTFS/workspace"
mkdir -p "$ROOTFS/var/lib/cos"

# 9. Source COS profile on login
if ! grep -q 'cos/profile.sh' "$ROOTFS/etc/bash.bashrc" 2>/dev/null; then
    echo '[ -f /etc/cos/profile.sh ] && . /etc/cos/profile.sh' >> "$ROOTFS/etc/bash.bashrc"
fi

echo ":: done — rootfs at $ROOTFS"
