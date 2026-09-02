#!/usr/bin/env bash
# packaging/release-security/gpg-sign.sh — sign without putting the
# passphrase on any command line.
#
# `gpg --passphrase "$SECRET"` publishes the secret in `/proc/<pid>/cmdline`,
# which every local process can read for as long as gpg runs. Every Claw OS
# signing path therefore goes through these helpers, which hand the
# passphrase to gpg on a pipe under `--passphrase-fd 0` and never let it
# reach `argv` or a log.
#
# Source this file; it defines:
#
#   claw_gpg_sign_detached KEY_ID INPUT OUTPUT
#   claw_gpg_sign_clear    KEY_ID INPUT OUTPUT
#
# The passphrase is read from `$GPG_PASSPHRASE` when set. An unset or
# empty value means the key is not passphrase-protected.

claw_gpg_run() {
    local key_id="$1"
    shift
    local -a args=(
        --batch --yes --pinentry-mode loopback --default-key "$key_id"
    )
    if [ -n "${GPG_PASSPHRASE:-}" ]; then
        args+=(--passphrase-fd 0)
        printf '%s\n' "$GPG_PASSPHRASE" | gpg "${args[@]}" "$@"
    else
        gpg "${args[@]}" "$@" < /dev/null
    fi
}

claw_gpg_sign_detached() {
    local key_id="${1:?key id required}"
    local input="${2:?input required}"
    local output="${3:?output required}"
    claw_gpg_run "$key_id" --detach-sign --armor -o "$output" "$input"
}

claw_gpg_sign_clear() {
    local key_id="${1:?key id required}"
    local input="${2:?input required}"
    local output="${3:?output required}"
    claw_gpg_run "$key_id" --clearsign -o "$output" "$input"
}
