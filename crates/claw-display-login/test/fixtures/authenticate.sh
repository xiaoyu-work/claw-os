#!/usr/bin/env bash
set -euo pipefail

# Run only in a fresh Root mount namespace; no real account or PAM file is changed.
[[ "$EUID" == 0 && $# == 2 ]] || exit 64
[[ "$(readlink /proc/self/ns/mnt)" != "$(readlink /proc/1/ns/mnt)" ]] || {
    printf 'private mount namespace required\n' >&2
    exit 64
}
fixture=$(realpath -- "$1")
private=$(realpath -- "$2")
[[ -x "$fixture" && -d "$private" && "$private" != / ]] || exit 64
mount --make-rprivate /
mount -t tmpfs -o mode=0755,nosuid,nodev tmpfs /run
printf '%s\n' \
    'root:x:0:0:root:/root:/bin/sh' \
    'claw-display-test:x:62050:62050:Private display fixture:/run/display-home:/bin/sh' \
    >"$private/passwd"
printf '%s\n' 'root:x:0:' 'claw-display-test:x:62050:' >"$private/group"
hash=$(printf '%s\n' 'private-disposable-display-fixture-password' | openssl passwd -6 -stdin)
printf 'root:!:20000:0:99999:7:::\nclaw-display-test:%s:20000:0:99999:7:::\n' "$hash" \
    >"$private/shadow"
chmod 0600 "$private/shadow"
mount --bind "$private/passwd" /etc/passwd
mount --bind "$private/group" /etc/group
mount --bind "$private/shadow" /etc/shadow
mkdir -m 0700 /run/display-home
chown 62050:62050 /run/display-home
mkdir -m 0700 "$private/pam"
printf '%s\n' \
    'auth required pam_unix.so' \
    'account required pam_unix.so' \
    'session required pam_loginuid.so' >"$private/pam/cosmic-greeter"
if "$fixture" "$private/pam" deny-password; then
    printf 'wrong private password was accepted\n' >&2
    exit 1
fi
"$fixture" "$private/pam" authenticate-only
