#!/usr/bin/env bash
# packaging/deb/build-debs.sh -- assemble claw-os-agent and claw-os-base
# from already-built binaries and source-tree files.
#
# Output: $PROJECT_DIR/build/debs/
#
# Usage:
#   ./packaging/deb/build-debs.sh [all|agent|base]
#
# Required tools: dpkg-deb, fakeroot (or run as root).
# Optional:       gzip, find, install, sed.
#
# Inputs:
#   target/<RUST_TARGET>/release/cos          (built by cargo for $ARCH)
#   target/<RUST_TARGET:gnu>/release/cos-browser  (glibc — V8 needs it)
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
STAGE_DIR="${COS_DEB_STAGE_DIR:-$PROJECT_DIR/build/deb-staging}"

source "$PROJECT_DIR/scripts/lib/package-version.sh"
VERSION="$(package_version "$PROJECT_DIR")"
PACKAGE_SET="${1:-all}"
if [ "$#" -gt 1 ]; then
    echo "usage: $0 [all|agent|base]" >&2
    exit 2
fi
case "$PACKAGE_SET" in
    all|agent|base) ;;
    *)
        echo "error: unknown package set '$PACKAGE_SET'; expected all, agent, or base" >&2
        exit 2
        ;;
esac

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
# The package contract has exactly three names. Never let obsolete local
# artifacts leak into a newly generated repository.
find "$OUT_DIR" -maxdepth 1 -type f -name 'claw-os-*.deb' \
    ! -name 'claw-os-agent_*.deb' \
    ! -name 'claw-os-base_*.deb' \
    ! -name 'claw-os-desktop_*.deb' \
    -delete
case "$PACKAGE_SET" in
    all)
        rm -f "$OUT_DIR"/claw-os-agent_*_"$DEB_ARCH".deb
        rm -f "$OUT_DIR"/claw-os-base_*_all.deb
        ;;
    agent) rm -f "$OUT_DIR"/claw-os-agent_*_"$DEB_ARCH".deb ;;
    base) rm -f "$OUT_DIR"/claw-os-base_*_all.deb ;;
esac

###############################################################################
# Helper: verify and locate a built binary across known target dirs.
#
# Order: prefer the $ARCH-specific Rust target (musl, then gnu, then plain
# release/), then unsuffixed release/ as a final fallback (when cargo was
# invoked without --target on a native build). Every candidate is checked
# against the target Debian architecture before it can enter a package.
###############################################################################
binary_matches_arch() {
    local path="$1" expected_machine machine magic
    magic="$(od -An -tx1 -N4 "$path" 2>/dev/null | tr -d ' \n')"
    if [ "$magic" != "7f454c46" ]; then
        echo "  :: ignoring non-ELF binary candidate: $path" >&2
        return 1
    fi
    machine="$(od -An -tx1 -j18 -N2 "$path" 2>/dev/null | tr -d ' \n')"
    case "$DEB_ARCH" in
        amd64) expected_machine=3e00 ;;
        arm64) expected_machine=b700 ;;
        *)
            echo "error: no ELF machine mapping for Debian arch $DEB_ARCH" >&2
            return 1
            ;;
    esac
    if [ "$machine" != "$expected_machine" ]; then
        echo "  :: ignoring wrong-architecture binary ($machine != $expected_machine): $path" >&2
        return 1
    fi
    return 0
}

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
        if [ -f "$candidate" ] && binary_matches_arch "$candidate"; then
            echo "$candidate"
            return 0
        fi
    done
    return 1
}

