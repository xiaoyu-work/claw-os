#!/usr/bin/env bash
# rootfs/features/systemd/install.sh — install claw-os-systemd.deb into ROOTFS.
#
# The .deb's postinst calls `deb-systemd-helper enable` for the boot-required
# Claw OS units, including clawd.service. That creates the
# multi-user.target.wants symlinks without needing a running systemd, so it
# works inside a chroot.
#
# Inherited from environment: ROOTFS, PROJECT_DIR, SCRIPT_DIR.

set -euo pipefail

DEBS_DIR="$PROJECT_DIR/build/debs"

if ! ls "$DEBS_DIR/claw-os-systemd_"*.deb >/dev/null 2>&1; then
    echo "  :: claw-os-systemd.deb not found — building it"
    "$PROJECT_DIR/packaging/deb/build-debs.sh"
fi

SYSTEMD_DEB="$(ls "$DEBS_DIR/claw-os-systemd_"*"_all.deb" | head -1)"
echo "  :: installing $(basename "$SYSTEMD_DEB")"

mkdir -p "$ROOTFS/var/cache/cos-debs"
cp "$SYSTEMD_DEB" "$ROOTFS/var/cache/cos-debs/"
chroot "$ROOTFS" apt-get install -y --no-install-recommends \
    "/var/cache/cos-debs/$(basename "$SYSTEMD_DEB")"
chroot "$ROOTFS" apt-get clean
rm -rf "$ROOTFS/var/lib/apt/lists"/*

# Verify the wants symlink (postinst should have created it).
if [ -L "$ROOTFS/etc/systemd/system/multi-user.target.wants/cos-home-setup.service" ]; then
    echo "  :: cos-home-setup.service enabled"
else
    echo "  :: WARNING — deb-systemd-helper did not enable cos-home-setup.service" >&2
    echo "     falling back to direct symlink"
    mkdir -p "$ROOTFS/etc/systemd/system/multi-user.target.wants"
    ln -sf /usr/lib/systemd/system/cos-home-setup.service \
        "$ROOTFS/etc/systemd/system/multi-user.target.wants/cos-home-setup.service"
fi
echo "  :: cos-browser.service installed (not enabled by default)"

if [ -L "$ROOTFS/etc/systemd/system/multi-user.target.wants/clawd.service" ]; then
    echo "  :: clawd.service enabled as system daemon"
else
    echo "  :: WARNING — clawd.service was not enabled as a system daemon" >&2
    echo "     falling back to direct symlink"
    mkdir -p "$ROOTFS/etc/systemd/system/multi-user.target.wants"
    ln -sf /usr/lib/systemd/system/clawd.service \
        "$ROOTFS/etc/systemd/system/multi-user.target.wants/clawd.service"
fi

# Enable systemd-timesyncd so the wall clock comes up correct on every
# boot (matters for HTTPS, sudo timestamps, mail dates, sshd…). The
# unit ships disabled by the package; symlink it under
# sysinit.target.wants so it starts very early in boot, before
# multi-user.target.
echo "  :: enabling systemd-timesyncd.service"
mkdir -p "$ROOTFS/etc/systemd/system/sysinit.target.wants"
ln -sf /lib/systemd/system/systemd-timesyncd.service \
    "$ROOTFS/etc/systemd/system/sysinit.target.wants/systemd-timesyncd.service"

# Enable ydotoold (the userspace daemon that fronts /dev/uinput for the
# ydotool CLI). The AI agent uses ydotool as the universal GUI input
# driver — without ydotoold running, every keystroke / click attempt
# fails with "couldn't connect to socket". Only enable if the unit
# actually shipped (the ydotool package on trixie ships the unit but
# guard defensively in case the package is missing on arm64 or in a
# stripped variant).
YDOTOOLD_UNIT="$ROOTFS/lib/systemd/system/ydotoold.service"
if [ -f "$YDOTOOLD_UNIT" ]; then
    echo "  :: enabling ydotoold.service"
    ln -sf /lib/systemd/system/ydotoold.service \
        "$ROOTFS/etc/systemd/system/multi-user.target.wants/ydotoold.service"
else
    echo "  :: ydotoold.service unit not present (skipping)"
fi

# Enable fwupd-refresh.timer — weekly LVFS metadata refresh so the
# user sees firmware-update notifications without manual
# `fwupdmgr refresh`. The package ships the timer disabled; we
# opt in. Firmware *installation* still happens only on user
# action, this just keeps the metadata fresh.
FWUPD_REFRESH_TIMER="$ROOTFS/lib/systemd/system/fwupd-refresh.timer"
if [ -f "$FWUPD_REFRESH_TIMER" ]; then
    echo "  :: enabling fwupd-refresh.timer"
    mkdir -p "$ROOTFS/etc/systemd/system/timers.target.wants"
    ln -sf /lib/systemd/system/fwupd-refresh.timer \
        "$ROOTFS/etc/systemd/system/timers.target.wants/fwupd-refresh.timer"
fi

# Enable apt-daily-upgrade.timer (already enabled by default in
# Debian; symlink defensively in case a stripped /etc/systemd/system
# overlay clobbered it). The actual switch that *does* anything is
# /etc/apt/apt.conf.d/20auto-upgrades shipped by the desktop overlay.
APT_DAILY_UPGRADE_TIMER="$ROOTFS/lib/systemd/system/apt-daily-upgrade.timer"
if [ -f "$APT_DAILY_UPGRADE_TIMER" ]; then
    mkdir -p "$ROOTFS/etc/systemd/system/timers.target.wants"
    ln -sf /lib/systemd/system/apt-daily-upgrade.timer \
        "$ROOTFS/etc/systemd/system/timers.target.wants/apt-daily-upgrade.timer"
fi

# Wire fprintd into the PAM auth stack via the standard pam-auth-update
# mechanism. The libpam-fprintd package ships /usr/share/pam-configs/
# fprintd; pam-auth-update merges it into /etc/pam.d/common-auth. On
# machines without a fingerprint reader pam_fprintd returns PAM_IGNORE
# so password auth still works, this is safe to enable by default.
if [ -f "$ROOTFS/usr/share/pam-configs/fprintd" ]; then
    echo "  :: pam-auth-update --enable fprintd"
    chroot "$ROOTFS" env DEBIAN_FRONTEND=noninteractive \
        pam-auth-update --enable fprintd >/dev/null 2>&1 \
        || echo "    (pam-auth-update failed; not fatal — login still uses password)"
fi
