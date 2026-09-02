#!/usr/bin/env bash
# Merge locally built packages with the latest candidates from the existing
# signed repository. A local package replaces its candidate only when it is
# strictly newer in Debian version order.

set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
EXISTING_APT_REPO_URL="${EXISTING_APT_REPO_URL:?set EXISTING_APT_REPO_URL}"
APT_PUBLIC_KEYRING="${APT_PUBLIC_KEYRING:?set APT_PUBLIC_KEYRING}"
SUITE="${SUITE:-trixie}"
DEBS_DIR="${DEBS_DIR:-build/debs}"
PREVIOUS_RELEASE_SECURITY_DIR="${COS_PREVIOUS_RELEASE_SECURITY_DIR:-build/release-security-previous}"

if [ ! -s "$APT_PUBLIC_KEYRING" ]; then
    echo "error: existing-repository keyring is missing: $APT_PUBLIC_KEYRING" >&2
    exit 1
fi
mkdir -p "$DEBS_DIR"
# Start from nothing: a marker left by an earlier run would misstate
# what the *published* repository currently guarantees.
rm -rf "$PREVIOUS_RELEASE_SECURITY_DIR"
mkdir -p "$PREVIOUS_RELEASE_SECURITY_DIR"

# The security epoch a package build carries, or 0 when it predates
# release-security metadata.
package_security_epoch() {
    local deb="$1" epoch
    epoch="$(dpkg-deb --field "$deb" XB-Claw-Os-Security-Epoch 2>/dev/null || true)"
    case "$epoch" in
        ''|*[!0-9]*) printf '0\n' ;;
        *) printf '%s\n' "$epoch" ;;
    esac
}

# package|file architecture|APT query architecture|APT package expression
targets=(
    "claw-os-agent|amd64|amd64|claw-os-agent:amd64"
    "claw-os-agent|arm64|arm64|claw-os-agent:arm64"
    "claw-os-base|all|amd64|claw-os-base"
    "claw-os-desktop|amd64|amd64|claw-os-desktop:amd64"
    "claw-os-desktop|arm64|arm64|claw-os-desktop:arm64"
)

inrelease_url="$EXISTING_APT_REPO_URL/dists/$SUITE/InRelease"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
declare -A prepared_arches=()

# Fetching the current repository state is a *security* step, not a
# convenience: what it returns decides whether this publication is
# allowed to regress anything. A transport failure therefore never
# degrades into "there is no repository yet". Only an authenticated,
# unambiguous 404 means first publication.
#
# Every request is cache-busting. A CDN or proxy that keeps serving a
# stale snapshot would otherwise hide a newer published version from
# the regression checks below and make a downgrade look legitimate.
CLAW_NO_CACHE=(
    -H 'Cache-Control: no-cache, no-store, max-age=0'
    -H 'Pragma: no-cache'
)
CLAW_FETCH_STATUS=000
CLAW_FETCH_RC=0

# Run curl and report *curl's* exit status. `if ! status="$(curl ...)"`
# leaves `$?` describing the `!`, not the command, so the diagnostics
# always claimed exit 0. Capture it explicitly instead; the fail-closed
# control flow is unchanged.
claw_fetch() {
    local url="$1" out="$2"
    local body rc separator='?'
    case "$url" in *\?*) separator='&' ;; esac
    set +e
    body="$(curl -L -sS --fail-with-body -o "$out" -w '%{http_code}' \
        "${CLAW_NO_CACHE[@]}" "${url}${separator}cachebust=$(date -u +%s)-$$")"
    rc="$?"
    set -e
    CLAW_FETCH_STATUS="${body:-000}"
    CLAW_FETCH_RC="$rc"
    return "$rc"
}

if ! claw_fetch "$inrelease_url" "$tmp/InRelease"; then
    case "$CLAW_FETCH_STATUS" in
        404)
            echo ":: no existing signed APT repository; starting a new package pool"
            : > "$PREVIOUS_RELEASE_SECURITY_DIR/.no-existing-repository"
            exit 0
            ;;
        *)
            echo "error: could not read the existing APT repository (curl exit" >&2
            echo "       $CLAW_FETCH_RC, HTTP $CLAW_FETCH_STATUS): $inrelease_url" >&2
            echo "       Refusing to publish: a fetch failure is not evidence that" >&2
            echo "       the repository is empty." >&2
            exit 1
            ;;
    esac
