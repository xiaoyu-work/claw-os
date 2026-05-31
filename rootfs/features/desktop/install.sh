#!/usr/bin/env bash
# rootfs/features/desktop/install.sh — build the claw-os desktop from
# source (vendored under PROJECT_DIR/desktop) and wire it up so the rootfs
# boots into a Wayland login.
#
# Target distro: Debian 13 "trixie" (kernel 6.12 LTS, PipeWire 1.4, Mesa 24).
#
# Inputs (env):
#   ROOTFS       — target rootfs (from rootfs/build.sh)
#   PROJECT_DIR  — claw-os repo root (from rootfs/build.sh)
#   SCRIPT_DIR   — rootfs/ dir (from rootfs/build.sh)
#   DESKTOP_SRC  — optional override; otherwise $PROJECT_DIR/desktop
#
# Skipping: set DESKTOP_SKIP=1 to install runtime deps + overlay only and
# skip the ~30-60min cargo build. Useful when iterating on packages.txt /
# overlay / wiring without rebuilding the binaries.

set -euo pipefail

DESKTOP_SRC="${DESKTOP_SRC:-$PROJECT_DIR/desktop}"
FEATURE_DIR="$SCRIPT_DIR/features/desktop"
DESKTOP_PACKAGE_ROOT="$ROOTFS/build/claw-os-desktop-root"
DESKTOP_PACKAGE_ROOT_CHROOT="/build/claw-os-desktop-root"

# ---------------------------------------------------------------------------
# 0. Prepare package staging.
# ---------------------------------------------------------------------------
if [ "${DESKTOP_SKIP:-0}" = "1" ]; then
    if [ -d "$FEATURE_DIR/overlay" ] && [ -n "$(ls -A "$FEATURE_DIR/overlay" 2>/dev/null)" ]; then
        echo "  :: applying desktop overlay"
        cp -a --no-preserve=ownership "$FEATURE_DIR/overlay/." "$ROOTFS/"
    fi
    echo "  :: DESKTOP_SKIP=1 — runtime deps + overlay applied, build skipped"
    echo "  :: NOTE: login chain not wired (greeter binary missing). Re-run"
    echo "         without DESKTOP_SKIP to get a bootable graphical session."
    exit 0
fi

rm -rf "$DESKTOP_PACKAGE_ROOT"
mkdir -p "$DESKTOP_PACKAGE_ROOT"
if [ -d "$FEATURE_DIR/overlay" ] && [ -n "$(ls -A "$FEATURE_DIR/overlay" 2>/dev/null)" ]; then
    echo "  :: staging desktop overlay"
    cp -a --no-preserve=ownership "$FEATURE_DIR/overlay/." "$DESKTOP_PACKAGE_ROOT/"
    # /etc/environment is owned by the base system on many Debian installs.
    # The desktop package merges cursor defaults into it from postinst instead
    # of trying to own the file directly.
    rm -f "$DESKTOP_PACKAGE_ROOT/etc/environment"
    # These icons are shipped by claw-os-base with the agent .desktop launcher.
    # Keep desktop from owning them too.
    find "$DESKTOP_PACKAGE_ROOT/usr/share/icons/hicolor" \
        -path '*/apps/clawos-agent.png' -type f -delete 2>/dev/null || true
fi

# ---------------------------------------------------------------------------
# 1. Validate source tree.
# ---------------------------------------------------------------------------

if [ ! -d "$DESKTOP_SRC" ] || [ ! -f "$DESKTOP_SRC/justfile" ]; then
    cat >&2 <<EOF
  error: desktop source not found at $DESKTOP_SRC
  Expected an in-tree vendored copy with a top-level justfile.
  Either:
    1. Run from a checked-out claw-os tree (desktop/ should exist).
    2. Set DESKTOP_SRC=/path/to/source-tree and re-run.
    3. Set DESKTOP_SKIP=1 to install runtime deps + overlay only (no DE binaries).
EOF
    exit 1
fi

echo "  :: validating desktop source tree at $DESKTOP_SRC"
missing=0
for sub in comp session panel launcher settings greeter toolkit; do
    [ -e "$DESKTOP_SRC/$sub" ] || { echo "    missing: $sub"; missing=1; }
