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
#   target/<RUST_TARGET>/release/cos          (built by cargo for $ARCH)
#   target/<RUST_TARGET>/release/cos-browser  (built by cargo for $ARCH)
#   apps/, skills/                                        (source tree)
#   rootfs/overlay/etc/cos/*, rootfs/overlay/usr/...      (source tree)
#   rootfs/features/systemd/overlay/usr/lib/systemd/...   (source tree)
#
# Architecture: $ARCH (default = host). Switches both the Rust target
# triple searched for binaries and the `Architecture:` field in the
# emitted .deb. Native-only — see scripts/lib/arch.sh.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

source "$PROJECT_DIR/scripts/lib/arch.sh"

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

echo ":: claw-os deb build — version $VERSION arch $DEB_ARCH"

rm -rf "$STAGE_DIR"
mkdir -p "$STAGE_DIR" "$OUT_DIR"

###############################################################################
# Helper: locate a built binary across known target dirs.
#
# Order: prefer the $ARCH-specific Rust target (musl, then gnu, then plain
# release/), then unsuffixed release/ as a final fallback (when cargo was
# invoked without --target on a native build).
###############################################################################
find_bin() {
    local name="$1"
    local gnu_target="${RUST_TARGET/-musl/-gnu}"
    local candidate
    for candidate in \
        "$PROJECT_DIR/target/$RUST_TARGET/release/$name" \
        "$PROJECT_DIR/target/$gnu_target/release/$name" \
        "$PROJECT_DIR/target/release/$name" \
        "$PROJECT_DIR/core/target/$RUST_TARGET/release/$name" \
        "$PROJECT_DIR/core/target/release/$name" \
        "$PROJECT_DIR/desktop/agent/target/$RUST_TARGET/release/$name" \
        "$PROJECT_DIR/desktop/agent/target/release/$name"; do
        if [ -f "$candidate" ]; then
            echo "$candidate"
            return 0
        fi
    done
    return 1
}

###############################################################################
# Helper: render control file with __VERSION__ and __ARCH__ substituted.
###############################################################################
render_control() {
    local src="$1"
    local dst="$2"
    sed -e "s/__VERSION__/$VERSION/g" -e "s/__ARCH__/$DEB_ARCH/g" "$src" > "$dst"
}

###############################################################################
# 1. claw-os-base
###############################################################################
echo "===> staging claw-os-base"
BASE_STAGE="$STAGE_DIR/claw-os-base"
mkdir -p "$BASE_STAGE/DEBIAN"
mkdir -p "$BASE_STAGE/usr/local/bin"
mkdir -p "$BASE_STAGE/usr/lib/cos/apps"
mkdir -p "$BASE_STAGE/usr/lib/cos/skills"
mkdir -p "$BASE_STAGE/usr/lib/cos/init"
mkdir -p "$BASE_STAGE/usr/lib/cos/python"
mkdir -p "$BASE_STAGE/usr/share/applications"
mkdir -p "$BASE_STAGE/etc/cos"

# Control + maintainer scripts.
render_control "$SCRIPT_DIR/claw-os-base/control" "$BASE_STAGE/DEBIAN/control"
install -m 644 "$SCRIPT_DIR/claw-os-base/conffiles" "$BASE_STAGE/DEBIAN/conffiles"
install -m 755 "$SCRIPT_DIR/claw-os-base/postinst" "$BASE_STAGE/DEBIAN/postinst"

# Binary: cos.
COS_BIN="$(find_bin cos)" || { echo "error: cos binary not built" >&2; exit 1; }
echo "  :: cos          <- $COS_BIN"
install -m 755 "$COS_BIN" "$BASE_STAGE/usr/local/bin/cos"

# Binary: clawd (system agent daemon).
CLAWD_BIN="$(find_bin clawd)" || { echo "error: clawd binary not built" >&2; exit 1; }
echo "  :: clawd        <- $CLAWD_BIN"
install -m 755 "$CLAWD_BIN" "$BASE_STAGE/usr/local/bin/clawd"