fi
status="$CLAW_FETCH_STATUS"
if [ "$status" != "200" ]; then
    echo "error: existing APT repository returned HTTP $status: $inrelease_url" >&2
    exit 1
fi
if [ ! -s "$tmp/InRelease" ]; then
    echo "error: the existing InRelease is empty: $inrelease_url" >&2
    exit 1
fi

# The InRelease is clear-signed. Verify it before believing anything it
# says — including whether this repository has already established a
# release-security baseline.
if ! gpgv --keyring "$APT_PUBLIC_KEYRING" --output "$tmp/Release" \
    "$tmp/InRelease" >/dev/null 2>&1; then
    echo "error: the existing InRelease does not verify against $APT_PUBLIC_KEYRING" >&2
    exit 1
fi

# Freshness of what we just authenticated. A signature proves origin,
# not recency: an origin or CDN replaying a months-old signed snapshot
# would otherwise let this publication "not regress" a repository state
# that no longer exists.
#
# Residual, stated plainly: an attacker controlling the whole origin can
# serve a consistently old but still-valid snapshot inside its
# Valid-Until window, and no client-side check here can see that. The
# defences against that are the short Valid-Until, the scheduled
# metadata refresh, and the installed floor on each machine.
"$PROJECT_DIR/packaging/apt-repo/check-index-freshness.py" "$tmp/Release"

baseline_field="$(awk -F': *' '
    /^Claw-Os-Release-Security-Baseline:/ { print $2; exit }
' "$tmp/Release" | tr -d '[:space:]')"
if [ "$baseline_field" = "1" ]; then
    echo ":: the published repository advertises a release-security baseline"
    : > "$PREVIOUS_RELEASE_SECURITY_DIR/.baseline-established"
    rm -f "$PREVIOUS_RELEASE_SECURITY_DIR/.no-existing-repository"
else
    echo ":: the published repository predates release-security metadata"
    rm -f "$PREVIOUS_RELEASE_SECURITY_DIR/.baseline-established"
    : > "$PREVIOUS_RELEASE_SECURITY_DIR/.pre-protection-repository"
fi

# Fetch one signed artifact from the published repository. Once a
# baseline exists every one of these is mandatory: a 404, a transport
# error or a signature failure is fatal, because the alternative is
# publishing without knowing what we would be replacing.
#
# The detached signature proves the artifact came from the publisher.
# The checksum cross-check proves it belongs to *this* signed index, so
# an origin cannot pair a current InRelease with an older, separately
# signed manifest it kept around.
release_checksum() {
    local relative="$1"
    awk -v want="$relative" '
        /^SHA256:/ { in_block = 1; next }
        /^[A-Za-z-]+:/ { in_block = 0 }
        in_block && NF == 3 && $3 == want { print $1; exit }
    ' "$tmp/Release"
}

cross_check_against_release() {
    local relative="$1" file="$2" required="$3"
    local expected actual
    expected="$(release_checksum "$relative")"
    if [ -z "$expected" ]; then
        if [ "$required" = "required" ]; then
            echo "error: the signed Release does not list $relative; the origin is" >&2
            echo "       serving an index and artifacts that do not belong together" >&2
            exit 1
        fi
        # Not part of this signed index, so it is not published.
        return 1
    fi
    actual="$(sha256sum "$file" | cut -d' ' -f1)"
    if [ "$expected" != "$actual" ]; then
        echo "error: $relative does not match the checksum in the signed Release" >&2
        echo "       (index $expected, fetched $actual)" >&2
        exit 1
    fi
    return 0
}