done
[ "$missing" = "0" ] || {
    echo "  error: desktop source tree is incomplete"
    exit 1
}

# ---------------------------------------------------------------------------
# 2. Build the desktop inside the chroot so binaries link against rootfs
#    glibc, not the host's.
#
#    Several desktop/* crates have `path = "../../crates/<x>"` dependencies
#    pointing at the top-level repo `crates/` directory (claw-bridge,
#    cos-mcp-serve, …). Bind-mount that too so the relative path resolves
#    inside the chroot (../../crates from /build/desktop-src/<x> →
#    /build/crates).
#
#    Likewise desktop/{term,edit,files,launcher}/Cargo.toml depend on
#    `path = "../../cos-runtime/rust"` for the internal SDK that wraps
#    every `cos app <id> <verb>` call (audit + caps + snapshot). Without
#    this mount cargo cannot resolve cos-runtime inside the chroot and
#    the desktop build fails before producing a single binary.
#
#    And cos-runtime itself pulls in `claw-os-sdk = { path =
#    "../../claw-os-sdk/rust" }` (the public app-developer SDK that
#    cos-runtime layers audit/caps on top of). Bind-mount that one too
#    or cargo dies in the desktop crates with "failed to read
#    /build/claw-os-sdk/rust/Cargo.toml".
# ---------------------------------------------------------------------------
CHROOT_SRC="$ROOTFS/build/desktop-src"
CHROOT_CRATES="$ROOTFS/build/crates"
CHROOT_RUNTIME="$ROOTFS/build/cos-runtime"
CHROOT_SDK="$ROOTFS/build/claw-os-sdk"
PROJECT_CRATES="$PROJECT_DIR/crates"
PROJECT_RUNTIME="$PROJECT_DIR/cos-runtime"
PROJECT_SDK="$PROJECT_DIR/claw-os-sdk"
mkdir -p "$CHROOT_SRC"
if ! mountpoint -q "$CHROOT_SRC"; then
    mount --bind "$DESKTOP_SRC" "$CHROOT_SRC"
fi
if [ -d "$PROJECT_CRATES" ]; then
    mkdir -p "$CHROOT_CRATES"
    if ! mountpoint -q "$CHROOT_CRATES"; then
        mount --bind "$PROJECT_CRATES" "$CHROOT_CRATES"
    fi
fi
if [ -d "$PROJECT_RUNTIME" ]; then
    mkdir -p "$CHROOT_RUNTIME"
    if ! mountpoint -q "$CHROOT_RUNTIME"; then
        mount --bind "$PROJECT_RUNTIME" "$CHROOT_RUNTIME"
    fi
fi
if [ -d "$PROJECT_SDK" ]; then
    mkdir -p "$CHROOT_SDK"
    if ! mountpoint -q "$CHROOT_SDK"; then
        mount --bind "$PROJECT_SDK" "$CHROOT_SDK"
    fi
fi

cleanup() {
    umount "$CHROOT_SDK" 2>/dev/null || true
    rmdir "$CHROOT_SDK" 2>/dev/null || true
    umount "$CHROOT_RUNTIME" 2>/dev/null || true
    rmdir "$CHROOT_RUNTIME" 2>/dev/null || true
    umount "$CHROOT_CRATES" 2>/dev/null || true
    rmdir "$CHROOT_CRATES" 2>/dev/null || true
    umount "$CHROOT_SRC" 2>/dev/null || true
    rmdir "$CHROOT_SRC" 2>/dev/null || true
    rmdir "$ROOTFS/build" 2>/dev/null || true
}
trap cleanup EXIT

# Rust toolchain inside the chroot. `rustup` package is in trixie; we use
# the minimal stable profile to keep image size down.
echo "  :: ensuring rustup toolchain in chroot"
chroot "$ROOTFS" bash -c '
    set -e
    # rustup show active-toolchain exits 0 even when nothing is configured,
    # so check `rustup default` instead. Output is empty when no default set.
    if [ -z "$(rustup default 2>/dev/null)" ]; then
        rustup toolchain install stable --profile minimal
        rustup default stable
    fi
    export PATH="/root/.cargo/bin:$PATH"
    command -v just >/dev/null || cargo install --quiet just
