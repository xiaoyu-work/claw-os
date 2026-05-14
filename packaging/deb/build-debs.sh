#!/usr/bin/env bash
# packaging/deb/build-debs.sh — assemble claw-os-{base,browser,systemd}.deb
# from already-built binaries + source-tree files.
#
# Output: $PROJECT_DIR/build/debs/
#
# Required tools: dpkg-deb, fakeroot (or run as root).
# Optional:       gzip, find, install, sed.
#
# Inputs:
#   target/x86_64-unknown-linux-musl/release/cos          (built by cargo)
#   target/x86_64-unknown-linux-gnu/release/cos-browser   (built by cargo)
#   apps/, plugins/, skills/                              (source tree)
#   rootfs/overlay/etc/cos/*, rootfs/overlay/usr/...      (source tree)
#   rootfs/features/systemd/overlay/usr/lib/systemd/...   (source tree)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

OUT_DIR="$PROJECT_DIR/build/debs"
STAGE_DIR="$PROJECT_DIR/build/deb-staging"

# Version: read from core/Cargo.toml — same source of truth as rootfs build.
VERSION="$(grep '^version' "$PROJECT_DIR/core/Cargo.toml" | head -1 | sed 's/.*"\(.*\)".*/\1/')"

DPKG_DEB="$(command -v dpkg-deb || true)"
if [ -z "$DPKG_DEB" ]; then
    echo "error: dpkg-deb not found. Install it with: apt-get install dpkg-dev" >&2
    exit 1
fi

# Prefer fakeroot so files in the .deb are owned by root even when this
# script is invoked unprivileged. Fall back to running directly when
# already root or fakeroot is missing.
if [ "$(id -u)" -eq 0 ]; then
    FAKEROOT=""
elif command -v fakeroot >/dev/null 2>&1; then
    FAKEROOT="fakeroot --"
else
    echo "warning: not root and fakeroot not available — files in .debs will" >&2
    echo "         be owned by uid=$(id -u). Install 'fakeroot' to fix." >&2
    FAKEROOT=""
fi

echo ":: claw-os deb build — version $VERSION"

rm -rf "$STAGE_DIR"
mkdir -p "$STAGE_DIR" "$OUT_DIR"

###############################################################################
# Helper: locate a built binary across known target dirs.
###############################################################################
find_bin() {
    local name="$1"
    local candidate
    for candidate in \
        "$PROJECT_DIR/target/x86_64-unknown-linux-musl/release/$name" \
        "$PROJECT_DIR/target/x86_64-unknown-linux-gnu/release/$name" \
        "$PROJECT_DIR/target/release/$name" \
        "$PROJECT_DIR/core/target/x86_64-unknown-linux-musl/release/$name" \
        "$PROJECT_DIR/core/target/release/$name" \
        "$PROJECT_DIR/desktop/agent/target/x86_64-unknown-linux-musl/release/$name" \
        "$PROJECT_DIR/desktop/agent/target/release/$name"; do
        if [ -f "$candidate" ]; then
            echo "$candidate"
            return 0
        fi
    done
    return 1
}

###############################################################################
# Helper: render control file with __VERSION__ substituted.
###############################################################################
render_control() {
    local src="$1"
    local dst="$2"
    sed "s/__VERSION__/$VERSION/g" "$src" > "$dst"
}

###############################################################################
# 1. claw-os-base
###############################################################################
echo "===> staging claw-os-base"
BASE_STAGE="$STAGE_DIR/claw-os-base"
mkdir -p "$BASE_STAGE/DEBIAN"
mkdir -p "$BASE_STAGE/usr/local/bin"
mkdir -p "$BASE_STAGE/usr/lib/cos/apps"
mkdir -p "$BASE_STAGE/usr/lib/cos/plugins"
mkdir -p "$BASE_STAGE/usr/lib/cos/skills"
mkdir -p "$BASE_STAGE/usr/lib/cos/init"
mkdir -p "$BASE_STAGE/usr/share/cos-agent/web"
mkdir -p "$BASE_STAGE/etc/cos"

# Control + maintainer scripts.
render_control "$SCRIPT_DIR/claw-os-base/control" "$BASE_STAGE/DEBIAN/control"
install -m 644 "$SCRIPT_DIR/claw-os-base/conffiles" "$BASE_STAGE/DEBIAN/conffiles"
install -m 755 "$SCRIPT_DIR/claw-os-base/postinst" "$BASE_STAGE/DEBIAN/postinst"