# Binaries: claw-semantic-daemon + claw-semantic CLI.
# Both are optional — the OS still works without them (Recoll covers
# the keyword layer); a missing binary just disables the semantic
# search layer and apps/docs.search falls back to recoll-only.
CLAW_SEMANTIC_DAEMON_BIN="$(find_bin claw-semantic-daemon || true)"
if [ -n "$CLAW_SEMANTIC_DAEMON_BIN" ] && [ -f "$CLAW_SEMANTIC_DAEMON_BIN" ]; then
    echo "  :: claw-semantic-daemon  <- $CLAW_SEMANTIC_DAEMON_BIN"
    install -m 755 "$CLAW_SEMANTIC_DAEMON_BIN" "$BASE_STAGE/usr/local/bin/claw-semantic-daemon"
else
    echo "  :: WARNING — claw-semantic-daemon binary not built; semantic search disabled" >&2
fi

CLAW_SEMANTIC_CLI_BIN="$(find_bin claw-semantic || true)"
if [ -n "$CLAW_SEMANTIC_CLI_BIN" ] && [ -f "$CLAW_SEMANTIC_CLI_BIN" ]; then
    echo "  :: claw-semantic         <- $CLAW_SEMANTIC_CLI_BIN"
    install -m 755 "$CLAW_SEMANTIC_CLI_BIN" "$BASE_STAGE/usr/local/bin/claw-semantic"
fi

# Shell scripts shared with all targets.
install -m 755 "$PROJECT_DIR/rootfs/overlay/usr/local/bin/cos-init" \
    "$BASE_STAGE/usr/local/bin/cos-init"
install -m 755 "$PROJECT_DIR/rootfs/overlay/usr/lib/cos/init/setup-home.sh" \
    "$BASE_STAGE/usr/lib/cos/init/setup-home.sh"

# Config files (declared as conffiles above). Agent config is per-user
# (~/.config/cos/config.json, written by `cos agent setup`); only the
# profile shim is shipped system-wide here.
install -m 644 "$PROJECT_DIR/rootfs/overlay/etc/cos/profile.sh" \
    "$BASE_STAGE/etc/cos/profile.sh"

# Inject the version into profile.sh so the binaries and scripts
# agree on the same string at runtime.
sed -i "s/COS_VERSION=\".*\"/COS_VERSION=\"$VERSION\"/" "$BASE_STAGE/etc/cos/profile.sh"

# Apps, skills.
if [ -d "$PROJECT_DIR/apps" ]; then
    cp -a "$PROJECT_DIR/apps/." "$BASE_STAGE/usr/lib/cos/apps/"
fi
if [ -d "$PROJECT_DIR/skills" ]; then
    cp -a "$PROJECT_DIR/skills/." "$BASE_STAGE/usr/lib/cos/skills/"
fi

# Python packages shipped system-wide so every bundled app can
# `from claw_os_sdk import ai` (public AI surface) and
# `from cos_runtime import policy` (internal capability gate). The
# `cos` kernel's app-spawn wrapper (core/src/bridge.rs) and its
# MCP-session helper (core/src/agent/tools/cos_apps_session.rs) both
# seed sys.path / PYTHONPATH from /usr/lib/cos/python first, so a
# deb-only install needs the packages to live exactly here.
SDK_PY_SRC="$PROJECT_DIR/claw-os-sdk/python/src/claw_os_sdk"
RUNTIME_PY_SRC="$PROJECT_DIR/cos-runtime/python/src/cos_runtime"
if [ -d "$SDK_PY_SRC" ]; then
    echo "  :: claw_os_sdk python  <- $SDK_PY_SRC"
    cp -a "$SDK_PY_SRC" "$BASE_STAGE/usr/lib/cos/python/claw_os_sdk"
    find "$BASE_STAGE/usr/lib/cos/python/claw_os_sdk" -name '__pycache__' -type d \
        -exec rm -rf {} + 2>/dev/null || true
else
    echo "  :: WARNING — claw-os-sdk python tree missing at $SDK_PY_SRC" >&2
fi
if [ -d "$RUNTIME_PY_SRC" ]; then
    echo "  :: cos_runtime python  <- $RUNTIME_PY_SRC"
    cp -a "$RUNTIME_PY_SRC" "$BASE_STAGE/usr/lib/cos/python/cos_runtime"
    find "$BASE_STAGE/usr/lib/cos/python/cos_runtime" -name '__pycache__' -type d \
        -exec rm -rf {} + 2>/dev/null || true
else
    echo "  :: WARNING — cos-runtime python tree missing at $RUNTIME_PY_SRC" >&2