###############################################################################
# Helper: make sure `cargo` is on PATH.
#
# This script normally runs as part of `sudo ./build.sh`, so $HOME is /root
# and a rustup toolchain installed under the invoking user's home is not on
# PATH. Probe the usual locations (including the sudo-invoking user's home)
# before giving up.
###############################################################################
ensure_cargo() {
    # Resolve the sudo-invoking user's home up front. Under `sudo ./build.sh`,
    # $HOME is /root but the Rust toolchain is usually installed in the
    # invoking user's home (~/.rustup, ~/.cargo). cargo there is a rustup proxy
    # that, when run as root, would look at /root/.rustup (empty) and fail with
    # "rustup could not choose a version of cargo to run". Point RUSTUP_HOME at
    # the user's toolchain so the existing install is reused (no second,
    # root-owned toolchain). Leave CARGO_HOME at root's default so build caches
    # are not written into the user's home as root.
    local sudo_home=""
    if [ -n "${SUDO_USER:-}" ]; then
        sudo_home="$(getent passwd "$SUDO_USER" 2>/dev/null | cut -d: -f6)"
    fi
    if [ -n "$sudo_home" ] && [ -z "${RUSTUP_HOME:-}" ] && [ -d "$sudo_home/.rustup" ]; then
        export RUSTUP_HOME="$sudo_home/.rustup"
    fi

    # 1. Make sure a `cargo` is on PATH.
    if ! command -v cargo >/dev/null 2>&1; then
        local dir
        for dir in \
            "$HOME/.cargo/bin" \
            "/root/.cargo/bin" \
            "${sudo_home:+$sudo_home/.cargo/bin}"; do
            [ -n "$dir" ] && [ -x "$dir/cargo" ] && { export PATH="$dir:$PATH"; break; }
        done
    fi
    if ! command -v cargo >/dev/null 2>&1; then
        echo "error: cargo not found — the Rust toolchain is required to build the" >&2
        echo "       cos / clawd / cos-browser binaries. Install it with:" >&2
        echo "         curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y" >&2
        echo "         . \"\$HOME/.cargo/env\"" >&2
        echo "       (when building under sudo, the toolchain is still found via" >&2
        echo "       \$SUDO_USER's home, so a normal user-level rustup install is fine)." >&2
        exit 1
    fi
    # 2. Make sure cargo can actually run. When `cargo` is a rustup proxy with
    #    no default toolchain (e.g. installed via `apt install rustup`), it
    #    errors out: "rustup could not choose a version of cargo to run".
    #    Self-heal by installing and selecting the stable toolchain. rustup may
    #    live next to cargo (same dir) even when not on PATH, so look there too.
    if ! cargo --version >/dev/null 2>&1; then
        local rustup_bin
        rustup_bin="$(command -v rustup 2>/dev/null || true)"
        if [ -z "$rustup_bin" ]; then
            local cargo_dir
            cargo_dir="$(dirname "$(command -v cargo)")"
            [ -x "$cargo_dir/rustup" ] && rustup_bin="$cargo_dir/rustup"
        fi
        if [ -n "$rustup_bin" ]; then
            echo "  :: no default Rust toolchain configured — installing stable" >&2
            "$rustup_bin" toolchain install stable >&2 || true
            "$rustup_bin" default stable >&2 || true
        fi
    fi
    if ! cargo --version >/dev/null 2>&1; then
        echo "error: cargo is present but cannot run. If it is a rustup proxy with" >&2
        echo "       no default toolchain, set one with:  rustup default stable" >&2
        exit 1
    fi
    return 0
}

###############################################################################
# Helper: locate a built binary, compiling its crate on demand if missing.
#
# build-debs.sh assembles already-built binaries; CI compiles them as
# separate cached steps before calling the rootfs build. A local
# `./build.sh` has no such step, so compile on demand here the first time a
# required binary is absent. The build always names an explicit target, so a
# cross-enabled packaging run cannot silently produce a host-architecture
# binary. No-op in CI (binaries already present, so find_bin succeeds and
# cargo never runs).
#
# The target defaults to $RUST_TARGET (musl, static). Callers override it for
# crates that cannot link against musl — see cos-browser below.
#
#   $1 = binary name to locate   $2 = cargo package to build if missing
#   $3 = target triple (optional, default $RUST_TARGET)
###############################################################################
ensure_bin() {
    local bin="$1" pkg="$2" target="${3:-$RUST_TARGET}" path
    if path="$(find_bin "$bin")"; then
        echo "$path"
        return 0
    fi
    ensure_cargo
    # rustup installs the host gnu triple by default but musl (and a
    # non-host triple generally) may be missing on a fresh box.
    if command -v rustup >/dev/null 2>&1 \
        && ! rustup target list --installed 2>/dev/null | grep -qx "$target"; then
        echo "  :: rust target $target not installed — adding" >&2
        rustup target add "$target" >&2 || true
    fi
    echo "  :: $bin not built — compiling (cargo build --release --target $target -p $pkg)" >&2
    ( cd "$PROJECT_DIR" && cargo build --release --target "$target" -p "$pkg" ) >&2
    find_bin "$bin"
}