'

echo "  :: building desktop (cold tree: 30–60 minutes)"
# Several desktop crates (greeter, player) use `vergen` in their build.rs to
# embed VERGEN_GIT_SHA / VERGEN_GIT_COMMIT_DATE at compile time. The chroot
# has no .git so vergen fails. Pre-compute on the host and pass through.
VERGEN_GIT_SHA="$(git -C "$PROJECT_DIR" rev-parse HEAD 2>/dev/null || echo unknown)"
VERGEN_GIT_COMMIT_DATE="$(git -C "$PROJECT_DIR" log -1 --format=%cs HEAD 2>/dev/null || date -u +%Y-%m-%d)"
chroot "$ROOTFS" env \
    VERGEN_GIT_SHA="$VERGEN_GIT_SHA" \
    VERGEN_GIT_COMMIT_DATE="$VERGEN_GIT_COMMIT_DATE" \
    DESKTOP_PACKAGE_ROOT="$DESKTOP_PACKAGE_ROOT_CHROOT" \
    bash -c '
    set -e
    export CARGO_HOME=/root/.cargo
    export PATH="$CARGO_HOME/bin:$PATH"
    cd /build/desktop-src
    just build
    # NB: pass rootdir and prefix as POSITIONAL args. `just install rootdir=""`
    # would set rootdir to the literal string "rootdir=" (the entire token is
    # the value of positional param 1), producing nonsense install paths like
    # `/build/desktop-src/rootdir=/prefix=/usr/bin/cosmic-greeter`. The
    # cosmic-* binaries then never reach /usr/bin and the resulting image has
    # no working desktop. See desktop/justfile recipe `install rootdir="" prefix="/usr/local"`.
    just install "$DESKTOP_PACKAGE_ROOT" /usr

    # ----------------------------------------------------------------------
    # ClawOS Agent UI + bridge — separate workspace (no justfile) under
    # desktop/agent/. com.clawos.Agent.desktop expects /usr/local/bin/cos-agent-ui
    # via `cos app agent open`; the cos-agent-bridge.service user unit
    # invokes /usr/local/bin/cos-agent-bridge. Both binaries must be
    # produced here or the desktop agent app fails to launch with
    #   {"error":"cos-agent-ui is not installed"}.
    # ----------------------------------------------------------------------
    if [ -f /build/desktop-src/agent/Cargo.toml ]; then
        echo "  :: building cos-agent-ui + cos-agent-bridge"
        cd /build/desktop-src/agent
        cargo build --release --workspace
        install -Dm0755 target/release/cos-agent-ui     "$DESKTOP_PACKAGE_ROOT/usr/local/bin/cos-agent-ui"
        install -Dm0755 target/release/cos-agent-bridge "$DESKTOP_PACKAGE_ROOT/usr/local/bin/cos-agent-bridge"
    fi
'

