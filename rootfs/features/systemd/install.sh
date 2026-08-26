#!/usr/bin/env bash
# rootfs/features/systemd/install.sh -- verify and finish wiring the units
# owned by claw-os-agent and claw-os-base.
#
# Inherited from environment: ROOTFS, PROJECT_DIR, SCRIPT_DIR.

set -euo pipefail

for unit in clawd.service cos-browser.service cos-home-setup.service; do
    if [ ! -f "$ROOTFS/usr/lib/systemd/system/$unit" ]; then
        echo "error: expected packaged system unit missing: $unit" >&2
        exit 1
    fi
done

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

# User indexers are global default.target services, so they also run for
# headless/SSH user managers. Their ConditionPathIsExecutable directives
# make missing optional binaries a clean skip.
mkdir -p "$ROOTFS/etc/systemd/user/default.target.wants"
for unit in claw-recoll-index.service claw-semantic.service; do
    if [ -f "$ROOTFS/usr/lib/systemd/user/$unit" ]; then
        ln -sf "/usr/lib/systemd/user/$unit" \
            "$ROOTFS/etc/systemd/user/default.target.wants/$unit"
    else
        echo "error: expected user unit missing: $unit" >&2
        exit 1
    fi
done

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
