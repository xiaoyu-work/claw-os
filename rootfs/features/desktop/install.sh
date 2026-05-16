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

# ---------------------------------------------------------------------------
# 0. Apply static overlay (drop-in files, default configs) — always runs.
# ---------------------------------------------------------------------------
if [ -d "$FEATURE_DIR/overlay" ] && [ -n "$(ls -A "$FEATURE_DIR/overlay" 2>/dev/null)" ]; then
    echo "  :: applying desktop overlay"
    cp -a "$FEATURE_DIR/overlay/." "$ROOTFS/"
fi

# ---------------------------------------------------------------------------
# 1. Validate source tree (unless skipped).
# ---------------------------------------------------------------------------
if [ "${DESKTOP_SKIP:-0}" = "1" ]; then
    echo "  :: DESKTOP_SKIP=1 — runtime deps + overlay applied, build skipped"
    echo "  :: NOTE: login chain not wired (greeter binary missing). Re-run"
    echo "         without DESKTOP_SKIP to get a bootable graphical session."
    exit 0
fi

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
    just install "" /usr

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
        install -Dm0755 target/release/cos-agent-ui     /usr/local/bin/cos-agent-ui
        install -Dm0755 target/release/cos-agent-bridge /usr/local/bin/cos-agent-bridge
    fi
'

# ---------------------------------------------------------------------------
# 2b. Assert that critical data files actually landed in the rootfs.
#
# These are silently-required by the desktop and have, in the past, gone
# missing without breaking the build — leading to bugs like "Panel/Dock
# settings page shows only Reset to Default" (missing schema files under
# /usr/share/cosmic/com.clawos.Panel.*/v1/) or "wallpaper page has no preview"
# (missing /usr/share/backgrounds/cosmic/claw-default.jpg).
#
# If any of these are missing here, the rootfs is broken — fail the build
# loudly instead of shipping a broken image.
# ---------------------------------------------------------------------------
echo "  :: verifying critical desktop data files"
required_files=(
    # Wallpaper bitmap + cosmic-bg default entry
    "$ROOTFS/usr/share/backgrounds/cosmic/claw-default.jpg"
    "$ROOTFS/usr/share/cosmic/com.clawos.Background/v1/all"
    # Panel + Dock default schemas — the "name" file is the canary;
    # if it's missing the settings page collapses to "Reset to Default".
    "$ROOTFS/usr/share/cosmic/com.clawos.Panel.Panel/v1/name"
    "$ROOTFS/usr/share/cosmic/com.clawos.Panel.Panel/v1/padding_overlap"
    "$ROOTFS/usr/share/cosmic/com.clawos.Panel.Dock/v1/name"
    "$ROOTFS/usr/share/cosmic/com.clawos.Panel.Dock/v1/padding_overlap"
    # Theme + comp defaults
    "$ROOTFS/usr/share/cosmic/com.clawos.Theme.Dark/v1/name"
    "$ROOTFS/usr/share/cosmic/com.clawos.Theme.Light/v1/name"
    "$ROOTFS/usr/share/cosmic/com.clawos.Theme.Mode/v1/is_dark"
)
missing=0
for f in "${required_files[@]}"; do
    if [ ! -e "$f" ]; then
        echo "    missing: ${f#$ROOTFS}"
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
# 3. Wire up the login chain.
#
# `just install` puts the binaries / .desktop / sysusers / tmpfiles in
# place, but the upstream Debian packaging (which we are NOT using) is
# responsible for systemd .service files, the greetd config, and the PAM
# stack. We install them by hand here.
# ---------------------------------------------------------------------------
GREETER_DEB="$DESKTOP_SRC/greeter/debian"

echo "  :: installing greeter systemd units, PAM, greetd config"
install -Dm0644 "$GREETER_DEB/cosmic-greeter.service" \
    "$ROOTFS/lib/systemd/system/cosmic-greeter.service"
install -Dm0644 "$GREETER_DEB/cosmic-greeter-daemon.service" \
    "$ROOTFS/lib/systemd/system/cosmic-greeter-daemon.service"
install -Dm0644 "$GREETER_DEB/cosmic-greeter.pam" \
    "$ROOTFS/etc/pam.d/cosmic-greeter"
install -Dm0644 "$DESKTOP_SRC/greeter/cosmic-greeter.toml" \
    "$ROOTFS/etc/greetd/cosmic-greeter.toml"

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
echo "  :: creating cosmic-initial-setup system user (first-boot wizard)"
source "$PROJECT_DIR/scripts/lib/add-cos-user.sh"
add_cosmic_initial_setup_user "$ROOTFS"

