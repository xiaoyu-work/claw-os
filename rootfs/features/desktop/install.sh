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
# ---------------------------------------------------------------------------
CHROOT_SRC="$ROOTFS/build/desktop-src"
CHROOT_CRATES="$ROOTFS/build/crates"
PROJECT_CRATES="$PROJECT_DIR/crates"
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

cleanup() {
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
chroot "$ROOTFS" bash -c '
    set -e
    export CARGO_HOME=/root/.cargo
    export PATH="$CARGO_HOME/bin:$PATH"
    cd /build/desktop-src
    just build
    just install rootdir="" prefix=/usr
'

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

# Plymouth boot splash — the overlay shipped the "claw" theme files
# (claw.plymouth, claw.script, watermark.png, dot.png). Activate it as the
# default; initramfs is rebuilt lazily on first boot or by update-initramfs.
echo "  :: setting plymouth default theme = claw"
chroot "$ROOTFS" plymouth-set-default-theme claw || true

echo "  :: desktop installed; default target = graphical.target"
echo "  :: greeter:  /etc/systemd/system/display-manager.service -> cosmic-greeter.service"
echo "  :: greetd cfg: /etc/greetd/cosmic-greeter.toml"