###############################################################################
# Helper: render control file with __VERSION__ and __ARCH__ substituted.
###############################################################################
render_control() {
    local src="$1"
    local dst="$2"
    sed -e "s/__VERSION__/$VERSION/g" -e "s/__ARCH__/$DEB_ARCH/g" "$src" > "$dst"
}

SYSTEM_UNITS_SRC="$PROJECT_DIR/rootfs/features/systemd/overlay/usr/lib/systemd/system"

###############################################################################
# 1. claw-os-agent
###############################################################################
if [ "$PACKAGE_SET" = "all" ] || [ "$PACKAGE_SET" = "agent" ]; then
echo "===> staging claw-os-agent"
AGENT_STAGE="$STAGE_DIR/claw-os-agent"
mkdir -p \
    "$AGENT_STAGE/DEBIAN" \
    "$AGENT_STAGE/etc/cos" \
    "$AGENT_STAGE/usr/lib/cos/apps" \
    "$AGENT_STAGE/usr/lib/cos/python" \
    "$AGENT_STAGE/usr/lib/cos/skills" \
    "$AGENT_STAGE/usr/lib/systemd/system" \
    "$AGENT_STAGE/usr/lib/systemd/user" \
    "$AGENT_STAGE/usr/local/bin" \
    "$AGENT_STAGE/usr/share/polkit-1/actions"
chmod 0755 "$AGENT_STAGE/DEBIAN"

render_control "$SCRIPT_DIR/claw-os-agent/control" "$AGENT_STAGE/DEBIAN/control"
install -m 644 "$SCRIPT_DIR/claw-os-agent/conffiles" "$AGENT_STAGE/DEBIAN/conffiles"
install -m 755 "$SCRIPT_DIR/claw-os-agent/postinst" "$AGENT_STAGE/DEBIAN/postinst"
install -m 755 "$SCRIPT_DIR/claw-os-agent/prerm" "$AGENT_STAGE/DEBIAN/prerm"
install -m 755 "$SCRIPT_DIR/claw-os-agent/postrm" "$AGENT_STAGE/DEBIAN/postrm"

COS_BIN="$(ensure_bin cos cos)" || { echo "error: cos binary not built" >&2; exit 1; }
CLAWD_BIN="$(ensure_bin clawd cos)" || { echo "error: clawd binary not built" >&2; exit 1; }
APPROVAL_HELPER_BIN="$(ensure_bin claw-approval-helper cos)" || {
    echo "error: claw-approval-helper binary not built" >&2; exit 1; }
APP_RUNNER_BIN="$(ensure_bin claw-app-runner cos)" || {
    echo "error: claw-app-runner binary not built" >&2; exit 1; }
MAIL_AI_HOST_BIN="$(ensure_bin claw-mail-ai-host cos)" || {
    echo "error: claw-mail-ai-host binary not built" >&2; exit 1; }

echo "  :: cos                    <- $COS_BIN"
echo "  :: clawd                  <- $CLAWD_BIN"
echo "  :: claw-approval-helper   <- $APPROVAL_HELPER_BIN"
echo "  :: claw-app-runner        <- $APP_RUNNER_BIN"
echo "  :: claw-mail-ai-host      <- $MAIL_AI_HOST_BIN"
install -m 755 "$COS_BIN" "$AGENT_STAGE/usr/local/bin/cos"
install -m 755 "$CLAWD_BIN" "$AGENT_STAGE/usr/local/bin/clawd"
install -m 755 "$APPROVAL_HELPER_BIN" "$AGENT_STAGE/usr/local/bin/claw-approval-helper"
install -m 755 "$APP_RUNNER_BIN" "$AGENT_STAGE/usr/local/bin/claw-app-runner"
install -m 755 "$MAIL_AI_HOST_BIN" "$AGENT_STAGE/usr/lib/cos/claw-mail-ai-host"
install -m 644 \
    "$PROJECT_DIR/rootfs/overlay/usr/share/polkit-1/actions/org.clawos.approval.policy" \
    "$AGENT_STAGE/usr/share/polkit-1/actions/org.clawos.approval.policy"

# V8 only publishes prebuilt libraries for glibc targets, so the browser
# binaries intentionally live beside the musl-built core in the same package.
COS_BROWSER_BIN="$(ensure_bin cos-browser cos-browser "${RUST_TARGET/-musl/-gnu}")" || {
    echo "error: cos-browser binary not built" >&2; exit 1; }
