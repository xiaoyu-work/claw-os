#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
ROOTFS="$PROJECT_DIR/build/claw-os-rootfs"
SUITE="bookworm"

# Read version from Cargo.toml (single source of truth)
COS_VERSION=$(grep '^version' "$PROJECT_DIR/core/Cargo.toml" | head -1 | sed 's/.*"\(.*\)".*/\1/')

if [ "$(id -u)" -ne 0 ]; then
    echo "error: must run as root" >&2
    exit 1
fi

# 0. Locate pre-built cos binary (built by CI or manually before running this script)
COS_BIN="$PROJECT_DIR/target/x86_64-unknown-linux-musl/release/cos"
if [ ! -f "$COS_BIN" ]; then
    COS_BIN="$PROJECT_DIR/core/target/x86_64-unknown-linux-musl/release/cos"
fi
if [ ! -f "$COS_BIN" ]; then
    COS_BIN="$PROJECT_DIR/target/release/cos"
fi
if [ ! -f "$COS_BIN" ]; then
    COS_BIN="$PROJECT_DIR/core/target/release/cos"
fi
if [ ! -f "$COS_BIN" ]; then
    COS_BIN="$PROJECT_DIR/target/x86_64-unknown-linux-gnu/release/cos"
fi
if [ ! -f "$COS_BIN" ]; then
    COS_BIN="$PROJECT_DIR/core/target/x86_64-unknown-linux-gnu/release/cos"
fi
if [ ! -f "$COS_BIN" ]; then
    echo "error: cos binary not found. Build it first: cargo build --release -p cos" >&2
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
    npm install -g typescript tsx
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

# 4. Install Python packages for cos apps (not available via apt)
echo ":: installing Python packages"
chroot "$ROOTFS" pip3 install --break-system-packages --no-cache-dir \
    pymupdf python-docx openpyxl python-pptx pyyaml

# 5. Apply overlay (config files, cos-init, etc.)
echo ":: applying overlay"
cp -a "$SCRIPT_DIR/overlay/." "$ROOTFS/"

# 5a. Inject version from Cargo.toml into runtime files
echo ":: setting version to $COS_VERSION"
sed -i "s/\"version\": \".*\"/\"version\": \"$COS_VERSION\"/" "$ROOTFS/etc/cos/config.json"
sed -i "s/COS_VERSION=\".*\"/COS_VERSION=\"$COS_VERSION\"/" "$ROOTFS/etc/cos/profile.sh"

# 5. Install Rust cos binary
echo ":: installing cos binary"
install -m 755 "$COS_BIN" "$ROOTFS/usr/local/bin/cos"

# 6. Install apps
echo ":: installing apps"
mkdir -p "$ROOTFS/usr/lib/cos/apps"
cp -a "$PROJECT_DIR/apps/." "$ROOTFS/usr/lib/cos/apps/"

# 6b. Install plugins and skills
echo ":: installing plugins and skills"
mkdir -p "$ROOTFS/usr/lib/cos/plugins"
mkdir -p "$ROOTFS/usr/lib/cos/skills"
if [ -d "$PROJECT_DIR/plugins" ]; then
    cp -a "$PROJECT_DIR/plugins/." "$ROOTFS/usr/lib/cos/plugins/"
fi
if [ -d "$PROJECT_DIR/skills" ]; then
    cp -a "$PROJECT_DIR/skills/." "$ROOTFS/usr/lib/cos/skills/"
fi

# 7. Install cos-browser engine (vendored Obscura, Rust)
echo ":: installing cos-browser binary"
COS_BROWSER_BIN="$PROJECT_DIR/target/x86_64-unknown-linux-gnu/release/cos-browser"
if [ ! -f "$COS_BROWSER_BIN" ]; then
    COS_BROWSER_BIN="$PROJECT_DIR/target/release/cos-browser"
fi
if [ ! -f "$COS_BROWSER_BIN" ]; then
    echo "error: cos-browser binary not found. Build it first:" >&2
    echo "  cargo build --release -p cos-browser" >&2
    exit 1
fi
install -m 755 "$COS_BROWSER_BIN" "$ROOTFS/usr/local/bin/cos-browser"

COS_BROWSER_WORKER="$(dirname "$COS_BROWSER_BIN")/cos-browser-worker"
if [ -f "$COS_BROWSER_WORKER" ]; then
    install -m 755 "$COS_BROWSER_WORKER" "$ROOTFS/usr/local/bin/cos-browser-worker"
fi
echo "   installed cos-browser to /usr/local/bin"

# 9. Create runtime directories
mkdir -p "$ROOTFS/den"
mkdir -p "$ROOTFS/var/lib/cos"

# 10. Source COS profile on login
if ! grep -q 'cos/profile.sh' "$ROOTFS/etc/bash.bashrc" 2>/dev/null; then
    echo '[ -f /etc/cos/profile.sh ] && . /etc/cos/profile.sh' >> "$ROOTFS/etc/bash.bashrc"
fi

echo ":: done — rootfs at $ROOTFS"