# ---------------------------------------------------------------------------
# 2b. Assert that critical data files landed in the package staging root.
#
# These are silently-required by the desktop and have, in the past, gone
# missing without breaking the build — leading to bugs like "Panel/Dock
# settings page shows only Reset to Default" (missing schema files under
# /usr/share/cosmic/com.clawos.Panel.*/v1/) or "wallpaper page has no preview"
# (missing /usr/share/backgrounds/cosmic/claw-default.jpg).
#
# If any of these are missing here, the package would produce a broken image —
# fail loudly instead of shipping it.
# ---------------------------------------------------------------------------
echo "  :: verifying critical desktop data files"
required_files=(
    # Wallpaper bitmap + cosmic-bg default entry
    "$DESKTOP_PACKAGE_ROOT/usr/share/backgrounds/cosmic/claw-default.jpg"
    "$DESKTOP_PACKAGE_ROOT/usr/share/cosmic/com.clawos.Background/v1/all"
    # Panel + Dock default schemas — the "name" file is the canary;
    # if it's missing the settings page collapses to "Reset to Default".
    "$DESKTOP_PACKAGE_ROOT/usr/share/cosmic/com.clawos.Panel.Panel/v1/name"
    "$DESKTOP_PACKAGE_ROOT/usr/share/cosmic/com.clawos.Panel.Panel/v1/padding_overlap"
    "$DESKTOP_PACKAGE_ROOT/usr/share/cosmic/com.clawos.Panel.Dock/v1/name"
    "$DESKTOP_PACKAGE_ROOT/usr/share/cosmic/com.clawos.Panel.Dock/v1/padding_overlap"
    # Theme + comp defaults
    "$DESKTOP_PACKAGE_ROOT/usr/share/cosmic/com.clawos.Theme.Dark/v1/name"
    "$DESKTOP_PACKAGE_ROOT/usr/share/cosmic/com.clawos.Theme.Light/v1/name"
    "$DESKTOP_PACKAGE_ROOT/usr/share/cosmic/com.clawos.Theme.Mode/v1/is_dark"
    # cosmic-comp appearance — enables window shadows + corner clipping by
    # default. Without this file cosmic-comp falls back to the AppearanceConfig
    # struct Default (now also shadow_tiled_windows=true, but seed wins).
    "$DESKTOP_PACKAGE_ROOT/usr/share/cosmic/com.clawos.Comp/v1/appearance_settings"
    # Icon theme — WhiteSur is installed via just install
    # icons-whitesur-pkg/install in desktop/justfile. If it's missing
    # the toolkit default icon_theme="WhiteSur-dark" falls back to
    # adwaita and the system loses its macOS-ish look.
    "$DESKTOP_PACKAGE_ROOT/usr/share/icons/WhiteSur-dark/index.theme"
)
missing=0
for f in "${required_files[@]}"; do
    if [ ! -e "$f" ]; then
        echo "    missing: ${f#$DESKTOP_PACKAGE_ROOT}"
        missing=1
    fi
done
if [ "$missing" = "1" ]; then
    echo "  error: critical desktop data files are missing — the resulting" >&2
    echo "         image would have broken Settings + Wallpaper pages."     >&2
    echo "         Check desktop/justfile install recipes and confirm"      >&2
    echo "         the source tree under $DESKTOP_SRC is intact."           >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# 3. Stage login-chain wiring into claw-os-desktop.deb.
#
# `just install` puts the binaries / .desktop / sysusers / tmpfiles in
# place, but the upstream Debian packaging (which we are NOT using) is
# responsible for systemd .service files, the greetd config, and the PAM stack.
# We stage those files into the deb and let postinst handle users, locale-gen,
# systemctl enable, sysusers/tmpfiles, and plymouth activation.
# ---------------------------------------------------------------------------
GREETER_DEB="$DESKTOP_SRC/greeter/debian"

echo "  :: installing greeter systemd units, PAM, greetd config"
install -Dm0644 "$GREETER_DEB/cosmic-greeter.service" \
    "$DESKTOP_PACKAGE_ROOT/lib/systemd/system/cosmic-greeter.service"
install -Dm0644 "$GREETER_DEB/cosmic-greeter-daemon.service" \
    "$DESKTOP_PACKAGE_ROOT/lib/systemd/system/cosmic-greeter-daemon.service"
install -Dm0644 "$GREETER_DEB/cosmic-greeter.pam" \
    "$DESKTOP_PACKAGE_ROOT/etc/pam.d/cosmic-greeter"
install -Dm0644 "$DESKTOP_SRC/greeter/cosmic-greeter.toml" \
    "$DESKTOP_PACKAGE_ROOT/etc/greetd/cosmic-greeter.toml"

# ---------------------------------------------------------------------------
# First-boot wizard wiring.
#
# Fresh images have no human user with a usable password, so cosmic-greeter
# has nothing to log into. Pop!_OS solves this with an installer ISO; we
# don't ship one, so we run cosmic-initial-setup as a first-boot wizard
# inside greetd's [initial_session]:
#
#   1. Create system user `cosmic-initial-setup` (uid <1000, hidden from
#      the cosmic-greeter user list by UserFilter).
#   2. Append [initial_session] to /etc/greetd/cosmic-greeter.toml
#      pointing at /usr/lib/cos/firstboot-session — a wrapper that either
#      execs cosmic-session (no human user yet) or exits 0 so greetd
#      falls through to [default_session] (wizard already ran).
#
# Inside the cosmic-session started by the wrapper, the autostart entry
# /etc/xdg/autostart/com.clawos.InitialSetup.desktop auto-launches the
# wizard. When the wizard's Finish handler runs `loginctl terminate-user
# cosmic-initial-setup`, the session dies and greetd advances to
# cosmic-greeter for normal login as the user the wizard just created.
# ---------------------------------------------------------------------------
echo "  :: staging first-boot wizard greetd config"
if ! grep -q '^\[initial_session\]' "$DESKTOP_PACKAGE_ROOT/etc/greetd/cosmic-greeter.toml"; then
    cat >> "$DESKTOP_PACKAGE_ROOT/etc/greetd/cosmic-greeter.toml" <<'EOF'

