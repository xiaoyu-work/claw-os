#!/bin/sh
set -eu
umask 077

fail() {
    printf 'Claw OS: %s\n' "$*" >&2
    exit 1
}

if [ "$#" -ne 3 ]; then
    fail "usage: sudo /bin/sh $0 --plan|--install PAYLOAD.zip SHA256"
fi
mode=$1
case "$mode" in
    --plan|--install) ;;
    *) fail "choose --plan or --install" ;;
esac
if [ "$(/usr/bin/uname -s)" != Darwin ] || [ "$(/usr/bin/uname -m)" != arm64 ]; then
    fail "run from a native Apple Silicon macOS terminal, not Rosetta or Linux"
fi
[ "$(/usr/bin/id -u)" -eq 0 ] && [ -t 0 ] || fail "use sudo from an interactive macOS terminal"

payload=$2
case "$payload" in
    /*) ;;
    *) payload="$PWD/$payload" ;;
esac
[ -f "$payload" ] || fail "Linux payload is not a regular file"
digest=$3
case "$digest" in
    *[!0-9a-f]*) fail "use the SHA-256 from the trusted Claw OS build" ;;
esac
[ "${#digest}" -eq 64 ] || fail "use the SHA-256 from the trusted Claw OS build"
scripts=$(cd "$(/usr/bin/dirname "$0")" && pwd)

work=$(/usr/bin/mktemp -d /private/var/tmp/claw-asahi.XXXXXX)
restore_mount="$work/restore-app"
attached=no
cleanup() {
    if [ "$attached" = yes ]; then
        /usr/bin/hdiutil detach "$restore_mount" >/dev/null || return 1
    fi
    /bin/rm -rfx -- "$work"
}
trap cleanup EXIT

verify() {
    printf '%s  %s\n' "$1" "$2" | /usr/bin/env -i PATH=/usr/bin:/bin /usr/bin/shasum -a 256 -c - >/dev/null
}

fetch() {
    /usr/bin/curl -q -fsSL --proto '=https' --proto-redir '=https' --output "$3" "$1"
    verify "$2" "$3" || fail "checksum does not match: $1"
}

/usr/bin/install -d "$work/os" "$work/runtime"
/bin/cp "$payload" "$work/os/claw-os.zip"
verify "$digest" "$work/os/claw-os.zip" || fail "Linux payload checksum does not match"

# AsahiLinux/asahi-installer v0.9.2, signed source dffbb38ef0c00c0431c609ecd8a00f42deb5b24c.
fetch https://cdn.asahilinux.org/installer/installer-v0.9.2.tar.gz \
    484d999e53b4aef27d17c4478e49567e96842f8a6cb35c073a0df5bf0bbe8276 "$work/engine.tar.gz"
/usr/bin/tar -oxzf "$work/engine.tar.gz" -C "$work/runtime"
/bin/mv "$work/runtime/main.py" "$work/runtime/asahi_engine.py"
/usr/bin/install -m 0600 "$scripts/install-macos.py" "$work/runtime/main.py"

decoder=
requirement=
if [ "$(/usr/sbin/sysctl -n hw.model)" = Mac17,5 ]; then
    base=https://github.com/omacom/omarchy-mac-installer/releases/download
    version=v0.9.2-omarchy.20261106.0f73bea79639
    fetch "$base/engine-$version/installer-$version.tar.gz" \
        a60855716be5cea4f2113a368f784a5fe55a7034602f48fac8bb6e79f796fd7a "$work/neo.tar.gz"
    /usr/bin/install -d "$work/neo" "$restore_mount"
    /usr/bin/tar -oxzf "$work/neo.tar.gz" -C "$work/neo"
    /usr/bin/install -m 0600 "$work/neo"/omarchy_*.py "$work/runtime/"
    /usr/bin/install -m 0600 "$work/neo/main.py" "$work/runtime/neo_engine.py"
    /usr/bin/install -m 0600 "$scripts/install-macos-neo.py" "$work/runtime/claw_neo.py"

    fetch "$base/v2.3.16/Omarchy-Mac-Installer-2.3.16.dmg" \
        0b70717b0bfa3277f41159b5de4a0cab9f75dda9ba64f13d4b4a02a7860dd027 "$work/restore.dmg"
    /usr/bin/hdiutil attach -quiet -readonly -nobrowse -mountpoint "$restore_mount" "$work/restore.dmg"
    attached=yes
    decoder="$work/restore-image"
    /usr/bin/install -m 0700 "$restore_mount/Omarchy Installer.app/Contents/Helpers/omarchy-restore-image" "$decoder"
    /usr/bin/hdiutil detach "$restore_mount" >/dev/null
    attached=no
    verify d023dd527f2d5837d0954ed691da904fbdd17b2c4d54914b9efd8e7cd16dc8b2 "$decoder" \
        || fail "signed restore helper checksum does not match"
    requirement='anchor apple generic and identifier "org.omarchy.installer.restore-image" and certificate 1[field.1.2.840.113635.100.6.2.6] and certificate leaf[field.1.2.840.113635.100.6.1.13] and certificate leaf[subject.OU] = "T2C384FJBD"'
fi

cd "$work/runtime"

# Do not inherit Asahi expert, alternate restore-server, or telemetry settings.
/usr/bin/env -i HOME="$work" PATH=/usr/bin:/bin:/usr/sbin:/sbin \
    DISTRO='Claw OS' DISTRO_DOCS='https://github.com/xiaoyu-work/claw-os/blob/main/docs/image-architecture.md' \
    OMARCHY_RESTORE_IMAGE="$decoder" OMARCHY_RESTORE_IMAGE_REQUIREMENT="$requirement" \
    REPO_BASE="$work" /usr/bin/caffeinate -dis /bin/sh ./install.sh "$mode"
