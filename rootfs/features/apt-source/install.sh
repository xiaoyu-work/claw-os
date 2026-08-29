#!/usr/bin/env bash
# rootfs/features/apt-source/install.sh — configure the Claw OS apt repo on
# the installed system. After this runs, `apt update` will see
# claw-os-agent, claw-os-base, and claw-os-desktop as upgradeable packages.
#
# Inherited from environment: ROOTFS.
#
# Overridable env vars:
#   COS_APT_REPO_URL    — repo base URL (default: official GH Pages)
#   COS_APT_REPO_SUITE  — suite name    (default: trixie)
#   COS_APT_PUBLIC_KEY_FILE — binary OpenPGP public keyring
#   COS_APT_PUBLIC_KEY_URL  — fallback public key URL
#   COS_APT_PUBLIC_KEY_FINGERPRINT — required when downloading the key

set -euo pipefail

COS_APT_REPO_URL="${COS_APT_REPO_URL:-https://xiaoyu-work.github.io/claw-os}"
COS_APT_REPO_SUITE="${COS_APT_REPO_SUITE:-trixie}"
COS_APT_PUBLIC_KEY_FILE="${COS_APT_PUBLIC_KEY_FILE:-$PROJECT_DIR/packaging/apt-repo/claw-os-archive-keyring.gpg}"
COS_APT_PUBLIC_KEY_URL="${COS_APT_PUBLIC_KEY_URL:-https://xiaoyu-work.github.io/claw-os/claw-os-archive-keyring.gpg}"
COS_APT_PUBLIC_KEY_FINGERPRINT="${COS_APT_PUBLIC_KEY_FINGERPRINT:-}"

if [ ! -s "$COS_APT_PUBLIC_KEY_FILE" ]; then
    if [ -z "$COS_APT_PUBLIC_KEY_FINGERPRINT" ]; then
        echo "error: set COS_APT_PUBLIC_KEY_FILE or COS_APT_PUBLIC_KEY_FINGERPRINT" >&2
        exit 1
    fi
    case "$COS_APT_PUBLIC_KEY_URL" in
        https://*) ;;
        *)
            echo "error: apt public key URL must use HTTPS" >&2
            exit 1
            ;;
    esac
    COS_APT_PUBLIC_KEY_FILE="$(mktemp)"
    trap 'rm -f "$COS_APT_PUBLIC_KEY_FILE"' EXIT
    echo "  :: fetching apt public key from $COS_APT_PUBLIC_KEY_URL"
    curl --proto '=https' --proto-redir '=https' -fsSL \
        "$COS_APT_PUBLIC_KEY_URL" -o "$COS_APT_PUBLIC_KEY_FILE"
fi
if ! gpg --batch --show-keys "$COS_APT_PUBLIC_KEY_FILE" >/dev/null 2>&1; then
    echo "error: invalid Claw OS apt public key: $COS_APT_PUBLIC_KEY_FILE" >&2
    exit 1
fi
if [ -n "$COS_APT_PUBLIC_KEY_FINGERPRINT" ]; then
    mapfile -t primary_fingerprints < <(
        gpg --batch --with-colons --show-keys "$COS_APT_PUBLIC_KEY_FILE" \
            | awk -F: '
                $1 == "pub" { want_fpr = 1; next }
                want_fpr && $1 == "fpr" {
                    print toupper($10)
                    want_fpr = 0
                }
            '
    )
    expected_fingerprint="$(printf '%s' "$COS_APT_PUBLIC_KEY_FINGERPRINT" \
        | tr -d '[:space:]' | tr '[:lower:]' '[:upper:]')"
    if [ "${#primary_fingerprints[@]}" -ne 1 ] \
        || [ "${primary_fingerprints[0]}" != "$expected_fingerprint" ]; then
        echo "error: Claw OS apt public key fingerprint mismatch" >&2
        exit 1
    fi
fi

echo "  :: writing /etc/apt/sources.list.d/claw-os.list"
mkdir -p "$ROOTFS/etc/apt/sources.list.d" "$ROOTFS/etc/apt/apt.conf.d"
install -Dm0644 "$COS_APT_PUBLIC_KEY_FILE" \
    "$ROOTFS/usr/share/keyrings/claw-os-archive-keyring.gpg"

# Signature options are freshness options too. `check-valid-until=yes`
# makes APT refuse an index whose signed `Valid-Until` has passed, so a
# stale or frozen mirror cannot keep offering a superseded release
# indefinitely; the `allow-*` options refuse every insecure fallback
# that would otherwise accept it. Downgrade protection itself lives in
# the packages and in /var/lib/cos/security — these only stop the
# repository layer from handing an old snapshot over in the first
# place.
cat > "$ROOTFS/etc/apt/sources.list.d/claw-os.list" <<EOF
# Claw OS — official package repository.
# Source: https://github.com/xiaoyu-work/claw-os
deb [signed-by=/usr/share/keyrings/claw-os-archive-keyring.gpg check-valid-until=yes allow-insecure=no allow-weak=no allow-downgrade-to-insecure=no by-hash=yes] $COS_APT_REPO_URL $COS_APT_REPO_SUITE main
EOF

cat > "$ROOTFS/etc/apt/apt.conf.d/50claw-os-repository" <<'EOF'
// Claw OS repository freshness.
//
// APT signatures prove who published an index; `Valid-Until` is what
// bounds how long that index may still be served. Keeping the check on
// — and refusing the insecure fallbacks that would bypass it — is what
// stops a stale or hostile mirror from replaying an old, still validly
// signed snapshot at an installed system.
Acquire::Check-Valid-Until "true";
Acquire::AllowInsecureRepositories "false";
Acquire::AllowDowngradeToInsecureRepositories "false";
EOF

echo "  :: apt source ready ($COS_APT_REPO_URL $COS_APT_REPO_SUITE main)"