# Binary: cos.
COS_BIN="$(find_bin cos)" || { echo "error: cos binary not built" >&2; exit 1; }
echo "  :: cos          <- $COS_BIN"
install -m 755 "$COS_BIN" "$BASE_STAGE/usr/local/bin/cos"

# Binary: cos-agent-bridge (HTTP+SSE daemon for com.clawos.Agent).
# Optional — the rest of the OS works without it, so a missing bridge
# binary just produces a warning rather than failing the deb build.
COS_AGENT_BRIDGE_BIN="$(find_bin cos-agent-bridge || true)"
if [ -n "$COS_AGENT_BRIDGE_BIN" ] && [ -f "$COS_AGENT_BRIDGE_BIN" ]; then
    echo "  :: cos-agent-bridge  <- $COS_AGENT_BRIDGE_BIN"
    install -m 755 "$COS_AGENT_BRIDGE_BIN" "$BASE_STAGE/usr/local/bin/cos-agent-bridge"
else
    echo "  :: WARNING — cos-agent-bridge binary not built; skipping" >&2
fi

# Agent web SPA: ship whatever Next/Vite export lives at
# desktop/agent/web/out or .next/standalone. Falls back to a stub
# index.html so the bridge's static file server has something to
# return until the UI build is wired into CI.
AGENT_WEB_SRC=""
for candidate in \
    "$PROJECT_DIR/desktop/agent/web/out" \
    "$PROJECT_DIR/desktop/agent/web/.next/standalone/desktop/agent/web/public"; do
    if [ -d "$candidate" ] && [ -f "$candidate/index.html" ]; then
        AGENT_WEB_SRC="$candidate"
        break
    fi
done
if [ -n "$AGENT_WEB_SRC" ]; then
    echo "  :: cos-agent web SPA <- $AGENT_WEB_SRC"
    cp -a "$AGENT_WEB_SRC/." "$BASE_STAGE/usr/share/cos-agent/web/"
else
    echo "  :: cos-agent web SPA — no export found, writing placeholder index.html" >&2
    cat > "$BASE_STAGE/usr/share/cos-agent/web/index.html" <<'PLACEHOLDER'
<!doctype html>
<html><head><meta charset="utf-8"><title>ClawOS Agent</title></head>
<body><pre>ClawOS Agent — web UI not yet built into this image.
The agent bridge is running on this port; talk to /api/chat directly.</pre></body></html>
PLACEHOLDER
fi

# Shell scripts shared with all targets.
install -m 755 "$PROJECT_DIR/rootfs/overlay/usr/local/bin/cos-init" \
    "$BASE_STAGE/usr/local/bin/cos-init"
install -m 755 "$PROJECT_DIR/rootfs/overlay/usr/lib/cos/init/setup-den.sh" \
    "$BASE_STAGE/usr/lib/cos/init/setup-den.sh"

# Config files (declared as conffiles above).
install -m 644 "$PROJECT_DIR/rootfs/overlay/etc/cos/config.json" \
    "$BASE_STAGE/etc/cos/config.json"
install -m 644 "$PROJECT_DIR/rootfs/overlay/etc/cos/profile.sh" \
    "$BASE_STAGE/etc/cos/profile.sh"

# Inject the version into config.json + profile.sh so the binaries and
# scripts agree on the same string at runtime.
sed -i "s/\"version\": \".*\"/\"version\": \"$VERSION\"/" "$BASE_STAGE/etc/cos/config.json"
sed -i "s/COS_VERSION=\".*\"/COS_VERSION=\"$VERSION\"/" "$BASE_STAGE/etc/cos/profile.sh"

# Apps, plugins, skills.
if [ -d "$PROJECT_DIR/apps" ]; then
    cp -a "$PROJECT_DIR/apps/." "$BASE_STAGE/usr/lib/cos/apps/"
fi
if [ -d "$PROJECT_DIR/plugins" ]; then
    cp -a "$PROJECT_DIR/plugins/." "$BASE_STAGE/usr/lib/cos/plugins/"
fi
if [ -d "$PROJECT_DIR/skills" ]; then
    cp -a "$PROJECT_DIR/skills/." "$BASE_STAGE/usr/lib/cos/skills/"
fi