[initial_session]
command = "/usr/lib/cos/firstboot-session"
user = "cosmic-initial-setup"
EOF
fi

# Create the cosmic-greeter system user + its runtime/state dirs from the
# sysusers.d / tmpfiles.d that `just install` already dropped. Package postinst
# runs systemd-sysusers/systemd-tmpfiles after the files are installed.

# Upstream cosmic-greeter.service has its [Install] section commented out
# (the deb postinst manages display-manager.service symlinking via debconf).
# We are not running dpkg, so wire the systemd targets explicitly.
echo "  :: enabling display-manager + supporting services"
mkdir -p "$DESKTOP_PACKAGE_ROOT/etc/systemd/system/graphical.target.wants"
mkdir -p "$DESKTOP_PACKAGE_ROOT/etc/systemd/system/multi-user.target.wants"

ln -sf /lib/systemd/system/cosmic-greeter.service \
    "$DESKTOP_PACKAGE_ROOT/etc/systemd/system/graphical.target.wants/cosmic-greeter.service"
ln -sf /lib/systemd/system/cosmic-greeter.service \
    "$DESKTOP_PACKAGE_ROOT/etc/systemd/system/display-manager.service"
ln -sf /lib/systemd/system/cosmic-greeter-daemon.service \
    "$DESKTOP_PACKAGE_ROOT/etc/systemd/system/multi-user.target.wants/cosmic-greeter-daemon.service"

# Boot to graphical.target by default.
ln -sf /lib/systemd/system/graphical.target \
    "$DESKTOP_PACKAGE_ROOT/etc/systemd/system/default.target"

# Per-user services (PipeWire, WirePlumber, xdg-desktop-portal). These ship
# with default.target.wants symlinks from their deb packages, but in case
# they ever stop doing so, force-enable them here in /etc/systemd/user/.
mkdir -p "$DESKTOP_PACKAGE_ROOT/etc/systemd/user/sockets.target.wants"
mkdir -p "$DESKTOP_PACKAGE_ROOT/etc/systemd/user/default.target.wants"
for unit in pipewire.socket pipewire-pulse.socket; do
    [ -e "$ROOTFS/usr/lib/systemd/user/$unit" ] && \
        ln -sf "/usr/lib/systemd/user/$unit" \
            "$DESKTOP_PACKAGE_ROOT/etc/systemd/user/sockets.target.wants/$unit"
done
for unit in pipewire.service wireplumber.service; do
    [ -e "$ROOTFS/usr/lib/systemd/user/$unit" ] && \
        ln -sf "/usr/lib/systemd/user/$unit" \
            "$DESKTOP_PACKAGE_ROOT/etc/systemd/user/default.target.wants/$unit"
done

# cos-agent-bridge.service is shipped by rootfs/features/systemd/overlay/
# at /usr/lib/systemd/user/. It declares WantedBy=graphical-session.target
# but `systemctl --user enable` only runs in the user's session, which
# means a fresh user (no prior login) never gets the symlink. Wire it
# globally so the bridge starts as soon as cosmic-session reaches
# graphical-session.target.
mkdir -p "$DESKTOP_PACKAGE_ROOT/etc/systemd/user/graphical-session.target.wants"
[ -e "$ROOTFS/usr/lib/systemd/user/cos-agent-bridge.service" ] && \
    ln -sf "/usr/lib/systemd/user/cos-agent-bridge.service" \
        "$DESKTOP_PACKAGE_ROOT/etc/systemd/user/graphical-session.target.wants/cos-agent-bridge.service"

