#!/bin/sh
set -eu
umask 077
export LC_ALL=C

fail() {
    printf 'Claw OS: %s\n' "$*" >&2
    exit 1
}

case "$#" in
    3|6) ;;
    *) fail "usage: sudo /bin/sh $0 --plan|--install PAYLOAD.zip SHA256 [RESTORE_HELPER RESTORE_SHA256 APPLE_TEAM_ID]" ;;
esac
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
scripts=$(cd "$(/usr/bin/dirname "$0")" && pwd)
. "$scripts/macos-restore-source.sh"
valid_sha256 "$3" || fail "use the SHA-256 from the trusted Claw OS build"

model=$(/usr/sbin/sysctl -n hw.model)
decoder=
requirement=
if [ "$model" = Mac17,5 ]; then
    [ "$#" -eq 6 ] || fail "MacBook Neo requires the signed Claw OS restore helper, its SHA-256 and Apple Team ID"
    helper=$4
    case "$helper" in
        /*) ;;
        *) helper="$PWD/$helper" ;;
    esac
    [ -f "$helper" ] || fail "restore helper is not a regular file"
    valid_sha256 "$5" || fail "use the restore helper SHA-256 from the trusted Claw OS build"
    requirement=$(restore_requirement "$6") || fail "use the Apple Team ID from the trusted Claw OS build"
else
    [ "$#" -eq 3 ] || fail "the restore helper is only needed on MacBook Neo"
fi

work=$(/usr/bin/mktemp -d /private/var/tmp/claw-asahi.XXXXXX)
trap '/bin/rm -rfx -- "$work"' EXIT
if [ "$model" = Mac17,5 ]; then
    decoder="$work/restore-image"
    /usr/bin/install -m 0700 "$helper" "$decoder"
    verify_sha256 "$5" "$decoder" || fail "signed restore helper checksum does not match"
    /usr/bin/codesign --verify --strict "-R=$requirement" "$decoder" || fail "restore helper does not match the Claw OS signing identity"
fi
/usr/bin/install -d "$work/os" "$work/runtime"
/bin/cp "$payload" "$work/os/claw-os.zip"
verify_sha256 "$3" "$work/os/claw-os.zip" || fail "Linux payload checksum does not match"

# AsahiLinux/asahi-installer v0.9.2, signed source dffbb38ef0c00c0431c609ecd8a00f42deb5b24c.
fetch_pinned https://cdn.asahilinux.org/installer/installer-v0.9.2.tar.gz \
    484d999e53b4aef27d17c4478e49567e96842f8a6cb35c073a0df5bf0bbe8276 "$work/engine.tar.gz" \
    || fail "could not verify the pinned Asahi installer"
/usr/bin/tar -oxzf "$work/engine.tar.gz" -C "$work/runtime"
/bin/mv "$work/runtime/main.py" "$work/runtime/asahi_engine.py"
/usr/bin/install -m 0600 "$scripts/install-macos.py" "$work/runtime/main.py"

if [ "$model" = Mac17,5 ]; then
    fetch_pinned "$NEO_SOURCE_URL" "$NEO_SOURCE_SHA256" "$work/neo-source.tar.gz" \
        || fail "could not verify the pinned Neo installer overlay source"
    overlay="$NEO_SOURCE_ROOT/Engine/overlay/src"
    /usr/bin/install -d "$work/neo"
    /usr/bin/tar -oxzf "$work/neo-source.tar.gz" -C "$work/neo" "$overlay" "$NEO_SOURCE_ROOT/LICENSE"
    /usr/bin/install -m 0600 "$work/neo/$overlay"/omarchy_*.py "$work/runtime/"
    /usr/bin/install -m 0600 "$work/neo/$NEO_SOURCE_ROOT/LICENSE" "$work/runtime/LICENSE.neo"
    /usr/bin/install -m 0600 "$scripts/install-macos-neo.py" "$work/runtime/claw_neo.py"
fi

cd "$work/runtime"

# Do not inherit Asahi expert, alternate restore-server, or telemetry settings.
/usr/bin/env -i HOME="$work" PATH=/usr/bin:/bin:/usr/sbin:/sbin \
    DISTRO='Claw OS' DISTRO_DOCS='https://github.com/xiaoyu-work/claw-os/blob/main/docs/image-architecture.md' \
    OMARCHY_RESTORE_IMAGE="$decoder" OMARCHY_RESTORE_IMAGE_REQUIREMENT="$requirement" \
    REPO_BASE="$work" /usr/bin/caffeinate -dis /bin/sh ./install.sh "$mode"
