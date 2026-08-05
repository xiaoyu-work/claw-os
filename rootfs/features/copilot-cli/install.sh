#!/usr/bin/env bash
# rootfs/features/copilot-cli/install.sh — install the GitHub Copilot CLI
# (`@github/copilot`) globally inside the chroot so `copilot` lands on
# $PATH for every shell.
#
# Used by cosmic-term's `@`-trigger AI integration (desktop/term/src/ai/)
# which exec's `copilot -p "<prompt>" --allow-all-tools` from a shell
# function. The first time a user runs it, copilot itself walks them
# through OAuth device-flow auth and writes credentials to
# ~/.config/github-copilot/.
#
# Inherited: $ROOTFS.

set -euo pipefail

# Sanity check: nodejs + npm must already be installed via packages.txt.
if ! chroot "$ROOTFS" /usr/bin/env -i PATH=/usr/bin:/bin which npm >/dev/null 2>&1; then
    echo "  error: npm not present in rootfs — packages.txt should pull it in" >&2
    exit 1
fi

echo "  :: installing @github/copilot (global) via npm"
# `--unsafe-perm` lets npm run install scripts as root inside the chroot
# without dropping privileges (which fails since UID mapping is faked).
chroot "$ROOTFS" /usr/bin/env -i \
    PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    HOME=/root \
    npm install -g --unsafe-perm @github/copilot

# Verify the entrypoint landed.
if [ ! -x "$ROOTFS/usr/bin/copilot" ] && [ ! -x "$ROOTFS/usr/local/bin/copilot" ]; then
    echo "  error: copilot binary not found after npm install -g" >&2
    exit 1
fi

echo "  :: copilot-cli feature applied"
