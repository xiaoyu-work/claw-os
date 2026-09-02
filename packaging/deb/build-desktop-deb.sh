#!/usr/bin/env bash
# packaging/deb/build-desktop-deb.sh — wrap a staged desktop root into
# claw-os-desktop_<version>_<arch>.deb.
#
# The desktop feature builds inside the target rootfs, then installs the
# workspace into a staging root under $ROOTFS/build. This script adds Debian
# metadata and emits the package into build/debs/.

set -euo pipefail

if [ $# -ne 1 ]; then
    echo "usage: $0 <desktop-package-root>" >&2
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
source "$PROJECT_DIR/scripts/lib/arch.sh"

STAGE_ROOT="$1"
OUT_DIR="$PROJECT_DIR/build/debs"
source "$PROJECT_DIR/scripts/lib/package-version.sh"
VERSION="$(package_version "$PROJECT_DIR")"

if [ ! -d "$STAGE_ROOT" ]; then
    echo "error: desktop package root not found: $STAGE_ROOT" >&2
    exit 1
fi
for binary in cos-agent-ui cos-agent-bridge cos-ask-claw-launcher; do
    if [ ! -x "$STAGE_ROOT/usr/local/bin/$binary" ]; then
        echo "error: required desktop Agent binary missing: $binary" >&2
        exit 1
    fi
done

DPKG_DEB="$(command -v dpkg-deb || true)"
if [ -z "$DPKG_DEB" ]; then
    echo "error: dpkg-deb not found. Install it with: apt-get install dpkg-dev" >&2
    exit 1
fi

if [ "$(id -u)" -eq 0 ]; then
    FAKEROOT=""
elif command -v fakeroot >/dev/null 2>&1; then
    FAKEROOT="fakeroot --"
else
    echo "warning: not root and fakeroot not available — files in .deb will" >&2
    echo "         be owned by uid=$(id -u). Install 'fakeroot' to fix." >&2
    FAKEROOT=""
fi

mkdir -p "$STAGE_ROOT/DEBIAN" "$OUT_DIR"
chmod 0755 "$STAGE_ROOT/DEBIAN"

# The desktop package owns only the explicitly listed COSMIC/panel app
# manifests. Every other app is shipped by claw-os-agent.
DESKTOP_APPS_FILE="$SCRIPT_DIR/claw-os-desktop/apps.list"
mkdir -p "$STAGE_ROOT/usr/lib/cos/apps"
while IFS= read -r app_id; do
    [ -n "$app_id" ] || continue
    app_src="$PROJECT_DIR/apps/$app_id"
    if [ ! -f "$app_src/app.json" ]; then
        echo "error: desktop app listed but missing: $app_id" >&2
        exit 1
    fi
    rm -rf "$STAGE_ROOT/usr/lib/cos/apps/$app_id"
    cp -a "$app_src" "$STAGE_ROOT/usr/lib/cos/apps/$app_id"
done < "$DESKTOP_APPS_FILE"
find "$STAGE_ROOT/usr/lib/cos/apps" -name '__pycache__' -type d \
    -exec rm -rf {} + 2>/dev/null || true

THERMALD_DEP="${THERMALD_PKG:+$THERMALD_PKG, }"
VAAPI_INTEL_NONFREE_DEP="${VAAPI_INTEL_NONFREE_PKG:+$VAAPI_INTEL_NONFREE_PKG, }"

# Release-security metadata. The desktop package owns the graphical
# Agent binaries, so it carries its own signed manifest, preinst and
# prerm gate rather than inheriting the agent's.
RELEASE_SECURITY_DIR="$PROJECT_DIR/packaging/release-security"
RELEASE_SECURITY_POLICY="$RELEASE_SECURITY_DIR/policy.json"
if ! command -v python3 >/dev/null 2>&1; then
    echo "error: python3 is required to generate release-security metadata" >&2
    exit 1
fi
policy_field() {
    python3 - "$RELEASE_SECURITY_POLICY" "$1" <<'PY'
import json, sys
document = json.load(open(sys.argv[1], encoding="utf-8"))
cursor = document
for key in sys.argv[2].split("."):
    cursor = cursor[key]
print(cursor)
PY
}
SECURITY_EPOCH="$(policy_field security_epoch)"
SECURITY_ABI="$(policy_field abi)"
MIN_AGENT_VERSION="$(policy_field minimum_compatible.claw-os-agent)"
MIN_BASE_VERSION="$(policy_field minimum_compatible.claw-os-base)"
MIN_DESKTOP_VERSION="$(policy_field minimum_compatible.claw-os-desktop)"
source "$RELEASE_SECURITY_DIR/sign-manifest.sh"
# The passphrase is needed by exactly one child command. Hold it in a
# shell-local and drop it from the exported environment immediately, so
# no unrelated build step inherits it.
RELEASE_SECURITY_PASSPHRASE="${GPG_PASSPHRASE:-}"
unset GPG_PASSPHRASE
RELEASE_SECURITY_KEY_ID="$(claw_resolve_signing_key)" || exit 1
if [ -z "${SOURCE_DATE_EPOCH:-}" ]; then
    SOURCE_DATE_EPOCH="$(git -C "$PROJECT_DIR" log -1 --pretty=%ct 2>/dev/null || true)"
    [ -n "$SOURCE_DATE_EPOCH" ] && export SOURCE_DATE_EPOCH
fi

DESKTOP_MANIFEST="$STAGE_ROOT/usr/lib/cos/release-security/claw-os-desktop/manifest.json"
install -d -m 0755 "$STAGE_ROOT/usr/lib/cos/release-security"
claw_write_release_manifest \
    "$RELEASE_SECURITY_KEY_ID" "$RELEASE_SECURITY_PASSPHRASE" \
    "$RELEASE_SECURITY_DIR/make-manifest.py" claw-os-desktop "$VERSION" \
    "$DEB_ARCH" "${SUITE:-trixie}" "$STAGE_ROOT" "$RELEASE_SECURITY_POLICY" \
    "$DESKTOP_MANIFEST" || exit 1
# The signing material has done its job; drop the shell-local copy too.
unset RELEASE_SECURITY_PASSPHRASE

sed -e "s/__VERSION__/$VERSION/g" -e "s/__ARCH__/$DEB_ARCH/g" \
    -e "s/__ABI__/$SECURITY_ABI/g" \
    -e "s/__SECURITY_EPOCH__/$SECURITY_EPOCH/g" \
    -e "s/__MIN_AGENT__/$MIN_AGENT_VERSION/g" \
    -e "s/__MIN_BASE__/$MIN_BASE_VERSION/g" \
    -e "s/__MIN_DESKTOP__/$MIN_DESKTOP_VERSION/g" \
    -e "s/__THERMALD_DEP__/$THERMALD_DEP/g" \
    -e "s/__VAAPI_INTEL_NONFREE_DEP__/$VAAPI_INTEL_NONFREE_DEP/g" \
    "$SCRIPT_DIR/claw-os-desktop/control" > "$STAGE_ROOT/DEBIAN/control"
install -m 644 "$SCRIPT_DIR/claw-os-desktop/conffiles" "$STAGE_ROOT/DEBIAN/conffiles"
sed -e "s/__VERSION__/$VERSION/g" \
    "$SCRIPT_DIR/claw-os-desktop/postinst" > "$STAGE_ROOT/DEBIAN/postinst"
chmod 0755 "$STAGE_ROOT/DEBIAN/postinst"
install -m 755 "$SCRIPT_DIR/claw-os-desktop/prerm" "$STAGE_ROOT/DEBIAN/prerm"

# The preinst carries this candidate's own signed manifest: it runs
# before any of the package's files exist on disk.
"$RELEASE_SECURITY_DIR/render-preinst.sh" \
    claw-os-desktop "$VERSION" "$SECURITY_EPOCH" "$STAGE_ROOT" "$STAGE_ROOT/DEBIAN/preinst"

# Debian encodes the epoch colon as %3a in artifact file names.
FILE_VERSION="${VERSION//:/%3a}"
OUT="$OUT_DIR/claw-os-desktop_${FILE_VERSION}_${DEB_ARCH}.deb"
echo ":: dpkg-deb --build claw-os-desktop -> $OUT"
$FAKEROOT "$DPKG_DEB" --root-owner-group --build "$STAGE_ROOT" "$OUT" >/dev/null
