#!/usr/bin/env bash
# Preserve the latest signed desktop packages across Agent/base-only APT
# publications. A newly built desktop package already in DEBS_DIR always wins.

set -euo pipefail

EXISTING_APT_REPO_URL="${EXISTING_APT_REPO_URL:?set EXISTING_APT_REPO_URL}"
APT_PUBLIC_KEYRING="${APT_PUBLIC_KEYRING:?set APT_PUBLIC_KEYRING}"
SUITE="${SUITE:-trixie}"
DEBS_DIR="${DEBS_DIR:-build/debs}"
ALLOW_MISSING_EXISTING_REPO="${ALLOW_MISSING_EXISTING_REPO:-0}"

if [ ! -s "$APT_PUBLIC_KEYRING" ]; then
    echo "error: existing-repository keyring is missing: $APT_PUBLIC_KEYRING" >&2
    exit 1
fi
mkdir -p "$DEBS_DIR"

missing_arches=()
for arch in amd64 arm64; do
    if compgen -G "$DEBS_DIR/claw-os-desktop_*_${arch}.deb" >/dev/null; then
        echo "  :: using newly built claw-os-desktop package for $arch"
    else
        missing_arches+=("$arch")
    fi
done
if [ "${#missing_arches[@]}" -eq 0 ]; then
    exit 0
fi

inrelease_url="$EXISTING_APT_REPO_URL/dists/$SUITE/InRelease"
status="$(curl -L -sS -o /dev/null -w '%{http_code}' "$inrelease_url")"
case "$status" in
    200) ;;
    404)
        if [ "$ALLOW_MISSING_EXISTING_REPO" = "1" ]; then
            echo ":: no existing signed APT repository; bootstrap explicitly allowed"
            exit 0
        fi
        echo "error: existing APT repository is missing: $inrelease_url" >&2
        echo "       set ALLOW_MISSING_EXISTING_REPO=1 only for first publication" >&2
        exit 1
        ;;
    *)
        echo "error: existing APT repository returned HTTP $status: $inrelease_url" >&2
        exit 1
        ;;
esac

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

for arch in "${missing_arches[@]}"; do
    source_list="$tmp/source-${arch}.list"
    lists_dir="$tmp/lists-${arch}"
    mkdir -p "$lists_dir/partial"
    printf 'deb [arch=%s signed-by=%s] %s %s main\n' \
        "$arch" "$APT_PUBLIC_KEYRING" "$EXISTING_APT_REPO_URL" "$SUITE" \
        > "$source_list"

    apt_options=(
        -o "Dir::Etc::sourcelist=$source_list"
        -o "Dir::Etc::sourceparts=-"
        -o "Dir::State::lists=$lists_dir"
        -o "APT::Architecture=$arch"
        -o "APT::Architectures=$arch"
    )
    apt-get "${apt_options[@]}" update -qq
    candidate="$(
        apt-cache "${apt_options[@]}" policy "claw-os-desktop:$arch" \
            | awk '/^[[:space:]]*Candidate:/ { print $2; exit }'
    )"
    if [ -z "$candidate" ] || [ "$candidate" = "(none)" ]; then
        echo "  :: no existing claw-os-desktop package for $arch"
        continue
    fi

    echo "  :: preserving claw-os-desktop $candidate for $arch"
    (
        cd "$DEBS_DIR"
        apt-get "${apt_options[@]}" download "claw-os-desktop:$arch=$candidate"
    )
done
