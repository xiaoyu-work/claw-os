#!/usr/bin/env bash
# rootfs/features/live/install.sh — Live media tweaks.
#
# What we DO:
#  - Enable NetworkManager so the live system gets a network on boot.
#  - Install openssh-server (via packages.txt) but DO NOT enable it.
#    Live-config creates a default user with a known password ("user"/"live"
#    by default), so auto-starting ssh on a public network would expose a
#    known-credential login. Users opt in with `systemctl enable --now ssh`.
#  - Configure tty1 autologin for the live-config "user" account so headless
#    boots (qemu -nographic, USB keyboard) reach a shell without a password
#    prompt.
#  - Set the hostname to `claw-os-live` so terminals / mDNS announce as
#    something recognisable.
#  - Install Claw OS branding in /etc/issue (pre-login) and /etc/motd
#    (post-login).
#  - Suppress `apt-daily.timer` / `apt-daily-upgrade.timer` while booted
#    from the live medium — they have nothing useful to do on a
#    read-only squashfs and waste bandwidth. Drops are conditional on
#    `/run/live/medium` existing, so the timers re-enable themselves
#    after Calamares installs to disk.
#  - If a graphical session is also installed (the `desktop` feature
#    ran first), drop an `[initial_session]` block into greetd's config
#    so the live ISO boots straight into the live user's Wayland
#    session — no password prompt on first boot.
#
# What we DON'T do:
#  - Create the live user (live-config does this at boot via the
#    `boot=live components` kernel cmdline).
#  - Touch /etc/sudoers (live-config configures passwordless sudo for the
#    live user only).
#  - Pack the rootfs into a bootable ISO — that's the iso-build step,
#    which is deliberately deferred.
#
# Inherited from environment: ROOTFS.

set -euo pipefail

# -----------------------------------------------------------------------------
# Network + ssh.
# -----------------------------------------------------------------------------

# Enable NetworkManager.
mkdir -p "$ROOTFS/etc/systemd/system/multi-user.target.wants"
ln -sf /usr/lib/systemd/system/NetworkManager.service \
    "$ROOTFS/etc/systemd/system/multi-user.target.wants/NetworkManager.service"

# -----------------------------------------------------------------------------
# Console autologin (text-mode boot or graphical session fallback).
# -----------------------------------------------------------------------------

# Auto-login on tty1. live-config defaults the live user to username "user".
mkdir -p "$ROOTFS/etc/systemd/system/getty@tty1.service.d"
cat > "$ROOTFS/etc/systemd/system/getty@tty1.service.d/autologin.conf" <<'EOF'
[Service]
ExecStart=
ExecStart=-/sbin/agetty --autologin user --noclear %I $TERM
EOF

# -----------------------------------------------------------------------------
# Hostname + login banners.
# -----------------------------------------------------------------------------

for path in etc/hostname etc/issue etc/motd; do
    if [ -e "$ROOTFS/$path" ]; then
        cp -a "$ROOTFS/$path" "$ROOTFS/$path.claw-installed"
    fi
done

echo "claw-os-live" > "$ROOTFS/etc/hostname"

cat > "$ROOTFS/etc/issue" <<'EOF'

  Welcome to Claw OS Live — \n \l

  Default credentials: user / live
  Run `sudo calamares` (or click "Install Claw OS") to install.

EOF

cat > "$ROOTFS/etc/motd" <<'EOF'

  Claw OS Live — running from memory.
  Changes are not preserved. Run `sudo calamares` (or click the
  "Install Claw OS" icon on the desktop) to install permanently.

EOF

# -----------------------------------------------------------------------------
# Skip background apt while booted from live medium.
# -----------------------------------------------------------------------------
#
# `live-boot` mounts the live medium at /run/live/medium for the duration
# of the live session. We gate the apt timers on its NON-existence so
# they stay disabled on the live ISO and re-enable themselves once
# Calamares installs to disk and /run/live/medium is gone.

for timer in apt-daily.timer apt-daily-upgrade.timer; do
    dropin="$ROOTFS/etc/systemd/system/${timer}.d"
    mkdir -p "$dropin"
    cat > "$dropin/skip-on-live.conf" <<'EOF'
[Unit]
ConditionPathExists=!/run/live/medium
EOF
done

# -----------------------------------------------------------------------------
# Optional: greetd autologin if the desktop feature ran first.
# -----------------------------------------------------------------------------
#
# We DON'T require `desktop` to have run — live is useful as a pure
# text-mode rescue medium too. But when greetd's cosmic-greeter.toml
# is present, we layer an `[initial_session]` over it so the live
# user goes straight into a Wayland session at first boot. After
# logout, greetd falls back to `[default_session]` (the regular
# greeter), which is what a "rescue + recovery shell" user expects.

if [ -f "$ROOTFS/etc/greetd/cosmic-greeter.toml" ]; then
    echo "  :: layering greetd initial_session for live user autologin"
    cp -a "$ROOTFS/etc/greetd/cosmic-greeter.toml" \
        "$ROOTFS/etc/greetd/cosmic-greeter.toml.claw-installed"
    cat > "$ROOTFS/etc/greetd/cosmic-greeter.toml" <<'EOF'
[terminal]
vt = "1"

[general]
service = "cosmic-greeter"

[default_session]
command = "cosmic-greeter-start"
user = "cosmic-greeter"

# Live-only: skip the password prompt on first boot. live-config
# creates the "user" account at boot before greetd starts. After
# the live user logs out, default_session takes over.
[initial_session]
command = "cosmic-session"
user = "user"
EOF
fi

echo "  :: NetworkManager enabled, tty1 autologin configured"
echo "  :: hostname=claw-os-live, /etc/issue + /etc/motd installed"
echo "  :: apt-daily / apt-daily-upgrade skipped while on live medium"
echo "  :: ssh installed but NOT enabled (run 'systemctl enable --now ssh' to expose it)"