COS_BROWSER_WORKER="$(dirname "$COS_BROWSER_BIN")/cos-browser-worker"
if [ ! -f "$COS_BROWSER_WORKER" ] || ! binary_matches_arch "$COS_BROWSER_WORKER"; then
    echo "error: cos-browser-worker binary not built for $DEB_ARCH" >&2
    exit 1
fi
echo "  :: cos-browser            <- $COS_BROWSER_BIN"
echo "  :: cos-browser-worker     <- $COS_BROWSER_WORKER"
install -m 755 "$COS_BROWSER_BIN" "$AGENT_STAGE/usr/local/bin/cos-browser"
install -m 755 "$COS_BROWSER_WORKER" "$AGENT_STAGE/usr/local/bin/cos-browser-worker"

CLAW_SEMANTIC_DAEMON_BIN="$(
    ensure_bin claw-semantic-daemon claw-semantic "${RUST_TARGET/-musl/-gnu}"
)" || { echo "error: claw-semantic-daemon binary not built" >&2; exit 1; }
CLAW_SEMANTIC_CLI_BIN="$(
    ensure_bin claw-semantic claw-semantic "${RUST_TARGET/-musl/-gnu}"
)" || { echo "error: claw-semantic binary not built" >&2; exit 1; }
echo "  :: claw-semantic-daemon  <- $CLAW_SEMANTIC_DAEMON_BIN"
echo "  :: claw-semantic         <- $CLAW_SEMANTIC_CLI_BIN"
install -m 755 "$CLAW_SEMANTIC_DAEMON_BIN" \
    "$AGENT_STAGE/usr/local/bin/claw-semantic-daemon"
install -m 755 "$CLAW_SEMANTIC_CLI_BIN" "$AGENT_STAGE/usr/local/bin/claw-semantic"

install -m 644 "$PROJECT_DIR/rootfs/overlay/etc/cos/profile.sh" \
    "$AGENT_STAGE/etc/cos/profile.sh"
sed -i "s/COS_VERSION=\".*\"/COS_VERSION=\"$VERSION\"/" \
    "$AGENT_STAGE/etc/cos/profile.sh"

# All non-graphical apps belong to the reusable agent. The manifests in
# apps.list are Agent UI/COSMIC/panel integrations owned by claw-os-desktop.
DESKTOP_APPS_FILE="$SCRIPT_DIR/claw-os-desktop/apps.list"
while IFS= read -r app_id; do
    [ -n "$app_id" ] || continue
    if [ ! -f "$PROJECT_DIR/apps/$app_id/app.json" ]; then
        echo "error: desktop app listed but missing: $app_id" >&2
        exit 1
    fi
