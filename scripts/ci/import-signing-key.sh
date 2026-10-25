#!/usr/bin/env bash
set -euo pipefail

: "${RUNNER_TEMP:?RUNNER_TEMP is required}"
: "${GITHUB_ENV:?GITHUB_ENV is required}"
if [ -z "${CLAW_OS_APT_SIGNING_PRIVATE_KEY:-}" ]; then
    echo "error: CLAW_OS_APT_SIGNING_PRIVATE_KEY is required for signed publication" >&2
    exit 1
fi

export GNUPGHOME
GNUPGHOME="$(mktemp -d "$RUNNER_TEMP/claw-os-release-gnupg.XXXXXX")"
printf '%s' "$CLAW_OS_APT_SIGNING_PRIVATE_KEY" | gpg --batch --import
key_id="$(gpg --batch --with-colons --list-secret-keys \
    | awk -F: '$1 == "sec" { print $5; exit }')"
test -n "$key_id"
keyring="$GNUPGHOME/archive-keyring.gpg"
gpg --batch --export "$key_id" > "$keyring"
test -s "$keyring"
{
    echo "GNUPGHOME=$GNUPGHOME"
    echo "GPG_KEY_ID=$key_id"
    echo "CLAW_OS_RELEASE_SECURITY_KEY_ID=$key_id"
    echo "CLAW_OS_RELEASE_SECURITY_KEYRING=$keyring"
} >> "$GITHUB_ENV"
