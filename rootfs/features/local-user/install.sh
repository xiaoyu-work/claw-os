#!/usr/bin/env bash
# Add a local login account for artifacts without platform provisioning.

set -euo pipefail

source "$PROJECT_DIR/scripts/lib/add-cos-user.sh"

# A graphical image uses its first-boot wizard to create the real user.
# Headless local VM images need a usable account baked into the artifact.
if [ -f "$ROOTFS/etc/greetd/cosmic-greeter.toml" ] \
        && grep -q '^\[initial_session\]' "$ROOTFS/etc/greetd/cosmic-greeter.toml" 2>/dev/null; then
    echo "  :: desktop first-boot wizard present - skipping local 'cos' user"
else
    add_cos_user "$ROOTFS"
    echo "  :: created local 'cos' user"
fi
