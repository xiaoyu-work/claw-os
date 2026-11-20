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
driver="$(/usr/bin/dirname "$0")/install-macos.py"

work=$(/usr/bin/mktemp -d /private/var/tmp/claw-asahi.XXXXXX)
trap '/bin/rm -rf -- "$work"' EXIT

verify() {
    printf '%s  %s\n' "$1" "$2" | /usr/bin/env -i PATH=/usr/bin:/bin /usr/bin/shasum -a 256 -c - >/dev/null
}

/usr/bin/install -d "$work/os" "$work/runtime"
/bin/cp "$payload" "$work/os/claw-os.zip"
verify "$digest" "$work/os/claw-os.zip" || fail "Linux payload checksum does not match"

# AsahiLinux/asahi-installer v0.9.2, signed source dffbb38ef0c00c0431c609ecd8a00f42deb5b24c.
/usr/bin/curl -q -fsSL --proto '=https' --proto-redir '=https' --output "$work/engine.tar.gz" \
    https://cdn.asahilinux.org/installer/installer-v0.9.2.tar.gz
verify 484d999e53b4aef27d17c4478e49567e96842f8a6cb35c073a0df5bf0bbe8276 "$work/engine.tar.gz" \
    || fail "pinned Asahi installer checksum does not match"
/usr/bin/tar -oxzf "$work/engine.tar.gz" -C "$work/runtime"
/bin/mv "$work/runtime/main.py" "$work/runtime/asahi_engine.py"
/usr/bin/install -m 0600 "$driver" "$work/runtime/main.py"
cd "$work/runtime"

# Do not inherit Asahi expert, alternate restore-server, or telemetry settings.
/usr/bin/env -i HOME="$work" PATH=/usr/bin:/bin:/usr/sbin:/sbin \
    DISTRO='Claw OS' DISTRO_DOCS='https://github.com/xiaoyu-work/claw-os/blob/main/docs/image-architecture.md' \
    REPO_BASE="$work" /usr/bin/caffeinate -dis /bin/sh ./install.sh "$mode"