fetch_signed_artifact() {
    local name="$1" required="$2"
    local url="$EXISTING_APT_REPO_URL/dists/$SUITE/release-security/$name"
    local body="$tmp/$name" signature="$tmp/$name.asc"
    local body_status signature_status body_rc signature_rc

    claw_fetch "$url" "$body" || true
    body_status="$CLAW_FETCH_STATUS"
    body_rc="$CLAW_FETCH_RC"
    claw_fetch "$url.asc" "$signature" || true
    signature_status="$CLAW_FETCH_STATUS"
    signature_rc="$CLAW_FETCH_RC"

    if [ "$body_rc" != "0" ] || [ "$signature_rc" != "0" ] \
        || [ "$body_status" != "200" ] || [ "$signature_status" != "200" ]; then
        if [ "$required" = "required" ]; then
            echo "error: $name is advertised by the published repository but could" >&2
            echo "       not be retrieved (curl $body_rc/$signature_rc," >&2
            echo "       HTTP $body_status/$signature_status): $url" >&2
            exit 1
        fi
        return 1
    fi
    if ! gpgv --keyring "$APT_PUBLIC_KEYRING" "$signature" "$body" >/dev/null 2>&1; then
        echo "error: the published $name does not verify against the repository key" >&2
        exit 1
    fi
    cross_check_against_release "release-security/$name" "$body" "$required" || return 1
    cross_check_against_release "release-security/$name.asc" "$signature" "$required" || return 1
    cp "$body" "$PREVIOUS_RELEASE_SECURITY_DIR/$name"
    cp "$signature" "$PREVIOUS_RELEASE_SECURITY_DIR/$name.asc"
    return 0
}

if [ "$baseline_field" = "1" ]; then
    fetch_signed_artifact baseline.json required
    echo "  :: preserved the published release-security baseline marker"
fi

# Preserve the release-security metadata the repository publishes today,
# so the publication step can refuse a set that regresses it. Which of
# these must exist is decided by the *signed* Packages indexes below,
# not by whether a fetch happened to succeed.
for package in claw-os-agent claw-os-base claw-os-desktop; do
    for arch in amd64 arm64 all; do
        name="${package}_${arch}.json"
        if [ "$baseline_field" = "1" ]; then
            # A published package must have published metadata. The
            # per-package requirement is checked after the indexes are
            # read, so an architecture the repository simply does not
            # carry is not treated as missing.
            fetch_signed_artifact "$name" optional \
                && echo "  :: preserved published release-security for $package ($arch)"
        else
            fetch_signed_artifact "$name" optional >/dev/null || true
        fi
    done
done

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

    # The signed index says this package/architecture is published. Once
    # a baseline exists its release-security manifest must be published
    # too, and must have been retrieved and verified above — otherwise
    # the regression checks would run against a hole.
    if [ "$baseline_field" = "1" ] \
        && [ ! -s "$PREVIOUS_RELEASE_SECURITY_DIR/${package}_${file_arch}.json" ]; then
        echo "error: the published repository offers $package $candidate for" >&2
        echo "       $file_arch but no verified release-security manifest for it." >&2
        echo "       Refusing to publish against an incomplete baseline." >&2
        exit 1
    fi

    if [ -n "$newest_local" ] \
        && dpkg --compare-versions "$newest_local_version" gt "$candidate"; then
        # Version ordering alone is not enough: a build that carries a
        # lower security epoch must never replace a published one, even
        # when its Debian version sorts higher. The comparison is made
        # against the *authenticated* published artifact, so a download
        # failure is fatal rather than a reason to skip the check.
        candidate_deb="$tmp/candidate-${package}-${file_arch}.deb"
        rm -f "$candidate_deb"
        if ( cd "$tmp" && apt-get "${apt_options[@]}" download "$query=$candidate" \
            >/dev/null 2>&1 ); then
            downloaded="$(find "$tmp" -maxdepth 1 -name "${package}_*_${file_arch}.deb" \
                -o -maxdepth 1 -name "${package}_*_all.deb" | head -1)"
            if [ -n "$downloaded" ]; then
                mv "$downloaded" "$candidate_deb"
            fi
        fi
        if [ ! -s "$candidate_deb" ]; then
            if [ "$baseline_field" = "1" ]; then
                echo "error: could not download the published $package $candidate for" >&2
                echo "       $file_arch; refusing to replace an artifact this run could" >&2
                echo "       not authenticate." >&2
                exit 1
            fi
            echo "  :: note: pre-protection repository; $package $candidate could not be" >&2
            echo "     downloaded for the epoch comparison" >&2
        else
            local_epoch="$(package_security_epoch "$newest_local")"
            candidate_epoch="$(package_security_epoch "$candidate_deb")"
            if [ "$local_epoch" -lt "$candidate_epoch" ]; then
                echo "error: refusing to publish $package $newest_local_version" >&2
                echo "       (security epoch $local_epoch) over the published" >&2
                echo "       $candidate (security epoch $candidate_epoch)" >&2
                exit 1
            fi
        fi
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