fi

# Desktop launchers for ClawOS-specific apps (e.g. com.clawos.Agent).
# These live in the rootfs overlay so they ship even on overlay-only
# rootfs builds; here we mirror them into the deb so apt-based
# installs (WSL, Docker upgrades) also get them.
DESKTOP_OVERLAY="$PROJECT_DIR/rootfs/overlay/usr/share/applications"
if [ -d "$DESKTOP_OVERLAY" ]; then
    for desktop_file in "$DESKTOP_OVERLAY"/*.desktop; do
        [ -e "$desktop_file" ] || continue
        echo "  :: $(basename "$desktop_file")"
        install -m 644 "$desktop_file" \
            "$BASE_STAGE/usr/share/applications/$(basename "$desktop_file")"
    done
fi

# Hicolor icons for ClawOS-specific apps (clawos-agent.png ladder).
# Shipped here in the base deb so the .desktop launchers above resolve
# even without the full claw-os-desktop install.
ICON_OVERLAY="$PROJECT_DIR/rootfs/features/desktop/overlay/usr/share/icons"
if [ -d "$ICON_OVERLAY" ]; then
    for icon_path in "$ICON_OVERLAY"/hicolor/*/apps/clawos-agent.png; do
        [ -e "$icon_path" ] || continue
        rel="${icon_path#$ICON_OVERLAY/}"
        mkdir -p "$BASE_STAGE/usr/share/icons/$(dirname "$rel")"
        install -m 644 "$icon_path" "$BASE_STAGE/usr/share/icons/$rel"
    done
fi

# Build the .deb.
echo "  :: dpkg-deb --build claw-os-base"
$FAKEROOT $DPKG_DEB --root-owner-group --build "$BASE_STAGE" \
    "$OUT_DIR/claw-os-base_${VERSION}_${DEB_ARCH}.deb" >/dev/null

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
    "$OUT_DIR/claw-os-browser_${VERSION}_${DEB_ARCH}.deb" >/dev/null

###############################################################################
# 3. claw-os-systemd (arch all)
###############################################################################
echo "===> staging claw-os-systemd"
SYSTEMD_STAGE="$STAGE_DIR/claw-os-systemd"
mkdir -p "$SYSTEMD_STAGE/DEBIAN"
mkdir -p "$SYSTEMD_STAGE/usr/lib/systemd/system"
mkdir -p "$SYSTEMD_STAGE/usr/lib/systemd/user"

render_control "$SCRIPT_DIR/claw-os-systemd/control" "$SYSTEMD_STAGE/DEBIAN/control"
install -m 644 "$SCRIPT_DIR/claw-os-systemd/conffiles" "$SYSTEMD_STAGE/DEBIAN/conffiles"
install -m 755 "$SCRIPT_DIR/claw-os-systemd/postinst" "$SYSTEMD_STAGE/DEBIAN/postinst"
install -m 755 "$SCRIPT_DIR/claw-os-systemd/prerm"    "$SYSTEMD_STAGE/DEBIAN/prerm"
install -m 755 "$SCRIPT_DIR/claw-os-systemd/postrm"   "$SYSTEMD_STAGE/DEBIAN/postrm"

UNITS_SRC="$PROJECT_DIR/rootfs/features/systemd/overlay/usr/lib/systemd/system"
install -m 644 "$UNITS_SRC/cos-home-setup.service" \
    "$SYSTEMD_STAGE/usr/lib/systemd/system/cos-home-setup.service"
install -m 644 "$UNITS_SRC/cos-browser.service" \
    "$SYSTEMD_STAGE/usr/lib/systemd/system/cos-browser.service"
install -m 644 "$UNITS_SRC/clawd.service" \
    "$SYSTEMD_STAGE/usr/lib/systemd/system/clawd.service"

# Admin-editable default for cos-home-setup.service.
DEFAULTS_SRC="$PROJECT_DIR/rootfs/features/systemd/overlay/etc/default"
mkdir -p "$SYSTEMD_STAGE/etc/default"
install -m 644 "$DEFAULTS_SRC/cos-home" \
    "$SYSTEMD_STAGE/etc/default/cos-home"

# User-scoped units: cos-agent-bridge starts in graphical sessions and
# connects to the system clawd socket. Enabled globally by the postinst.
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
