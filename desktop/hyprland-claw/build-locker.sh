#!/usr/bin/env bash
set -euo pipefail

stage="${1:?desktop stage directory required}"
work="${2:?native build directory required}"
jobs="${CMAKE_BUILD_PARALLEL_LEVEL:-4}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
version=1.8.6
digest=e7e4561aa04a2e94e04d301accac5b923322219bbbfc833de69be34cbd31ee77
archive="$work/swaylock_${version}.orig.tar.xz"
source="$work/swaylock-source"
build="$work/swaylock-build"

if [[ ! -e "$archive" ]]; then
    curl --proto '=https' --proto-redir '=https' -fLsS --output "$archive" \
        "https://deb.debian.org/debian/pool/main/s/swaylock/swaylock_${version}.orig.tar.xz"
fi
[[ -f "$archive" && ! -L "$archive" ]]
printf '%s  %s\n' "$digest" "$archive" | sha256sum --check

mkdir "$source"
(umask 022; tar -xf "$archive" --no-same-owner --no-same-permissions \
    --strip-components=1 -C "$source")
git -C "$source" apply --unidiff-zero --whitespace=error-all \
    "$here/swaylock-pam-only.patch" "$here/swaylock-unlock-prompt.patch"
meson setup --wrap-mode=nodownload --buildtype=release \
    -Dpam=enabled -Dgdk-pixbuf=disabled -Dman-pages=disabled "$build" "$source"
meson compile -C "$build" -j "$jobs" swaylock

install -Dm0755 "$build/swaylock" "$stage/usr/libexec/claw-hyprland/claw-lockscreen"
install -Dm0644 "$here/session/claw-desktop-lock.pam" "$stage/etc/pam.d/claw-desktop-lock"
install -Dm0644 "$source/LICENSE" "$stage/usr/share/doc/claw-os-desktop/swaylock.LICENSE"
