#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../../.." && pwd)"
SYNC_SCRIPT="$PROJECT_DIR/packaging/apt-repo/sync-existing-packages.sh"
TEST_ROOT="$PROJECT_DIR/build/test-sync-existing-packages.$$"
ORIGINAL_PATH="$PATH"

trap 'rm -rf "$TEST_ROOT"' EXIT
rm -rf "$TEST_ROOT"
mkdir -p "$TEST_ROOT/bin"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

assert_file() {
    [ -f "$1" ] || fail "expected file: $1"
}

assert_absent() {
    [ ! -e "$1" ] || fail "unexpected file: $1"
}

assert_same() {
    cmp -s "$1" "$2" || fail "files differ: $1 $2"
}

make_deb() {
    local scenario="$1"
    local package="$2"
    local version="$3"
    local architecture="$4"
    local origin="$5"
    local output_dir="$6"
    local stage="$scenario/stage/${package}-${version}-${architecture}-${origin}"
    local output="$output_dir/${package}_${version}_${architecture}.deb"

    rm -rf "$stage"
    mkdir -p "$stage/DEBIAN" "$stage/usr/share/$package" "$output_dir"
    cat > "$stage/DEBIAN/control" <<EOF
Package: $package
Version: $version
Section: misc
Priority: optional
Architecture: $architecture
Maintainer: Claw OS Tests <tests@example.invalid>
Description: APT synchronization test fixture
EOF
    printf '%s\n' "$origin" > "$stage/usr/share/$package/fixture-origin"
    dpkg-deb --root-owner-group --nocheck --build "$stage" "$output" >/dev/null
}

cat > "$TEST_ROOT/bin/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s' "${FIXTURE_HTTP_STATUS:?}"
EOF

cat > "$TEST_ROOT/bin/apt-cache" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
query="${!#}"
printf 'apt-cache %s\n' "$query" >> "${FIXTURE_CALL_LOG:?}"
candidate="$(
    awk -F '|' -v query="$query" \
        '$1 == query { print $2; exit }' "${FIXTURE_CANDIDATES:?}"
)"
printf '  Candidate: %s\n' "${candidate:-(none)}"
EOF

cat > "$TEST_ROOT/bin/apt-get" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
mode=""
for argument in "$@"; do
    case "$argument" in
        update|download) mode="$argument" ;;
    esac
done
printf 'apt-get %s\n' "$mode" >> "${FIXTURE_CALL_LOG:?}"
case "$mode" in
    update)
        exit 0
        ;;
    download)
        request="${!#}"
        query="${request%%=*}"
        version="${request#*=}"
        artifact="$(
            awk -F '|' -v query="$query" -v version="$version" \
                '$1 == query && $2 == version { print $3; exit }' \
                "${FIXTURE_CANDIDATES:?}"
        )"
        [ -f "$artifact" ] || {
            echo "fixture package not found for $request" >&2
            exit 1
        }
        cp "$artifact" .
        ;;
    *)
        echo "unexpected apt-get invocation: $*" >&2
        exit 1
        ;;
esac
EOF
chmod +x "$TEST_ROOT/bin/curl" "$TEST_ROOT/bin/apt-cache" "$TEST_ROOT/bin/apt-get"

run_sync() {
    local scenario="$1"
    local http_status="$2"
    mkdir -p "$scenario/tmp"
    PATH="$TEST_ROOT/bin:$ORIGINAL_PATH" \
        FIXTURE_HTTP_STATUS="$http_status" \
        FIXTURE_CANDIDATES="$scenario/candidates" \
        FIXTURE_CALL_LOG="$scenario/calls" \
        EXISTING_APT_REPO_URL="https://apt.example.invalid" \
        APT_PUBLIC_KEYRING="$scenario/keyring.gpg" \
        DEBS_DIR="$scenario/debs" \
        TMPDIR="$scenario/tmp" \
        bash "$SYNC_SCRIPT"
}

test_out_of_order_publication() {
    local scenario="$TEST_ROOT/out-of-order"
    local debs="$scenario/debs"
    local remote="$scenario/remote"
    local candidates="$scenario/candidates"
    mkdir -p "$debs" "$remote"
    printf 'fixture keyring\n' > "$scenario/keyring.gpg"
    : > "$scenario/calls"

    make_deb "$scenario" claw-os-agent 2.0 amd64 remote "$remote"
    make_deb "$scenario" claw-os-agent 1.0+git9 arm64 remote "$remote"
    make_deb "$scenario" claw-os-base 2.0 all remote "$remote"
    make_deb "$scenario" claw-os-desktop 2.0 amd64 remote "$remote"
    cat > "$candidates" <<EOF
claw-os-agent:amd64|2.0|$remote/claw-os-agent_2.0_amd64.deb
claw-os-agent:arm64|1.0+git9|$remote/claw-os-agent_1.0+git9_arm64.deb
claw-os-base|2.0|$remote/claw-os-base_2.0_all.deb
claw-os-desktop:amd64|2.0|$remote/claw-os-desktop_2.0_amd64.deb
EOF

    make_deb "$scenario" claw-os-agent 2.0~rc1 amd64 older-build "$debs"
    make_deb "$scenario" claw-os-agent 1.0+git10 arm64 newer-build "$debs"
    make_deb "$scenario" claw-os-base 2.0 all equal-build "$debs"
    make_deb "$scenario" claw-os-desktop 1.0 arm64 first-build "$debs"
    cmp -s "$debs/claw-os-base_2.0_all.deb" \
        "$remote/claw-os-base_2.0_all.deb" \
        && fail "equal-version fixtures must have different contents"

    run_sync "$scenario" 200

    assert_absent "$debs/claw-os-agent_2.0~rc1_amd64.deb"
    assert_same "$remote/claw-os-agent_2.0_amd64.deb" \
        "$debs/claw-os-agent_2.0_amd64.deb"
    assert_file "$debs/claw-os-agent_1.0+git10_arm64.deb"
    assert_absent "$debs/claw-os-agent_1.0+git9_arm64.deb"
    assert_same "$remote/claw-os-base_2.0_all.deb" \
        "$debs/claw-os-base_2.0_all.deb"
    assert_same "$remote/claw-os-desktop_2.0_amd64.deb" \
        "$debs/claw-os-desktop_2.0_amd64.deb"
    assert_file "$debs/claw-os-desktop_1.0_arm64.deb"

    local package_count
    package_count="$(find "$debs" -maxdepth 1 -type f -name '*.deb' | wc -l)"
    [ "$package_count" -eq 5 ] \
        || fail "expected one package per target, found $package_count"
}

test_first_publication() {
    local scenario="$TEST_ROOT/first-publication"
    local debs="$scenario/debs"
    mkdir -p "$debs"
    printf 'fixture keyring\n' > "$scenario/keyring.gpg"
    : > "$scenario/candidates"
    : > "$scenario/calls"

    make_deb "$scenario" claw-os-agent 1.0 amd64 first-build "$debs"
    cp "$debs/claw-os-agent_1.0_amd64.deb" "$scenario/expected.deb"

    run_sync "$scenario" 404

    assert_same "$scenario/expected.deb" "$debs/claw-os-agent_1.0_amd64.deb"
    [ ! -s "$scenario/calls" ] \
        || fail "first publication must not query a repository that does not exist"
}

test_out_of_order_publication
test_first_publication
echo "PASS: APT package synchronization"
