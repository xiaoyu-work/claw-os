#!/usr/bin/env bash
# packaging/release-security/render-preinst.sh — render the shared
# downgrade-protection preinst for one package.
#
# A `preinst` runs before any of its own package's files exist on disk,
# so the candidate has to carry its evidence with it: this embeds the
# package identity, its Debian version, its security epoch and its
# canonical signed release manifest directly into the script.
#
# Usage:
#   render-preinst.sh PACKAGE VERSION EPOCH STAGE_DIR OUTPUT
#
# STAGE_DIR is the package staging root; the manifest is read from
# STAGE_DIR/usr/lib/cos/release-security/PACKAGE/manifest.json and its detached
# signature, when one was produced, from the matching `.asc`.

set -euo pipefail

if [ "$#" -ne 5 ]; then
    echo "usage: $0 PACKAGE VERSION EPOCH STAGE_DIR OUTPUT" >&2
    exit 2
fi

PACKAGE="$1"
VERSION="$2"
EPOCH="$3"
STAGE_DIR="$4"
OUTPUT="$5"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEMPLATE="$SCRIPT_DIR/../deb/common/security-floor.preinst"
MANIFEST="$STAGE_DIR/usr/lib/cos/release-security/$PACKAGE/manifest.json"
SIGNATURE="$STAGE_DIR/usr/lib/cos/release-security/$PACKAGE/manifest.json.asc"

if [ ! -f "$TEMPLATE" ]; then
    echo "error: preinst template not found: $TEMPLATE" >&2
    exit 1
fi
if [ ! -s "$MANIFEST" ]; then
    echo "error: $PACKAGE has no release-security manifest at $MANIFEST" >&2
    exit 1
fi
if [ ! -s "$SIGNATURE" ]; then
    SIGNATURE=/dev/null
fi

awk -v package="$PACKAGE" \
    -v version="$VERSION" \
    -v epoch="$EPOCH" \
    -v manifest="$MANIFEST" \
    -v signature="$SIGNATURE" '
    /^__RELEASE_SECURITY_MANIFEST__$/ {
        while ((getline line < manifest) > 0) print line
        close(manifest)
        next
    }
    /^__RELEASE_SECURITY_SIGNATURE__$/ {
        while ((getline line < signature) > 0) print line
        close(signature)
        next
    }
    {
        gsub(/__PACKAGE__/, package)
        gsub(/__VERSION__/, version)
        gsub(/__SECURITY_EPOCH__/, epoch)
        print
    }
' "$TEMPLATE" > "$OUTPUT"
chmod 0755 "$OUTPUT"

# A rendered preinst that still carries a placeholder would silently
# stop enforcing anything.
if grep -q '__RELEASE_SECURITY_\|__PACKAGE__\|__VERSION__\|__SECURITY_EPOCH__' "$OUTPUT"; then
    echo "error: rendered preinst still contains a placeholder" >&2
    exit 1
fi
bash -n "$OUTPUT"