# Build the .deb.
echo "  :: dpkg-deb --build claw-os-base"
$FAKEROOT $DPKG_DEB --root-owner-group --build "$BASE_STAGE" \
    "$OUT_DIR/claw-os-base_${VERSION}_amd64.deb" >/dev/null

###############################################################################
# 2. claw-os-browser
###############################################################################
echo "===> staging claw-os-browser"
BROWSER_STAGE="$STAGE_DIR/claw-os-browser"
mkdir -p "$BROWSER_STAGE/DEBIAN"
mkdir -p "$BROWSER_STAGE/usr/local/bin"
mkdir -p "$BROWSER_STAGE/usr/lib/cos/services/browser"

render_control "$SCRIPT_DIR/claw-os-browser/control" "$BROWSER_STAGE/DEBIAN/control"

COS_BROWSER_BIN="$(find_bin cos-browser)" || {
    echo "error: cos-browser binary not built" >&2; exit 1; }
echo "  :: cos-browser  <- $COS_BROWSER_BIN"
install -m 755 "$COS_BROWSER_BIN" "$BROWSER_STAGE/usr/local/bin/cos-browser"

COS_BROWSER_WORKER="$(dirname "$COS_BROWSER_BIN")/cos-browser-worker"
if [ -f "$COS_BROWSER_WORKER" ]; then
    echo "  :: cos-browser-worker  <- $COS_BROWSER_WORKER"
    install -m 755 "$COS_BROWSER_WORKER" "$BROWSER_STAGE/usr/local/bin/cos-browser-worker"
fi

install -m 644 "$PROJECT_DIR/rootfs/overlay/usr/lib/cos/services/browser/service.json" \
    "$BROWSER_STAGE/usr/lib/cos/services/browser/service.json"

echo "  :: dpkg-deb --build claw-os-browser"
$FAKEROOT $DPKG_DEB --root-owner-group --build "$BROWSER_STAGE" \
    "$OUT_DIR/claw-os-browser_${VERSION}_amd64.deb" >/dev/null

###############################################################################
# 3. claw-os-systemd (arch all)
###############################################################################
echo "===> staging claw-os-systemd"
SYSTEMD_STAGE="$STAGE_DIR/claw-os-systemd"
mkdir -p "$SYSTEMD_STAGE/DEBIAN"
mkdir -p "$SYSTEMD_STAGE/usr/lib/systemd/system"
mkdir -p "$SYSTEMD_STAGE/usr/lib/systemd/user"

render_control "$SCRIPT_DIR/claw-os-systemd/control" "$SYSTEMD_STAGE/DEBIAN/control"
install -m 755 "$SCRIPT_DIR/claw-os-systemd/postinst" "$SYSTEMD_STAGE/DEBIAN/postinst"
install -m 755 "$SCRIPT_DIR/claw-os-systemd/prerm"    "$SYSTEMD_STAGE/DEBIAN/prerm"
install -m 755 "$SCRIPT_DIR/claw-os-systemd/postrm"   "$SYSTEMD_STAGE/DEBIAN/postrm"

UNITS_SRC="$PROJECT_DIR/rootfs/features/systemd/overlay/usr/lib/systemd/system"
install -m 644 "$UNITS_SRC/cos-den-setup.service" \
    "$SYSTEMD_STAGE/usr/lib/systemd/system/cos-den-setup.service"
install -m 644 "$UNITS_SRC/cos-browser.service" \
    "$SYSTEMD_STAGE/usr/lib/systemd/system/cos-browser.service"

# User-scoped unit: auto-start cos-agent-bridge in every logged-in
# user's graphical session. Enabled globally by the postinst.
USER_UNITS_SRC="$PROJECT_DIR/rootfs/features/systemd/overlay/usr/lib/systemd/user"
install -m 644 "$USER_UNITS_SRC/cos-agent-bridge.service" \
    "$SYSTEMD_STAGE/usr/lib/systemd/user/cos-agent-bridge.service"

echo "  :: dpkg-deb --build claw-os-systemd"
$FAKEROOT $DPKG_DEB --root-owner-group --build "$SYSTEMD_STAGE" \
    "$OUT_DIR/claw-os-systemd_${VERSION}_all.deb" >/dev/null

###############################################################################
# Done.
###############################################################################
echo ""
echo ":: produced:"
ls -1 "$OUT_DIR"/*.deb | sed 's|^|     |'