# ---------------------------------------------------------------------------
# 4. Build and install claw-os-desktop.deb.
# ---------------------------------------------------------------------------
echo "  :: building claw-os-desktop.deb"
"$PROJECT_DIR/packaging/deb/build-desktop-deb.sh" "$DESKTOP_PACKAGE_ROOT"

DEBS_DIR="$PROJECT_DIR/build/debs"
DESKTOP_DEB="$DEBS_DIR/claw-os-desktop_${COS_VERSION}_${DEB_ARCH:-amd64}.deb"
if [ ! -f "$DESKTOP_DEB" ]; then
    echo "  error: expected desktop package missing: $DESKTOP_DEB" >&2
    exit 1
fi
echo "  :: installing $(basename "$DESKTOP_DEB")"

mkdir -p "$ROOTFS/var/cache/cos-debs"
cp "$DESKTOP_DEB" "$ROOTFS/var/cache/cos-debs/"
# The desktop deb ships some conffiles (e.g. /etc/apt/apt.conf.d/20auto-upgrades)
# that already exist on the rootfs from the base overlay. dpkg would normally
# stop at an interactive conffile prompt, but stdin is not a terminal inside the
# chroot, so the prompt hits EOF and aborts. Force the non-interactive default
# (keep the existing file: confdef + confold) and set the noninteractive frontend.
DEBIAN_FRONTEND=noninteractive chroot "$ROOTFS" apt-get install -y \
    --no-install-recommends \
    -o Dpkg::Options::=--force-confdef \
    -o Dpkg::Options::=--force-confold \
    "/var/cache/cos-debs/$(basename "$DESKTOP_DEB")"
chroot "$ROOTFS" apt-get clean
rm -rf "$ROOTFS/var/lib/apt/lists"/*
rm -rf "$DESKTOP_PACKAGE_ROOT"

# ---------------------------------------------------------------------------
# 5. Verify package install side effects.
# ---------------------------------------------------------------------------
echo "  :: verifying claw-os-desktop install"
if ! chroot "$ROOTFS" dpkg-query -W -f='${Status}' claw-os-desktop 2>/dev/null \
    | grep -qx 'install ok installed'; then
    echo "  error: claw-os-desktop package is not installed" >&2
    exit 1
fi

installed_required_files=(
    "$ROOTFS/usr/share/backgrounds/cosmic/claw-default.jpg"
    "$ROOTFS/usr/share/cosmic/com.clawos.Panel.Panel/v1/name"
    "$ROOTFS/usr/share/cosmic/com.clawos.Panel.Dock/v1/name"
    "$ROOTFS/usr/share/cosmic/com.clawos.Theme.Dark/v1/name"
    "$ROOTFS/usr/share/icons/WhiteSur-dark/index.theme"
    "$ROOTFS/etc/greetd/cosmic-greeter.toml"
    "$ROOTFS/lib/systemd/system/cosmic-greeter.service"
)
install_missing=0
for f in "${installed_required_files[@]}"; do
    if [ ! -e "$f" ]; then
        echo "    missing after package install: ${f#$ROOTFS}"
        install_missing=1
    fi
done
if [ "$install_missing" = "1" ]; then
    echo "  error: claw-os-desktop installed with missing desktop files" >&2
    exit 1
fi

locale_missing=0
for want in en_US.utf8 zh_CN.utf8 zh_TW.utf8 ja_JP.utf8; do
    if ! chroot "$ROOTFS" locale -a 2>/dev/null | grep -qx "$want"; then
        echo "    missing locale: $want"
        locale_missing=1
    fi
done
if [ "$locale_missing" = "1" ]; then
    echo "  error: UTF-8 locales failed to generate — cosmic-initial-setup" >&2
    echo "         Language page would be empty. Check claw-os-desktop postinst." >&2
    exit 1
fi

echo "  :: desktop installed via claw-os-desktop.deb; default target = graphical.target"
echo "  :: greeter:  /etc/systemd/system/display-manager.service -> cosmic-greeter.service"
echo "  :: greetd cfg: /etc/greetd/cosmic-greeter.toml"
