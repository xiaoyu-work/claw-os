#!/bin/sh
set -eu

restore_or_remove() {
    target="$1"
    backup="${target}.claw-installed"
    if [ -e "$backup" ]; then
        mv -f "$backup" "$target"
    else
        rm -f "$target"
    fi
}

rm -f /etc/systemd/system/getty@tty1.service.d/autologin.conf
rmdir /etc/systemd/system/getty@tty1.service.d 2>/dev/null || true

restore_or_remove /etc/issue
restore_or_remove /etc/motd

# The users/networkcfg modules have already written the hostname selected
# in Calamares. Preserve it even when the user intentionally chose the same
# text as the live hostname.
rm -f /etc/hostname.claw-installed

if [ -x /usr/libexec/claw-oobe ]; then
    install -Dm0644 /dev/null /etc/cos/first-run
    install -d /etc/systemd/system/multi-user.target.wants
    ln -sfn /usr/lib/systemd/system/claw-driver-online.service \
        /etc/systemd/system/multi-user.target.wants/claw-driver-online.service
fi

rm -f \
    /etc/profile.d/cos-installer.sh \
    /etc/cos/installer-xstartup \
    /etc/sudoers.d/cos-installer \
    /etc/systemd/system/apt-daily.timer.d/skip-on-live.conf \
    /etc/systemd/system/apt-daily-upgrade.timer.d/skip-on-live.conf
rmdir /etc/systemd/system/apt-daily.timer.d 2>/dev/null || true
rmdir /etc/systemd/system/apt-daily-upgrade.timer.d 2>/dev/null || true

rm -rf /etc/calamares
rm -f \
    /usr/share/applications/calamares.desktop \
    /usr/share/applications/io.calamares.calamares.desktop

rm -f /usr/lib/cos/init/cleanup-installer-target.sh
