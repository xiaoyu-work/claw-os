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
# Enough of curl for the sync script: `-o` output, `%{http_code}` and
# `--fail-with-body`. Bodies come from FIXTURE_BODY_DIR, keyed by the
# last path segment of the URL.
output=""
url=""
previous=""
fail_mode=0
for argument in "$@"; do
    case "$previous" in
        -o) output="$argument" ;;
    esac
    case "$argument" in
        --fail-with-body) fail_mode=1 ;;
        http://*|https://*) url="$argument" ;;
    esac
    previous="$argument"
done
status="${FIXTURE_HTTP_STATUS:?}"
if [ -n "$output" ]; then
    name="${url##*/}"
    # The sync script appends a cache-busting query so no proxy can
    # serve it a stale snapshot. A real origin ignores it; so does this.
    name="${name%%\?*}"
    if [ "$status" = "200" ] && [ -n "${FIXTURE_BODY_DIR:-}" ] \
        && [ -f "$FIXTURE_BODY_DIR/$name" ]; then
        cp "$FIXTURE_BODY_DIR/$name" "$output"
    else
        : > "$output"
    fi
fi
printf '%s' "$status"
if [ "$fail_mode" = "1" ] && [ "$status" != "200" ]; then
    exit 22
fi
exit 0
EOF

cat > "$TEST_ROOT/bin/gpgv" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
# The fixture repository is not really signed; the merge logic under
# test is what happens *after* verification succeeds.
output=""
previous=""
for argument in "$@"; do
    case "$previous" in
        --output) output="$argument" ;;
    esac
    previous="$argument"
done
if [ -n "$output" ]; then
    cp "${!#}" "$output" 2>/dev/null || : > "$output"
fi
exit 0
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
chmod +x "$TEST_ROOT/bin/curl" "$TEST_ROOT/bin/gpgv" \
    "$TEST_ROOT/bin/apt-cache" "$TEST_ROOT/bin/apt-get"

# A published repository that predates release-security metadata: the
# merge logic under test is the same either way, and the baseline
# ratchet has its own suite in test-release-security-publication.sh.
mkdir -p "$TEST_ROOT/bodies"
# `Date` and `Valid-Until` are generated: the publisher refuses to
# treat an undated, future-dated or expired index as current state.
write_inrelease() {
    local target="$1" date="$2" valid_until="$3"
    cat > "$target" <<EOF
Origin: Claw OS
Suite: trixie
Codename: trixie
Architectures: amd64 arm64 all
Components: main
Date: $date
Valid-Until: $valid_until
EOF
}
write_inrelease "$TEST_ROOT/bodies/InRelease" \
    "$(date -u -R)" "$(date -u -R -d '+30 days')"

run_sync() {
    local scenario="$1"
    local http_status="$2"
    mkdir -p "$scenario/tmp"
    PATH="$TEST_ROOT/bin:$ORIGINAL_PATH" \
        FIXTURE_HTTP_STATUS="$http_status" \
        FIXTURE_BODY_DIR="$TEST_ROOT/bodies" \
        FIXTURE_CANDIDATES="$scenario/candidates" \
        FIXTURE_CALL_LOG="$scenario/calls" \
        EXISTING_APT_REPO_URL="https://apt.example.invalid" \
        APT_PUBLIC_KEYRING="$scenario/keyring.gpg" \
        DEBS_DIR="$scenario/debs" \
        COS_PREVIOUS_RELEASE_SECURITY_DIR="$scenario/release-security-previous" \
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
