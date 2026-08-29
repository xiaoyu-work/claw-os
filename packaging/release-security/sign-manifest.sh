#!/usr/bin/env bash
# packaging/release-security/sign-manifest.sh — resolve the signing key
# and emit a package's release manifest, failing closed.
#
# Sourced by `packaging/deb/build-debs.sh` and
# `packaging/deb/build-desktop-deb.sh` so both make the same decision,
# and so the decision can be tested directly with a real key.
#
# The distinction that matters:
#
#   * No key requested — an explicitly unsigned local build. The
#     manifest is still written; the installed system decides what an
#     unsigned one is worth, and the publication verifier refuses it.
#   * A key requested but unusable, or signing that fails — a hard
#     error. Clearing the key id and continuing would produce an
#     unsigned artifact under a name a publication workflow is about to
#     upload.
#
# Defines:
#   claw_resolve_signing_key            -> echoes the key id, or empty
#   claw_write_release_manifest ...     -> manifest.json{,.asc}

# Resolve and validate the requested release-security signing key.
#
# Echoes the key id when one was requested and its secret key is
# usable, echoes nothing when none was requested, and returns non-zero
# when one was requested but cannot be used.
claw_resolve_signing_key() {
    local key_id="${CLAW_OS_RELEASE_SECURITY_KEY_ID:-${GPG_KEY_ID:-}}"
    if [ -z "$key_id" ]; then
        echo "  :: no release-security signing key requested;" >&2
        echo "     THIS IS AN UNSIGNED LOCAL BUILD and cannot be published" >&2
        printf '\n'
        return 0
    fi
    if ! gpg --batch --list-secret-keys "$key_id" >/dev/null 2>&1; then
        echo "error: release-security signing key $key_id was requested but its" >&2
        echo "       secret key is unavailable. Refusing to emit an unsigned" >&2
        echo "       package under a signed build. Unset" >&2
        echo "       CLAW_OS_RELEASE_SECURITY_KEY_ID/GPG_KEY_ID for an" >&2
        echo "       explicitly unsigned local build." >&2
        return 1
    fi
    printf '%s\n' "$key_id"
}

# claw_write_release_manifest KEY_ID PASSPHRASE MAKE_MANIFEST PACKAGE \
#                             VERSION ARCH SUITE STAGE POLICY OUTPUT
#
# An empty KEY_ID means an unsigned build. Otherwise the detached
# signature must exist and verify, and any failure removes both files so
# no later step can package a half-written or unsigned manifest.
claw_write_release_manifest() {
    local key_id="$1" passphrase="$2" make_manifest="$3" package="$4"
    local version="$5" arch="$6" suite="$7" stage="$8" policy="$9" output="${10}"
    local sign_args=()
    [ -n "$key_id" ] && sign_args=(--sign-key "$key_id")

    install -d -m 0755 "$(dirname "$output")"
    if ! GPG_PASSPHRASE="$passphrase" python3 "$make_manifest" \
        --package "$package" \
        --version "$version" \
        --arch "$arch" \
        --suite "$suite" \
        --stage-dir "$stage" \
        --policy "$policy" \
        --output "$output" \
        "${sign_args[@]}" > /dev/null; then
        rm -f "$output" "$output.asc"
        echo "error: could not write the $package release manifest" >&2
        return 1
    fi
    chmod 0644 "$output"

    [ -n "$key_id" ] || return 0

    if [ ! -s "$output.asc" ]; then
        rm -f "$output" "$output.asc"
        echo "error: signing $package with $key_id produced no signature;" >&2
        echo "       refusing to build an unsigned package" >&2
        return 1
    fi
    chmod 0644 "$output.asc"
    if ! gpg --batch --verify "$output.asc" "$output" >/dev/null 2>&1; then
        rm -f "$output" "$output.asc"
        echo "error: the $package release manifest signature does not verify" >&2
        return 1
    fi
    return 0
}