done < "$DESKTOP_APPS_FILE"
for app_dir in "$PROJECT_DIR/apps"/*; do
    [ -d "$app_dir" ] || continue
    app_id="$(basename "$app_dir")"
    [ "$app_id" = "__pycache__" ] && continue
    if grep -Fxq "$app_id" "$DESKTOP_APPS_FILE"; then
        continue
    fi
    cp -a "$app_dir" "$AGENT_STAGE/usr/lib/cos/apps/$app_id"
done
find "$AGENT_STAGE/usr/lib/cos/apps" -name '__pycache__' -type d \
    -exec rm -rf {} + 2>/dev/null || true

source_app_count="$(find "$PROJECT_DIR/apps" -mindepth 2 -maxdepth 2 \
    -name app.json -type f | wc -l)"
agent_app_count="$(find "$AGENT_STAGE/usr/lib/cos/apps" -mindepth 2 -maxdepth 2 \
    -name app.json -type f | wc -l)"
desktop_app_count="$(grep -cve '^[[:space:]]*$' "$DESKTOP_APPS_FILE")"
if [ $((agent_app_count + desktop_app_count)) -ne "$source_app_count" ]; then
    echo "error: app package partition is incomplete" >&2
    echo "       source=$source_app_count agent=$agent_app_count desktop=$desktop_app_count" >&2
    exit 1
fi

if [ -d "$PROJECT_DIR/skills" ]; then
    cp -a "$PROJECT_DIR/skills/." "$AGENT_STAGE/usr/lib/cos/skills/"
fi

SDK_PY_SRC="$PROJECT_DIR/claw-os-sdk/python/src/claw_os_sdk"
RUNTIME_PY_SRC="$PROJECT_DIR/cos-runtime/python/src/cos_runtime"
if [ ! -d "$SDK_PY_SRC" ] || [ ! -d "$RUNTIME_PY_SRC" ]; then
    echo "error: Python SDK/runtime source trees are required" >&2
    exit 1
fi
cp -a "$SDK_PY_SRC" "$AGENT_STAGE/usr/lib/cos/python/claw_os_sdk"
cp -a "$RUNTIME_PY_SRC" "$AGENT_STAGE/usr/lib/cos/python/cos_runtime"
find "$AGENT_STAGE/usr/lib/cos/python" -name '__pycache__' -type d \
    -exec rm -rf {} + 2>/dev/null || true

USER_UNITS_SRC="$PROJECT_DIR/rootfs/features/systemd/overlay/usr/lib/systemd/user"
install -m 644 "$SYSTEM_UNITS_SRC/clawd.service" \
    "$AGENT_STAGE/usr/lib/systemd/system/clawd.service"
install -m 644 "$SYSTEM_UNITS_SRC/cos-browser.service" \
    "$AGENT_STAGE/usr/lib/systemd/system/cos-browser.service"
install -m 644 "$USER_UNITS_SRC/claw-recoll-index.service" \
    "$AGENT_STAGE/usr/lib/systemd/user/claw-recoll-index.service"
install -m 644 "$USER_UNITS_SRC/claw-semantic.service" \
    "$AGENT_STAGE/usr/lib/systemd/user/claw-semantic.service"

echo "  :: dpkg-deb --build claw-os-agent"
$FAKEROOT $DPKG_DEB --root-owner-group --build "$AGENT_STAGE" \
    "$OUT_DIR/claw-os-agent_${VERSION}_${DEB_ARCH}.deb" >/dev/null
fi

###############################################################################
# 2. claw-os-base (architecture-independent Claw OS integration)
###############################################################################
if [ "$PACKAGE_SET" = "all" ] || [ "$PACKAGE_SET" = "base" ]; then
echo "===> staging claw-os-base"
BASE_STAGE="$STAGE_DIR/claw-os-base"
mkdir -p \
    "$BASE_STAGE/DEBIAN" \
    "$BASE_STAGE/etc/default" \
    "$BASE_STAGE/usr/lib/cos/init" \
    "$BASE_STAGE/usr/lib/systemd/system" \
    "$BASE_STAGE/usr/local/bin"
chmod 0755 "$BASE_STAGE/DEBIAN"

render_control "$SCRIPT_DIR/claw-os-base/control" "$BASE_STAGE/DEBIAN/control"
install -m 644 "$SCRIPT_DIR/claw-os-base/conffiles" "$BASE_STAGE/DEBIAN/conffiles"
install -m 755 "$SCRIPT_DIR/claw-os-base/postinst" "$BASE_STAGE/DEBIAN/postinst"
install -m 755 "$SCRIPT_DIR/claw-os-base/prerm" "$BASE_STAGE/DEBIAN/prerm"
install -m 755 "$SCRIPT_DIR/claw-os-base/postrm" "$BASE_STAGE/DEBIAN/postrm"

install -m 755 "$PROJECT_DIR/rootfs/overlay/usr/local/bin/cos-init" \
    "$BASE_STAGE/usr/local/bin/cos-init"
install -m 755 "$PROJECT_DIR/rootfs/overlay/usr/lib/cos/init/setup-home.sh" \
    "$BASE_STAGE/usr/lib/cos/init/setup-home.sh"
install -m 644 "$SYSTEM_UNITS_SRC/cos-home-setup.service" \
    "$BASE_STAGE/usr/lib/systemd/system/cos-home-setup.service"
install -m 644 "$PROJECT_DIR/rootfs/features/systemd/overlay/etc/default/cos-home" \
    "$BASE_STAGE/etc/default/cos-home"

echo "  :: dpkg-deb --build claw-os-base"
$FAKEROOT $DPKG_DEB --root-owner-group --build "$BASE_STAGE" \
    "$OUT_DIR/claw-os-base_${VERSION}_all.deb" >/dev/null
fi

###############################################################################
# Done.
###############################################################################
echo ""
echo ":: produced:"
ls -1 "$OUT_DIR"/*.deb | sed 's|^|     |'
