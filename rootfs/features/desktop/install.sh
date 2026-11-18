#!/usr/bin/env bash
set -euo pipefail

APP_PACKAGE_INPUTS="$(python3 "$PROJECT_DIR/scripts/app_packages.py" check \
    --variant desktop --architecture "$DEB_ARCH")"

install -d "$ROOTFS/build"
work="$(mktemp -d "$ROOTFS/build/claw-desktop.XXXXXX")"
inside="${work#"$ROOTFS"}"
stage="$work/package"
install -d "$work/source" "$stage"
cleanup() {
    if mountpoint -q "$work/source"; then
        umount "$work/source" || return 1
    fi
    rm -rf -- "$work"
}
trap cleanup EXIT

mount --bind "$PROJECT_DIR" "$work/source"
mount --make-private "$work/source"
mount -o remount,bind,ro "$work/source"
cp -a --no-preserve=ownership "$SCRIPT_DIR/features/desktop/overlay/." "$stage/"

echo "  :: building Claw OS Hyprland and desktop services against Debian $SUITE"
chroot "$ROOTFS" /bin/bash "$inside/source/desktop/hyprland-claw/stage-desktop.sh" \
    "$inside/source" "$inside/package" "$inside"
CLAW_DESKTOP_RUNTIME_DEPS="$(cat "$work/runtime-depends")" \
    "$PROJECT_DIR/packaging/deb/build-desktop-deb.sh" "$stage"

if [[ "${DESKTOP_PACKAGE_ONLY:-0}" == 1 ]]; then
    exit 0
fi

FILE_VERSION="${COS_VERSION//:/%3a}"
DESKTOP_DEB="$PROJECT_DIR/build/debs/claw-os-desktop_${FILE_VERSION}_${DEB_ARCH}.deb"
install -m0644 "$DESKTOP_DEB" "$work/desktop.deb"
install -d "$work/apps"
cp -- "$APP_PACKAGE_INPUTS"/archives/*.deb "$work/apps/"
app_debs=()
for app_deb in "$work"/apps/*.deb; do
    app_debs+=("${app_deb#"$ROOTFS"}")
done
chroot "$ROOTFS" env DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    "$inside/desktop.deb" "${app_debs[@]}"
for package in claw-os-desktop claw-app-desktop-shell; do
    test "$(chroot "$ROOTFS" dpkg-query -W -f='${db:Status-Status}' "$package")" = installed
done
test -x "$ROOTFS/usr/libexec/claw-hyprland/Hyprland"
test -f "$ROOTFS/usr/lib/cos/apps/desktop-shell/app.json"
test -f "$ROOTFS/usr/share/wayland-sessions/claw.desktop"
test "$(readlink "$ROOTFS/etc/systemd/system/display-manager.service")" = /usr/lib/systemd/system/greetd.service
echo "  :: installed the authenticated Hyprland desktop and signed Desktop Shell"
