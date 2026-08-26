#!/usr/bin/env bash
# Merge locally built packages with the latest candidates from the existing
# signed repository. A local package replaces its candidate only when it is
# strictly newer in Debian version order.

set -euo pipefail

EXISTING_APT_REPO_URL="${EXISTING_APT_REPO_URL:?set EXISTING_APT_REPO_URL}"
APT_PUBLIC_KEYRING="${APT_PUBLIC_KEYRING:?set APT_PUBLIC_KEYRING}"
SUITE="${SUITE:-trixie}"
DEBS_DIR="${DEBS_DIR:-build/debs}"

if [ ! -s "$APT_PUBLIC_KEYRING" ]; then
    echo "error: existing-repository keyring is missing: $APT_PUBLIC_KEYRING" >&2
    exit 1
fi
mkdir -p "$DEBS_DIR"

# package|file architecture|APT query architecture|APT package expression
targets=(
    "claw-os-agent|amd64|amd64|claw-os-agent:amd64"
    "claw-os-agent|arm64|arm64|claw-os-agent:arm64"
    "claw-os-base|all|amd64|claw-os-base"
    "claw-os-desktop|amd64|amd64|claw-os-desktop:amd64"
    "claw-os-desktop|arm64|arm64|claw-os-desktop:arm64"
)

inrelease_url="$EXISTING_APT_REPO_URL/dists/$SUITE/InRelease"
status="$(curl -L -sS -o /dev/null -w '%{http_code}' "$inrelease_url")"
case "$status" in
    200) ;;
    404)
        echo ":: no existing signed APT repository; starting a new package pool"
        exit 0
        ;;
    *)
        echo "error: existing APT repository returned HTTP $status: $inrelease_url" >&2
        exit 1
        ;;
esac

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
declare -A prepared_arches=()

prepare_arch() {
    local arch="$1"
    if [ "${prepared_arches[$arch]:-}" = "1" ]; then
        return
    fi

    local source_list="$tmp/source-${arch}.list"
    local lists_dir="$tmp/lists-${arch}"
    local cache_dir="$tmp/cache-${arch}"
    mkdir -p "$lists_dir/partial" "$cache_dir/archives/partial"
    printf 'deb [arch=%s signed-by=%s] %s %s main\n' \
        "$arch" "$APT_PUBLIC_KEYRING" "$EXISTING_APT_REPO_URL" "$SUITE" \
        > "$source_list"

    local apt_options=(
        -o "Dir::Etc::sourcelist=$source_list"
        -o "Dir::Etc::sourceparts=-"
        -o "Dir::State::lists=$lists_dir"
        -o "Dir::Cache=$cache_dir"
        -o "Dir::Cache::archives=$cache_dir/archives"
        -o "APT::Architecture=$arch"
        -o "APT::Architectures=$arch"
    )
    apt-get "${apt_options[@]}" update -qq
    prepared_arches[$arch]=1
}

shopt -s nullglob
for target in "${targets[@]}"; do
    IFS='|' read -r package file_arch query_arch query <<< "$target"
    prepare_arch "$query_arch"

    source_list="$tmp/source-${query_arch}.list"
    lists_dir="$tmp/lists-${query_arch}"
    cache_dir="$tmp/cache-${query_arch}"
    apt_options=(
        -o "Dir::Etc::sourcelist=$source_list"
        -o "Dir::Etc::sourceparts=-"
        -o "Dir::State::lists=$lists_dir"
        -o "Dir::Cache=$cache_dir"
        -o "Dir::Cache::archives=$cache_dir/archives"
        -o "APT::Architecture=$query_arch"
        -o "APT::Architectures=$query_arch"
    )
    candidate="$(
        apt-cache "${apt_options[@]}" policy "$query" \
            | awk '/^[[:space:]]*Candidate:/ { candidate=$2 } END { print candidate }'
    )"

    local_debs=("$DEBS_DIR/${package}_"*"_${file_arch}.deb")
    newest_local=""
    newest_local_version=""
    for deb in "${local_debs[@]}"; do
        if ! version="$(dpkg-deb --field "$deb" Version)"; then
            echo "error: cannot read package version from $deb" >&2
            exit 1
        fi
        if [ -z "$version" ]; then
            echo "error: package has no version: $deb" >&2
            exit 1
        fi
        if [ -z "$newest_local_version" ] \
            || dpkg --compare-versions "$version" gt "$newest_local_version"; then
            newest_local="$deb"
            newest_local_version="$version"
        fi
    done

    if [ -z "$candidate" ] || [ "$candidate" = "(none)" ]; then
        if [ -z "$newest_local" ]; then
            echo "  :: no existing or local $package package for $file_arch"
            continue
        fi
        for deb in "${local_debs[@]}"; do
            [ "$deb" = "$newest_local" ] || rm -f -- "$deb"
        done
        echo "  :: using newly built $package $newest_local_version for $file_arch (no existing candidate)"
        continue
    fi

    if [ -n "$newest_local" ] \
        && dpkg --compare-versions "$newest_local_version" gt "$candidate"; then
        for deb in "${local_debs[@]}"; do
            [ "$deb" = "$newest_local" ] || rm -f -- "$deb"
        done
        echo "  :: using newly built $package $newest_local_version for $file_arch; newer than signed candidate $candidate"
        continue
    fi

    if [ -n "$newest_local" ]; then
        echo "  :: ignoring local $package $newest_local_version for $file_arch; signed candidate $candidate is not older"
        rm -f -- "${local_debs[@]}"
    fi
    echo "  :: preserving $package $candidate for $file_arch"
    (
        cd "$DEBS_DIR"
        apt-get "${apt_options[@]}" download "$query=$candidate"
    )
done