if ! grep -q '^\[initial_session\]' "$ROOTFS/etc/greetd/cosmic-greeter.toml"; then
    echo "  :: appending [initial_session] block to cosmic-greeter.toml"
    cat >> "$ROOTFS/etc/greetd/cosmic-greeter.toml" <<'EOF'

[initial_session]
command = "/usr/lib/cos/firstboot-session"
user = "cosmic-initial-setup"
EOF
fi

# Create the cosmic-greeter system user + its runtime/state dirs from the
# sysusers.d / tmpfiles.d that `just install` already dropped.
echo "  :: applying systemd-sysusers / systemd-tmpfiles"
chroot "$ROOTFS" systemd-sysusers
chroot "$ROOTFS" systemd-tmpfiles --create

# Upstream cosmic-greeter.service has its [Install] section commented out
# (the deb postinst manages display-manager.service symlinking via debconf).
# We are not running dpkg, so wire the systemd targets explicitly.
echo "  :: enabling display-manager + supporting services"
mkdir -p "$ROOTFS/etc/systemd/system/graphical.target.wants"
mkdir -p "$ROOTFS/etc/systemd/system/multi-user.target.wants"

ln -sf /lib/systemd/system/cosmic-greeter.service \
    "$ROOTFS/etc/systemd/system/graphical.target.wants/cosmic-greeter.service"
ln -sf /lib/systemd/system/cosmic-greeter.service \
    "$ROOTFS/etc/systemd/system/display-manager.service"
ln -sf /lib/systemd/system/cosmic-greeter-daemon.service \
    "$ROOTFS/etc/systemd/system/multi-user.target.wants/cosmic-greeter-daemon.service"

# Boot to graphical.target by default.
ln -sf /lib/systemd/system/graphical.target \
    "$ROOTFS/etc/systemd/system/default.target"

# System services the desktop expects.
chroot "$ROOTFS" bash -c '
    set -e
    systemctl enable NetworkManager.service
    systemctl enable bluetooth.service        || true
    systemctl enable polkit.service           || true
    systemctl enable power-profiles-daemon.service || true
    systemctl enable upower.service           || true
    systemctl enable accounts-daemon.service  || true
    # VM integration — no-op on bare metal.
    systemctl enable qemu-guest-agent.service || true
    systemctl enable spice-vdagentd.service   || true
'

# Per-user services (PipeWire, WirePlumber, xdg-desktop-portal). These ship
# with default.target.wants symlinks from their deb packages, but in case
# they ever stop doing so, force-enable them here in /etc/systemd/user/.
mkdir -p "$ROOTFS/etc/systemd/user/sockets.target.wants"
mkdir -p "$ROOTFS/etc/systemd/user/default.target.wants"
for unit in pipewire.socket pipewire-pulse.socket; do
    [ -e "$ROOTFS/usr/lib/systemd/user/$unit" ] && \
        ln -sf "/usr/lib/systemd/user/$unit" \
            "$ROOTFS/etc/systemd/user/sockets.target.wants/$unit"
done
for unit in pipewire.service wireplumber.service; do
    [ -e "$ROOTFS/usr/lib/systemd/user/$unit" ] && \
        ln -sf "/usr/lib/systemd/user/$unit" \
            "$ROOTFS/etc/systemd/user/default.target.wants/$unit"
done

# cos-agent-bridge.service is shipped by rootfs/features/systemd/overlay/
# at /usr/lib/systemd/user/. It declares WantedBy=graphical-session.target
# but `systemctl --user enable` only runs in the user's session, which
# means a fresh user (no prior login) never gets the symlink. Wire it
# globally so the bridge starts as soon as cosmic-session reaches
# graphical-session.target.
mkdir -p "$ROOTFS/etc/systemd/user/graphical-session.target.wants"
[ -e "$ROOTFS/usr/lib/systemd/user/cos-agent-bridge.service" ] && \
    ln -sf "/usr/lib/systemd/user/cos-agent-bridge.service" \
        "$ROOTFS/etc/systemd/user/graphical-session.target.wants/cos-agent-bridge.service"

# Plymouth boot splash — the overlay shipped the "claw" theme files
# (claw.plymouth, claw.script, watermark.png, dot.png). Activate it as the
# default; initramfs is rebuilt lazily on first boot or by update-initramfs.
echo "  :: setting plymouth default theme = claw"
chroot "$ROOTFS" plymouth-set-default-theme claw || true

echo "  :: desktop installed; default target = graphical.target"
echo "  :: greeter:  /etc/systemd/system/display-manager.service -> cosmic-greeter.service"
echo "  :: greetd cfg: /etc/greetd/cosmic-greeter.toml"
