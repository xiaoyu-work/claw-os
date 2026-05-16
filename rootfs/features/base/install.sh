#!/usr/bin/env bash
# rootfs/features/base/install.sh — Node.js, Python packages, version
# injection, runtime directories, and login-time profile sourcing.
#
# Invoked by rootfs/build.sh after the base packages.txt has been apt-installed.
# Inherited from environment: ROOTFS, PROJECT_DIR, COS_VERSION.

set -euo pipefail

# Install Node.js 24 (required by cos apps).
NODE_MAJOR=24
echo "  :: installing Node.js $NODE_MAJOR"
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

# Install Python packages used by cos apps that are not available via apt.
echo "  :: installing Python packages"
chroot "$ROOTFS" pip3 install --break-system-packages --no-cache-dir \
    pymupdf python-docx openpyxl python-pptx pyyaml

# Inject version from Cargo.toml into runtime files (overlay was already
# applied by build.sh before features ran). The system overlay no longer
# ships /etc/cos/config.json — agent config lives under
# ~/.config/cos/config.json per user, defaults come from Rust.
echo "  :: injecting version $COS_VERSION"
sed -i "s/COS_VERSION=\".*\"/COS_VERSION=\"$COS_VERSION\"/" "$ROOTFS/etc/cos/profile.sh"
sed -i "s/@COS_VERSION@/$COS_VERSION/g" \
    "$ROOTFS/etc/os-release" \
    "$ROOTFS/usr/lib/os-release" \
    "$ROOTFS/etc/issue" \
    "$ROOTFS/etc/issue.net"

# Runtime directories.
mkdir -p "$ROOTFS/var/lib/cos"

# Source COS profile on every interactive login.
if ! grep -q 'cos/profile.sh' "$ROOTFS/etc/bash.bashrc" 2>/dev/null; then
    echo '[ -f /etc/cos/profile.sh ] && . /etc/cos/profile.sh' >> "$ROOTFS/etc/bash.bashrc"
fi
