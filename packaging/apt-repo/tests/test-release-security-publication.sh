#!/bin/bash
# packaging/apt-repo/tests/test-release-security-publication.sh
#
# Publication-side regression tests for Claw OS update downgrade
# protection. These run against real `.deb` archives, real OpenPGP
# signatures made with an ephemeral key, and the real
# `verify-release-security.sh` used by `build-repo.sh`.
#
# The property under test: a signed repository must never publish a set
# that moves an installed system backwards. Authenticity is APT's job;
# refusing to *republish* an older security epoch, an older version, a
# different artifact for a published version, or a mutually
# incompatible set is this job.
#
# Usage:
#   bash packaging/apt-repo/tests/test-release-security-publication.sh
#
# Environment:
#   COS_TEST_TMPDIR  scratch root (must be a Linux filesystem)

set -euo pipefail

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PROJECT_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/../../.." && pwd)

VERIFY="$PROJECT_DIR/packaging/apt-repo/verify-release-security.sh"
BUILD_REPO="$PROJECT_DIR/packaging/apt-repo/build-repo.sh"
SYNC="$PROJECT_DIR/packaging/apt-repo/sync-existing-packages.sh"
MAKE_MANIFEST="$PROJECT_DIR/packaging/release-security/make-manifest.py"
POLICY="$PROJECT_DIR/packaging/release-security/policy.json"

WORK_ROOT="${COS_TEST_TMPDIR:-$PROJECT_DIR/build/tests}"
WORK="$WORK_ROOT/release-security-publication-$$"
SUITE=trixie
COMPONENT=main
PASS=0

cleanup() {
    if [ -n "${GNUPGHOME:-}" ] && [ -d "${GNUPGHOME:-}" ]; then
        gpgconf --kill all >/dev/null 2>&1 || true
    fi
    # `COS_TEST_KEEP=1` retains the scratch repositories so a failed
    # publication can be inspected as the real thing.
    [ -n "${COS_TEST_KEEP:-}" ] || rm -rf "$WORK"
}
trap cleanup EXIT

fail() {
    printf 'not ok - %s\n' "$*" >&2
    exit 1
}

ok() {
    PASS=$((PASS + 1))
    printf 'ok %d - %s\n' "$PASS" "$*"
}

for tool in dpkg-deb gpg gpgv python3 apt-ftparchive; do
    command -v "$tool" >/dev/null 2>&1 || fail "$tool is required for this test"
done

mkdir -p "$WORK"

export GNUPGHOME="$WORK/gnupg"
mkdir -m 700 -p "$GNUPGHOME"
gpg --batch --quiet --passphrase '' --quick-generate-key \
    'Claw OS Publication Test <test@example.invalid>' default default never \
    >/dev/null 2>&1
KEY_ID="$(gpg --batch --with-colons --list-secret-keys \
    | awk -F: '$1 == "fpr" { print $10; exit }')"
[ -n "$KEY_ID" ] || fail "could not create an ephemeral signing key"
KEYRING="$WORK/keyring.gpg"
gpg --batch --export "$KEY_ID" > "$KEYRING"

component_paths() {
    python3 - "$POLICY" "$1" <<'PY'
import json, sys
policy = json.load(open(sys.argv[1], encoding="utf-8"))
for entry in policy["components"]:
    if entry["package"] == sys.argv[2]:
        print(entry["path"])
PY
}

# make_deb <out-dir> <package> <version> <arch> <marker>
#          [--epoch N] [--unsigned] [--issued-at ISO] [--tamper]
make_deb() {
    local out_dir="$1" package="$2" version="$3" arch="$4" marker="$5"
    shift 5
    local epoch="" unsigned="" issued_at="" tamper="" policy="$POLICY"
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --epoch) epoch="$2"; shift 2 ;;
            --unsigned) unsigned=1; shift ;;
            --issued-at) issued_at="$2"; shift 2 ;;
            --tamper) tamper=1; shift ;;
            *) fail "make_deb: unknown option $1" ;;
        esac
    done

    local stage="$WORK/stage-$package-$version-$arch"
    rm -rf "$stage"
    mkdir -p "$stage/DEBIAN" "$stage/usr/lib/cos/release-security/$package"
    local path
    for path in $(component_paths "$package"); do
        mkdir -p "$stage$(dirname "$path")"
        printf '%s %s %s\n' "$path" "$version" "$marker" > "$stage$path"
        chmod 0755 "$stage$path"
    done

    if [ -n "$epoch" ]; then
        policy="$stage/policy.json"
        python3 - "$POLICY" "$policy" "$epoch" <<'PY'
import json, sys
document = json.load(open(sys.argv[1], encoding="utf-8"))
document["security_epoch"] = int(sys.argv[3])
open(sys.argv[2], "w", encoding="utf-8").write(json.dumps(document))
PY
    fi

    local sign_args=() issued_args=()
    [ -z "$unsigned" ] && sign_args=(--sign-key "$KEY_ID")
    [ -n "$issued_at" ] && issued_args=(--issued-at "$issued_at")
    python3 "$MAKE_MANIFEST" \
        --package "$package" \
        --version "$version" \
        --arch "$arch" \
        --suite "$SUITE" \
        --stage-dir "$stage" \
        --policy "$policy" \
        --output "$stage/usr/lib/cos/release-security/$package/manifest.json" \
        "${sign_args[@]}" "${issued_args[@]}" > /dev/null
    rm -f "$stage/policy.json"

    if [ -n "$tamper" ]; then
        python3 - "$stage/usr/lib/cos/release-security/$package/manifest.json" <<'PY'
import json, sys
path = sys.argv[1]
document = json.loads(open(path, encoding="utf-8").read())
document["security_epoch"] = 99
open(path, "w", encoding="utf-8").write(
    json.dumps(document, sort_keys=True, separators=(",", ":")) + "\n"
)
PY
    fi

    local security_epoch
    security_epoch="${epoch:-$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["security_epoch"])' "$POLICY")}"
    cat > "$stage/DEBIAN/control" <<EOF
Package: $package
Version: $version
Section: admin
Priority: optional
Architecture: $arch
Maintainer: Claw OS <noreply@github.com>
XB-Claw-Os-Security-Epoch: $security_epoch
Description: publication fixture
EOF

    mkdir -p "$out_dir/pool/$COMPONENT/c/$package"
    dpkg-deb -Znone --root-owner-group --build "$stage" \
        "$out_dir/pool/$COMPONENT/c/$package/${package}_${version}_${arch}.deb" \
        >/dev/null
}

fresh_repo() {
    local repo="$WORK/$1"
    rm -rf "$repo"
    mkdir -p "$repo/pool/$COMPONENT"
    printf '%s\n' "$repo"
}

# The published-state markers `sync-existing-packages.sh` leaves behind.
# `verify-release-security.sh` refuses to run without one, because it is
# what proves the current repository was actually authenticated.
previous_dir() {
    local dir="$WORK/previous-$1"
    rm -rf "$dir"
    mkdir -p "$dir"
    case "$2" in
        baseline)
            : > "$dir/.baseline-established"
            ;;
        pre-protection)
            : > "$dir/.pre-protection-repository"
            ;;
        empty-repo)
            : > "$dir/.no-existing-repository"
            ;;
        none) ;;
        *) fail "previous_dir: unknown state $2" ;;
    esac
    printf '%s\n' "$dir"
}

export GPG_KEY_ID="$KEY_ID"

V1="1:0.2.0+git100.gaaaaaaaaaaaa"
V2="1:0.2.0+git200.gbbbbbbbbbbbb"

# ---------------------------------------------------------------------------
# 1. A coherent signed set verifies and publishes its metadata.
# ---------------------------------------------------------------------------
REPO="$(fresh_repo repo-good)"
make_deb "$REPO" claw-os-agent "$V2" amd64 alpha
make_deb "$REPO" claw-os-base "$V2" all alpha
BOOTSTRAP_PREVIOUS="$(previous_dir bootstrap pre-protection)"
COS_RELEASE_SECURITY_BOOTSTRAP=1 \
    "$VERIFY" "$REPO" "$SUITE" "$COMPONENT" "$KEYRING" "$BOOTSTRAP_PREVIOUS" >/dev/null \
    || fail "the one-time migration must publish a coherent signed set"
[ -s "$REPO/dists/$SUITE/release-security/claw-os-agent_amd64.json" ] \
    || fail "the agent manifest was not published"
[ -s "$REPO/dists/$SUITE/release-security/claw-os-agent_amd64.json.asc" ] \
    || fail "the agent manifest signature was not published"
[ -s "$REPO/dists/$SUITE/release-security/claw-os-base_all.json" ] \
    || fail "the base manifest was not published"
[ -s "$REPO/dists/$SUITE/release-security/baseline.json" ] \
    || fail "the migration did not establish a baseline marker"
gpgv --keyring "$KEYRING" "$REPO/dists/$SUITE/release-security/baseline.json.asc" \
    "$REPO/dists/$SUITE/release-security/baseline.json" >/dev/null 2>&1 \
    || fail "the baseline marker is not signed by the publishing key"
ok "the one-time migration publishes release-security metadata and a signed baseline"

PREVIOUS="$WORK/previous"
rm -rf "$PREVIOUS"
cp -a "$REPO/dists/$SUITE/release-security" "$PREVIOUS"
: > "$PREVIOUS/.baseline-established"

# ---------------------------------------------------------------------------
# 1b. The baseline is a ratchet.
# ---------------------------------------------------------------------------
REPO="$(fresh_repo repo-rebootstrap)"
make_deb "$REPO" claw-os-agent "$V2" amd64 alpha
if COS_RELEASE_SECURITY_BOOTSTRAP=1 \
    "$VERIFY" "$REPO" "$SUITE" "$COMPONENT" "$KEYRING" "$PREVIOUS" \
    >/dev/null 2>"$WORK/rebootstrap.err"; then
    fail "the one-time migration ran a second time"
fi
grep -q "cannot be run again" "$WORK/rebootstrap.err" \
    || fail "the repeated-migration refusal was not explained"
ok "the release-security migration cannot be run twice"

REPO="$(fresh_repo repo-nobaseline)"
make_deb "$REPO" claw-os-agent "$V2" amd64 alpha
NO_MARKER="$(previous_dir nomarker pre-protection)"
if "$VERIFY" "$REPO" "$SUITE" "$COMPONENT" "$KEYRING" "$NO_MARKER" \
    >/dev/null 2>"$WORK/nobaseline.err"; then
    fail "publication proceeded without a baseline and without the migration input"
fi
grep -q "bootstrap input" "$WORK/nobaseline.err" \
    || fail "the missing-baseline refusal was not explained"
ok "an ordinary publication cannot silently establish a baseline"

UNKNOWN_STATE="$(previous_dir unknown none)"
if "$VERIFY" "$REPO" "$SUITE" "$COMPONENT" "$KEYRING" "$UNKNOWN_STATE" \
    >/dev/null 2>"$WORK/unknown.err"; then
    fail "publication proceeded without knowing the published repository state"
fi
grep -q "never established" "$WORK/unknown.err" \
    || fail "the unknown-state refusal was not explained"
ok "publication refuses when the published repository state is unknown"

REPO="$(fresh_repo repo-lostmarker)"
make_deb "$REPO" claw-os-agent "$V2" amd64 alpha
LOST="$(previous_dir lost baseline)"
if "$VERIFY" "$REPO" "$SUITE" "$COMPONENT" "$KEYRING" "$LOST" \
    >/dev/null 2>"$WORK/lost.err"; then
    fail "publication proceeded with a baseline marker that was not preserved"
fi
grep -q "signed marker was not preserved" "$WORK/lost.err" \
    || fail "the lost-marker refusal was not explained"
ok "publication refuses when the published baseline marker was not retrieved"

# ---------------------------------------------------------------------------
# 2. Unsigned and tampered metadata are refused.
# ---------------------------------------------------------------------------
REPO="$(fresh_repo repo-unsigned)"
make_deb "$REPO" claw-os-agent "$V2" amd64 alpha --unsigned
if "$VERIFY" "$REPO" "$SUITE" "$COMPONENT" "$KEYRING" "$PREVIOUS" >/dev/null 2>"$WORK/unsigned.err"; then
    fail "an unsigned release manifest was published"
fi
grep -q "unsigned release manifest" "$WORK/unsigned.err" \
    || fail "the unsigned refusal was not explained"
ok "publication refuses a package with an unsigned release manifest"

REPO="$(fresh_repo repo-tampered)"
make_deb "$REPO" claw-os-agent "$V2" amd64 alpha --tamper
if "$VERIFY" "$REPO" "$SUITE" "$COMPONENT" "$KEYRING" "$PREVIOUS" >/dev/null 2>"$WORK/tampered.err"; then
    fail "a tampered release manifest was published"
fi
grep -q "does not verify" "$WORK/tampered.err" \
    || fail "the tampered refusal was not explained"
ok "publication refuses a manifest edited after signing"

# ---------------------------------------------------------------------------
# 3. A manifest that does not describe its package is refused.
# ---------------------------------------------------------------------------
REPO="$(fresh_repo repo-mismatch)"
make_deb "$REPO" claw-os-agent "$V2" amd64 alpha
mv "$REPO/pool/$COMPONENT/c/claw-os-agent/claw-os-agent_${V2}_amd64.deb" \
   "$WORK/relabel.deb"
RELABEL="$WORK/relabel"
rm -rf "$RELABEL"
mkdir -p "$RELABEL"
dpkg-deb -R "$WORK/relabel.deb" "$RELABEL"
sed -i "s/^Version: .*/Version: $V1/" "$RELABEL/DEBIAN/control"
dpkg-deb -Znone --root-owner-group --build "$RELABEL" \
    "$REPO/pool/$COMPONENT/c/claw-os-agent/claw-os-agent_${V1}_amd64.deb" >/dev/null
if "$VERIFY" "$REPO" "$SUITE" "$COMPONENT" "$KEYRING" "$PREVIOUS" >/dev/null 2>"$WORK/relabel.err"; then
    fail "a package relabelled to another version was published"
fi
grep -q "manifest names version" "$WORK/relabel.err" \
    || fail "the relabelling refusal was not explained"
ok "publication refuses a package whose manifest names another version"

# ---------------------------------------------------------------------------
# 4. Regressions against what is already published.
# ---------------------------------------------------------------------------
REPO="$(fresh_repo repo-older)"
make_deb "$REPO" claw-os-agent "$V1" amd64 alpha
make_deb "$REPO" claw-os-base "$V2" all alpha
if "$VERIFY" "$REPO" "$SUITE" "$COMPONENT" "$KEYRING" "$PREVIOUS" \
    >/dev/null 2>"$WORK/older.err"; then
    fail "an older version was republished"
fi
grep -q "below the published" "$WORK/older.err" \
    || fail "the version regression was not explained"
ok "publication refuses to republish an older version"

REPO="$(fresh_repo repo-epoch)"
make_deb "$REPO" claw-os-agent "0.2.0+git900.gzzzzzzzzzzzz" amd64 alpha --epoch 0
make_deb "$REPO" claw-os-base "$V2" all alpha
if "$VERIFY" "$REPO" "$SUITE" "$COMPONENT" "$KEYRING" "$PREVIOUS" \
    >/dev/null 2>"$WORK/epoch.err"; then
    fail "a lower security epoch was republished"
fi
grep -q "security epoch" "$WORK/epoch.err" \
    || fail "the epoch regression was not explained"
ok "publication refuses a lower security epoch even at a higher version"

REPO="$(fresh_repo repo-substitute)"
make_deb "$REPO" claw-os-agent "$V2" amd64 "different-bytes"
make_deb "$REPO" claw-os-base "$V2" all alpha
if "$VERIFY" "$REPO" "$SUITE" "$COMPONENT" "$KEYRING" "$PREVIOUS" \
    >/dev/null 2>"$WORK/substitute.err"; then
    fail "a published version was replaced with different content"
fi
grep -q "different content" "$WORK/substitute.err" \
    || fail "the artifact substitution was not explained"
ok "publication refuses to replace a published version with different bytes"

# ---------------------------------------------------------------------------
# 5. Expired metadata and incompatible sets.
# ---------------------------------------------------------------------------
REPO="$(fresh_repo repo-expired)"
make_deb "$REPO" claw-os-agent "$V2" amd64 alpha --issued-at "2000-01-01T00:00:00Z"
if "$VERIFY" "$REPO" "$SUITE" "$COMPONENT" "$KEYRING" "$PREVIOUS" >/dev/null 2>"$WORK/expired.err"; then
    fail "an already expired manifest was published"
fi
grep -q "already expired" "$WORK/expired.err" \
    || fail "the expiry refusal was not explained"
ok "publication refuses metadata that is already expired"

REPO="$(fresh_repo repo-incompatible)"
make_deb "$REPO" claw-os-agent "$V2" amd64 alpha
make_deb "$REPO" claw-os-base "1:0.1.0" all alpha
INCOMPATIBLE_PREVIOUS="$(previous_dir incompatible pre-protection)"
if COS_RELEASE_SECURITY_BOOTSTRAP=1 \
    "$VERIFY" "$REPO" "$SUITE" "$COMPONENT" "$KEYRING" "$INCOMPATIBLE_PREVIOUS" \
    >/dev/null 2>"$WORK/set.err"; then
    fail "a mutually incompatible set was published"
fi
grep -q "or newer" "$WORK/set.err" \
    || { cat "$WORK/set.err" >&2; fail "the compatibility refusal was not explained"; }
ok "publication refuses a mutually incompatible package set"

# ---------------------------------------------------------------------------
# 6. Migration ratchet: a pre-protection artifact is tolerated only while
#    the repository has no baseline, and never once it has one.
# ---------------------------------------------------------------------------
REPO="$(fresh_repo repo-legacy)"
LEGACY="$WORK/legacy-stage"
rm -rf "$LEGACY"
mkdir -p "$LEGACY/DEBIAN" "$LEGACY/usr/share/doc/claw-os-agent"
echo legacy > "$LEGACY/usr/share/doc/claw-os-agent/README"
cat > "$LEGACY/DEBIAN/control" <<EOF
Package: claw-os-agent
Version: $V1
Section: admin
Priority: optional
Architecture: amd64
Maintainer: Claw OS <noreply@github.com>
Description: artifact published before downgrade protection existed
EOF
mkdir -p "$REPO/pool/$COMPONENT/c/claw-os-agent"
dpkg-deb -Znone --root-owner-group --build "$LEGACY" \
    "$REPO/pool/$COMPONENT/c/claw-os-agent/claw-os-agent_${V1}_amd64.deb" >/dev/null
# A pool with nothing but pre-protection artifacts cannot establish a
# baseline: there is no metadata to anchor.
LEGACY_PREVIOUS="$(previous_dir legacy pre-protection)"
if COS_RELEASE_SECURITY_BOOTSTRAP=1 \
    "$VERIFY" "$REPO" "$SUITE" "$COMPONENT" "$KEYRING" "$LEGACY_PREVIOUS" \
    >/dev/null 2>"$WORK/legacy-only.err"; then
    ok "a pool of only pre-protection artifacts is published without a baseline"
else
    fail "a pre-protection pool must still be publishable during migration"
fi

# Add a protected package to the same pool and the migration succeeds,
# establishing the baseline.
make_deb "$REPO" claw-os-base "$V2" all alpha
LEGACY_PREVIOUS="$(previous_dir legacy2 pre-protection)"
COS_RELEASE_SECURITY_BOOTSTRAP=1 \
    "$VERIFY" "$REPO" "$SUITE" "$COMPONENT" "$KEYRING" "$LEGACY_PREVIOUS" >/dev/null 2>&1 \
    || fail "a mixed pre-protection pool must be publishable during migration"
ok "a pre-protection artifact is tolerated during the one-time migration"

if "$VERIFY" "$REPO" "$SUITE" "$COMPONENT" "$KEYRING" "$PREVIOUS" \
    >/dev/null 2>"$WORK/legacy.err"; then
    fail "a manifest-less artifact was accepted after a baseline was established"
fi
grep -q "established baseline" "$WORK/legacy.err" \
    || { cat "$WORK/legacy.err" >&2; fail "the migration-ratchet refusal was not explained"; }
ok "a manifest-less artifact is refused once a baseline exists"

# ---------------------------------------------------------------------------
# 7. Repository freshness metadata.
# ---------------------------------------------------------------------------
grep -Fq 'Valid-Until:' "$BUILD_REPO" \
    || fail "build-repo.sh must set a Release Valid-Until"
grep -Fq 'Acquire-By-Hash' "$BUILD_REPO" \
    || fail "build-repo.sh must publish by-hash indexes"
grep -Fq 'verify-release-security.sh' "$BUILD_REPO" \
    || fail "build-repo.sh must verify release-security metadata before signing"
grep -Fq 'Claw-Os-Release-Security-Baseline: 1' "$BUILD_REPO" \
    || fail "build-repo.sh must advertise the release-security baseline in Release"
grep -Fq 'XB-Claw-Os-Security-Epoch' "$SYNC" \
    || fail "the merge step must compare security epochs"
ok "the repository build wires freshness metadata and verification"

# ---------------------------------------------------------------------------
# 8. Retrieving the published state is fail-closed.
#
#    A transport failure must never be mistaken for "there is no
#    repository yet", because that is exactly the state in which every
#    regression check is skipped.
# ---------------------------------------------------------------------------
SERVE_ROOT="$WORK/served"
rm -rf "$SERVE_ROOT"
mkdir -p "$SERVE_ROOT/dists/$SUITE/release-security"

# A tiny HTTP server whose status codes the test controls.
cat > "$WORK/server.py" <<'PY'
import http.server, os, sys, threading

root, mode, port_file = sys.argv[1], sys.argv[2], sys.argv[3]


class Handler(http.server.SimpleHTTPRequestHandler):
    def translate_path(self, path):
        return os.path.join(root, path.lstrip("/").split("?")[0])

    def do_GET(self):
        if mode == "500":
            self.send_error(500, "server error")
            return
        if mode == "404":
            self.send_error(404, "not found")
            return
        if mode == "missing-manifests" and "/release-security/" in self.path:
            self.send_error(404, "not found")
            return
        super().do_GET()

    def log_message(self, *args):
        pass


server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
with open(port_file, "w", encoding="utf-8") as handle:
    handle.write(str(server.server_address[1]))
threading.Thread(target=server.serve_forever, daemon=True).start()
try:
    while True:
        threading.Event().wait(60)
except KeyboardInterrupt:
    pass
PY

start_server() {
    local mode="$1"
    local port_file="$WORK/port"
    rm -f "$port_file"
    python3 "$WORK/server.py" "$SERVE_ROOT" "$mode" "$port_file" &
    SERVER_PID=$!
    for _ in $(seq 1 50); do
        [ -s "$port_file" ] && break
        sleep 0.1
    done
    [ -s "$port_file" ] || fail "the test HTTP server did not start"
    SERVER_URL="http://127.0.0.1:$(cat "$port_file")"
}

stop_server() {
    [ -n "${SERVER_PID:-}" ] && kill "$SERVER_PID" 2>/dev/null || true
    wait "${SERVER_PID:-0}" 2>/dev/null || true
    SERVER_PID=""
}

run_sync() {
    local previous="$WORK/sync-previous"
    rm -rf "$previous" "$WORK/sync-debs"
    mkdir -p "$WORK/sync-debs"
    EXISTING_APT_REPO_URL="$SERVER_URL" \
        APT_PUBLIC_KEYRING="$KEYRING" \
        SUITE="$SUITE" \
        DEBS_DIR="$WORK/sync-debs" \
        COS_PREVIOUS_RELEASE_SECURITY_DIR="$previous" \
        bash "$SYNC"
}

# A server that is up but erroring is not an empty repository.
start_server 500
if run_sync >"$WORK/sync-500.log" 2>&1; then
    stop_server
    fail "an HTTP 500 was treated as a publishable state"
fi
stop_server
grep -q "Refusing to publish" "$WORK/sync-500.log" \
    || { cat "$WORK/sync-500.log" >&2; fail "the HTTP 500 refusal was not explained"; }
ok "an HTTP error while reading the published repository is fatal"

# A connection failure is not an empty repository either.
SERVER_URL="http://127.0.0.1:1"
if run_sync >"$WORK/sync-refused.log" 2>&1; then
    fail "a connection failure was treated as a publishable state"
fi
grep -q "Refusing to publish" "$WORK/sync-refused.log" \
    || { cat "$WORK/sync-refused.log" >&2; fail "the transport refusal was not explained"; }
ok "a transport failure while reading the published repository is fatal"

# A genuine 404 is the one case that means first publication.
start_server 404
run_sync >"$WORK/sync-404.log" 2>&1 || { cat "$WORK/sync-404.log" >&2; stop_server; \
    fail "a genuine 404 must be treated as first publication"; }
stop_server
[ -e "$WORK/sync-previous/.no-existing-repository" ] \
    || fail "first publication was not recorded"
[ ! -e "$WORK/sync-previous/.baseline-established" ] \
    || fail "first publication must not claim a baseline"
ok "only an authenticated 404 is treated as first publication"

# A repository that advertises a baseline but cannot produce its
# manifests is fatal.
cat > "$SERVE_ROOT/dists/$SUITE/Release" <<EOF
Origin: Claw OS
Suite: $SUITE
Codename: $SUITE
Date: $(date -u -R)
Valid-Until: $(date -u -R -d '+30 days')
Claw-Os-Release-Security-Baseline: 1
Architectures: amd64 all
Components: $COMPONENT
EOF
gpg --batch --yes --pinentry-mode loopback --default-key "$KEY_ID" \
    --clearsign -o "$SERVE_ROOT/dists/$SUITE/InRelease" "$SERVE_ROOT/dists/$SUITE/Release"
start_server missing-manifests
if run_sync >"$WORK/sync-nomanifest.log" 2>&1; then
    stop_server
    fail "a baseline repository with unreadable manifests was accepted"
fi
stop_server
grep -q "baseline" "$WORK/sync-nomanifest.log" \
    || { cat "$WORK/sync-nomanifest.log" >&2; fail "the missing-manifest refusal was not explained"; }
ok "a missing published manifest under an established baseline is fatal"

# An InRelease that does not verify is fatal, even when it is served
# perfectly.
printf 'not a signature\n' > "$SERVE_ROOT/dists/$SUITE/InRelease"
start_server ok
if run_sync >"$WORK/sync-badsig.log" 2>&1; then
    stop_server
    fail "an unverifiable InRelease was accepted"
fi
stop_server
grep -q "does not verify" "$WORK/sync-badsig.log" \
    || { cat "$WORK/sync-badsig.log" >&2; fail "the signature refusal was not explained"; }
ok "an InRelease that does not verify is fatal"

# The `Valid-Until` insertion must be exact and must survive whatever
# this apt version emits, so run the real transformation.
RELEASE_DIR="$WORK/release-check"
mkdir -p "$RELEASE_DIR/dists/$SUITE/$COMPONENT/binary-amd64"
printf 'Package: x\n' > "$RELEASE_DIR/dists/$SUITE/$COMPONENT/binary-amd64/Packages"
cat > "$RELEASE_DIR/release.conf" <<EOF
APT::FTPArchive::Release::Origin "Claw OS";
APT::FTPArchive::Release::Suite "$SUITE";
APT::FTPArchive::Release::Codename "$SUITE";
APT::FTPArchive::Release::Architectures "amd64";
APT::FTPArchive::Release::Components "$COMPONENT";
APT::FTPArchive::Release::Date "$(date -u -R)";
APT::FTPArchive::Release::Acquire-By-Hash "yes";
EOF
VALID_UNTIL="$(date -u -R -d '+30 days')"
( cd "$RELEASE_DIR" && apt-ftparchive -c=release.conf release "dists/$SUITE" \
    > "dists/$SUITE/Release" )
awk -v valid_until="$VALID_UNTIL" '
    /^Valid-Until:/ { next }
    { print }
    /^Date:/ { printf "Valid-Until: %s\n", valid_until }
' "$RELEASE_DIR/dists/$SUITE/Release" > "$RELEASE_DIR/Release.out"
[ "$(grep -c '^Valid-Until:' "$RELEASE_DIR/Release.out")" = "1" ] \
    || fail "the published Release must carry exactly one Valid-Until"
grep -q '^Acquire-By-Hash: yes' "$RELEASE_DIR/Release.out" \
    || fail "the published Release must advertise by-hash indexes"
ok "the published Release carries a Valid-Until bound and by-hash indexes"

# ---------------------------------------------------------------------------
# 18. End-to-end: build -> signed Release/InRelease -> sync -> publish again.
#
# The blocker this covers: `Release` advertised a release-security
# baseline unconditionally, so a repository with no baseline artifact
# still claimed one — and the next publication then demanded an artifact
# that had never been written. A signed, unrecoverable state.
#
# The three stages a real repository goes through are exercised in
# order, each through `build-repo.sh` and a local HTTP origin.
# ---------------------------------------------------------------------------
CYCLE="$WORK/cycle"
mkdir -p "$CYCLE"
export CLAW_OS_WEB_DIST_DIR="$CYCLE/web"
mkdir -p "$CLAW_OS_WEB_DIST_DIR"
echo "<html></html>" > "$CLAW_OS_WEB_DIST_DIR/index.html"

# Serve a built repository so `sync-existing-packages.sh` reads it the
# way the publication workflow does. This reuses the same origin the
# earlier scenarios use, with its output kept off this script's stdout
# so nothing can be left holding the pipeline open.
serve() {
    local origin_root="$1"
    local port_file="$WORK/cycle-port"
    rm -f "$port_file"
    python3 "$WORK/server.py" "$origin_root" ok "$port_file" \
        >"$WORK/cycle-server.log" 2>&1 &
    SERVER_PID=$!
    for _ in $(seq 1 50); do
        [ -s "$port_file" ] && break
        sleep 0.1
    done
    [ -s "$port_file" ] || fail "the fixture origin did not start"
    ORIGIN="http://127.0.0.1:$(cat "$port_file")"
}

build_repo_at() {
    local pool="$1" out="$2" previous="$3" bootstrap="${4:-0}"
    local debs="$out.debs"
    rm -rf "$out" "$debs"
    mkdir -p "$out" "$debs"
    find "$pool/pool" -name '*.deb' -exec cp {} "$debs/" \;
    COS_DEBS_DIR="$debs" \
    COS_APT_REPO_DIR="$out" \
    COS_PREVIOUS_RELEASE_SECURITY_DIR="$previous" \
    COS_RELEASE_SECURITY_BOOTSTRAP="$bootstrap" \
    SUITE="$SUITE" \
    GPG_KEY_ID="$KEY_ID" \
        "$BUILD_REPO" >"$WORK/build-repo.log" 2>&1
}

# The package-merge half of the sync script talks to APT, which on a
# developer machine would answer from the host's own sources. This test
# is about the release-security half — the baseline marker, the
# preserved manifests and the freshness checks — so APT is answered
# with "no candidate" and the merge becomes a no-op.
CYCLE_BIN="$CYCLE/bin"
mkdir -p "$CYCLE_BIN"
cat > "$CYCLE_BIN/apt-cache" <<'EOF'
#!/usr/bin/env bash
printf '  Candidate: (none)\n'
EOF
cat > "$CYCLE_BIN/apt-get" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$CYCLE_BIN/apt-cache" "$CYCLE_BIN/apt-get"

cycle_sync() {
    local previous="$1" debs="$2" log="$3"
    rm -rf "$previous" "$debs"
    mkdir -p "$previous" "$debs"
    PATH="$CYCLE_BIN:$PATH" \
        EXISTING_APT_REPO_URL="$ORIGIN" \
        APT_PUBLIC_KEYRING="$KEYRING" \
        DEBS_DIR="$debs" \
        COS_PREVIOUS_RELEASE_SECURITY_DIR="$previous" \
        SUITE="$SUITE" \
        bash "$SYNC" >"$log" 2>&1
}

# --- stage 1: a pool that predates downgrade protection --------------------
PRE_POOL="$(fresh_repo cycle-pre-pool)"
mkdir -p "$PRE_POOL/pool/$COMPONENT/c/claw-os-agent"
LEGACY="$WORK/stage-legacy"
rm -rf "$LEGACY"
mkdir -p "$LEGACY/DEBIAN" "$LEGACY/usr/share/doc/claw-os-agent"
echo legacy > "$LEGACY/usr/share/doc/claw-os-agent/README"
cat > "$LEGACY/DEBIAN/control" <<EOF
Package: claw-os-agent
Version: 0.1.0
Section: admin
Priority: optional
Architecture: amd64
Maintainer: Claw OS <noreply@github.com>
Description: pre-protection fixture
EOF
dpkg-deb -Znone --root-owner-group --build "$LEGACY" \
    "$PRE_POOL/pool/$COMPONENT/c/claw-os-agent/claw-os-agent_0.1.0_amd64.deb" >/dev/null

PRE_PREVIOUS="$(previous_dir cycle-pre empty-repo)"
# Publishing a pool that carries no release-security metadata is only
# possible through the explicit one-time migration input. An ordinary
# build cannot quietly publish an unprotected repository.
if build_repo_at "$PRE_POOL" "$CYCLE/repo-pre-refused" "$PRE_PREVIOUS" 0; then
    fail "an ordinary build published a repository with no baseline"
fi
grep -q "no release-security baseline yet" "$WORK/build-repo.log" \
    || { cat "$WORK/build-repo.log" >&2; fail "the missing baseline was not explained"; }
build_repo_at "$PRE_POOL" "$CYCLE/repo-pre" "$PRE_PREVIOUS" 1 \
    || { cat "$WORK/build-repo.log" >&2; fail "a pre-protection pool must still publish"; }
grep -q '^Claw-Os-Release-Security-Baseline:' "$CYCLE/repo-pre/dists/$SUITE/Release" \
    && fail "an unprotected repository must not advertise a baseline"
[ -d "$CYCLE/repo-pre/dists/$SUITE/release-security" ] \
    && fail "an unprotected repository must not publish a release-security directory"
test -s "$CYCLE/repo-pre/dists/$SUITE/InRelease" \
    || fail "the pre-protection repository was not signed"
ok "a pre-protection pool publishes signed metadata and claims no baseline"
ok "an ordinary build cannot publish a repository that has no baseline"

# The next publication must read that signed index and see honestly
# that no baseline exists.
serve "$CYCLE/repo-pre"
SYNC_PREVIOUS="$CYCLE/previous-after-pre"
cycle_sync "$SYNC_PREVIOUS" "$CYCLE/sync-debs" "$WORK/sync-pre.log" \
    || { cat "$WORK/sync-pre.log" >&2; fail "reading a pre-protection repository must succeed"; }
[ -f "$SYNC_PREVIOUS/.pre-protection-repository" ] \
    || fail "the publisher did not record that the origin predates protection"
[ ! -f "$SYNC_PREVIOUS/.baseline-established" ] \
    || fail "the publisher invented a baseline that was never published"
stop_server
ok "the next publication reads the signed index and sees no baseline"

# --- stage 2: the one-time migration to a protected repository -------------
PROT_POOL="$(fresh_repo cycle-protected-pool)"
make_deb "$PROT_POOL" claw-os-agent "$V1" amd64 alpha
make_deb "$PROT_POOL" claw-os-base "$V1" all alpha
build_repo_at "$PROT_POOL" "$CYCLE/repo-first" "$SYNC_PREVIOUS" 1 \
    || { cat "$WORK/build-repo.log" >&2; fail "the one-time migration must publish"; }
grep -q '^Claw-Os-Release-Security-Baseline: 1$' "$CYCLE/repo-first/dists/$SUITE/Release" \
    || fail "the first protected publication must advertise its baseline"
test -s "$CYCLE/repo-first/dists/$SUITE/release-security/baseline.json" \
    || fail "the baseline artifact was not written"
gpgv --keyring "$KEYRING" \
    "$CYCLE/repo-first/dists/$SUITE/release-security/baseline.json.asc" \
    "$CYCLE/repo-first/dists/$SUITE/release-security/baseline.json" >/dev/null 2>&1 \
    || fail "the published baseline does not verify"
ok "the first protected publication writes and advertises a signed baseline"

# --- stage 3: an ordinary publication on top of the protected one ----------
serve "$CYCLE/repo-first"
SECOND_PREVIOUS="$CYCLE/previous-after-first"
cycle_sync "$SECOND_PREVIOUS" "$CYCLE/sync-debs2" "$WORK/sync-first.log" \
    || { cat "$WORK/sync-first.log" >&2; fail "reading the protected repository must succeed"; }
[ -f "$SECOND_PREVIOUS/.baseline-established" ] \
    || fail "the established baseline was not carried forward"
test -s "$SECOND_PREVIOUS/baseline.json" \
    || fail "the signed baseline artifact was not preserved"
test -s "$SECOND_PREVIOUS/claw-os-agent_amd64.json" \
    || fail "the published agent manifest was not preserved"
stop_server
ok "a protected repository hands its baseline and manifests to the next publication"

NEXT_POOL="$(fresh_repo cycle-next-pool)"
make_deb "$NEXT_POOL" claw-os-agent "$V2" amd64 beta
make_deb "$NEXT_POOL" claw-os-base "$V2" all beta
build_repo_at "$NEXT_POOL" "$CYCLE/repo-second" "$SECOND_PREVIOUS" 0 \
    || { cat "$WORK/build-repo.log" >&2; fail "an ordinary protected publication must succeed"; }
grep -q '^Claw-Os-Release-Security-Baseline: 1$' "$CYCLE/repo-second/dists/$SUITE/Release" \
    || fail "the baseline marker was dropped by an ordinary publication"
python3 - "$CYCLE/repo-first/dists/$SUITE/release-security/baseline.json" \
    "$CYCLE/repo-second/dists/$SUITE/release-security/baseline.json" <<'PY'
import json, sys
first = json.load(open(sys.argv[1], encoding="utf-8"))
second = json.load(open(sys.argv[2], encoding="utf-8"))
assert first == second, "the baseline marker must be carried forward verbatim"
PY
ok "an ordinary publication preserves the established baseline verbatim"

# The migration cannot be replayed once the marker exists, whatever the
# workflow input says.
if build_repo_at "$NEXT_POOL" "$CYCLE/repo-rebootstrap" "$SECOND_PREVIOUS" 1; then
    fail "the one-time migration ran again on a protected repository"
fi
grep -q "already established" "$WORK/build-repo.log" \
    || { cat "$WORK/build-repo.log" >&2; fail "the repeated migration was not explained"; }
ok "the one-time migration cannot be replayed once a baseline exists"

# A protected origin that stops serving its baseline is fatal, not a
# reason to fall back to first publication.
rm -rf "$CYCLE/repo-broken"
cp -a "$CYCLE/repo-first" "$CYCLE/repo-broken"
rm -f "$CYCLE/repo-broken/dists/$SUITE/release-security/baseline.json"
serve "$CYCLE/repo-broken"
BROKEN_PREVIOUS="$CYCLE/previous-broken"
if cycle_sync "$BROKEN_PREVIOUS" "$CYCLE/sync-debs3" "$WORK/sync-broken.log"; then
    fail "a protected origin missing its baseline was accepted"
fi
stop_server
ok "a protected origin that stops serving its baseline is fatal"

# ---------------------------------------------------------------------------
# 19. Index freshness: a signature is not recency.
# ---------------------------------------------------------------------------
FRESH="$PROJECT_DIR/packaging/apt-repo/check-index-freshness.py"
release_fixture() {
    local target="$1" date="$2" valid_until="$3"
    {
        printf 'Origin: Claw OS\nSuite: %s\n' "$SUITE"
        printf 'Date: %s\n' "$date"
        if [ -n "$valid_until" ]; then
            printf 'Valid-Until: %s\n' "$valid_until"
        fi
    } > "$target"
}

release_fixture "$WORK/fresh-ok" "$(date -u -R)" "$(date -u -R -d '+30 days')"
"$FRESH" "$WORK/fresh-ok" >/dev/null || fail "a current index must be accepted"

release_fixture "$WORK/fresh-future" "$(date -u -R -d '+3 days')" \
    "$(date -u -R -d '+33 days')"
if "$FRESH" "$WORK/fresh-future" >/dev/null 2>"$WORK/fresh-future.err"; then
    fail "a future-dated index was accepted"
fi
grep -q "future" "$WORK/fresh-future.err" || fail "the future date was not explained"

release_fixture "$WORK/fresh-expired" "$(date -u -R -d '-40 days')" \
    "$(date -u -R -d '-10 days')"
if "$FRESH" "$WORK/fresh-expired" >/dev/null 2>"$WORK/fresh-expired.err"; then
    fail "an expired index was accepted"
fi

release_fixture "$WORK/fresh-stale" "$(date -u -R -d '-40 days')" \
    "$(date -u -R -d '+30 days')"
if "$FRESH" "$WORK/fresh-stale" >/dev/null 2>"$WORK/fresh-stale.err"; then
    fail "an index far beyond the freshness policy was accepted"
fi
grep -q "freshness policy" "$WORK/fresh-stale.err" \
    || fail "the staleness refusal was not explained"

release_fixture "$WORK/fresh-nodate" "" ""
sed -i '/^Date:/d' "$WORK/fresh-nodate"
if "$FRESH" "$WORK/fresh-nodate" >/dev/null 2>&1; then
    fail "an undated index was accepted"
fi
ok "the publisher refuses a future-dated, expired, stale or undated index"

# ---------------------------------------------------------------------------
# 20. The signing passphrase never reaches argv.
# ---------------------------------------------------------------------------
ARGV_DIR="$WORK/argv-probe"
mkdir -p "$ARGV_DIR/bin"
cat > "$ARGV_DIR/bin/gpg" <<'EOF'
#!/usr/bin/env bash
# Record the command line exactly as another local process would read
# it out of /proc, then defer to the real gpg.
printf '%s\n' "$*" >> "${ARGV_LOG:?}"
exec "${REAL_GPG:?}" "$@"
EOF
chmod +x "$ARGV_DIR/bin/gpg"
export ARGV_LOG="$ARGV_DIR/argv.log"
export REAL_GPG="$(command -v gpg)"
: > "$ARGV_LOG"

ARGV_POOL="$(fresh_repo argv-pool)"
GPG_PASSPHRASE="correct horse battery staple" \
    PATH="$ARGV_DIR/bin:$PATH" \
    make_deb "$ARGV_POOL" claw-os-agent "$V1" amd64 alpha

ARGV_PREVIOUS="$(previous_dir argv pre-protection)"
GPG_PASSPHRASE="correct horse battery staple" \
    PATH="$ARGV_DIR/bin:$PATH" \
    COS_RELEASE_SECURITY_BOOTSTRAP=1 \
    "$VERIFY" "$ARGV_POOL" "$SUITE" "$COMPONENT" "$KEYRING" "$ARGV_PREVIOUS" >/dev/null

grep -q 'correct horse battery staple' "$ARGV_LOG" \
    && { cat "$ARGV_LOG" >&2; fail "the signing passphrase appeared on a gpg command line"; }
grep -q -- '--passphrase-fd' "$ARGV_LOG" \
    || { cat "$ARGV_LOG" >&2; fail "the passphrase was not handed over on a file descriptor"; }
unset ARGV_LOG REAL_GPG
ok "the signing passphrase is passed on a descriptor, never in argv"

# ---------------------------------------------------------------------------
# 21. Workflow contracts that keep the published metadata fresh and the
#     signing passphrase private.
# ---------------------------------------------------------------------------
WORKFLOWS="$PROJECT_DIR/.github/workflows"
REFRESH="$WORKFLOWS/refresh-apt-metadata.yml"
[ -s "$REFRESH" ] || fail "there is no scheduled metadata refresh workflow"
python3 - "$REFRESH" <<'PY'
import sys

import yaml

document = yaml.safe_load(open(sys.argv[1], encoding="utf-8"))
# PyYAML reads the unquoted `on:` key as the boolean True.
triggers = document.get("on", document.get(True))
assert triggers and "schedule" in triggers, "the refresh must be scheduled"
minute, hour, day, month, weekday = triggers["schedule"][0]["cron"].split()
assert day == "*" and month == "*", "the refresh must not be monthly or later"
assert weekday != "*" or day != "*", "the refresh must have a fixed cadence"

body = open(sys.argv[1], encoding="utf-8").read()
assert "CLAW_OS_APT_SIGNING_PRIVATE_KEY" in body, "the refresh must sign"
assert "exit 1" in body, "a missing signing secret must fail the refresh"
assert "build-repo.sh" in body, "the refresh must rebuild and re-sign metadata"
assert "build-debs.sh" not in body, "a metadata refresh must not build packages"
assert "COS_RELEASE_SECURITY_BOOTSTRAP: \"0\"" in body, (
    "a refresh must never be able to run the one-time migration"
)
assert "Claw-Os-Release-Security-Baseline: 1" in body, (
    "the refresh must verify that it preserved the baseline"
)
PY
ok "a scheduled workflow re-signs repository metadata without rebuilding packages"

# The signing passphrase must never be handed to another process on a
# command line, including through sudo.
for workflow in "$WORKFLOWS"/*.yml; do
    if grep -q 'GPG_PASSPHRASE="\$' "$workflow" \
        && grep -q 'sudo env' "$workflow"; then
        fail "$(basename "$workflow") passes the passphrase through sudo argv"
    fi
done
grep -q -- '--preserve-env=.*GPG_PASSPHRASE' "$WORKFLOWS/publish-desktop-package.yml" \
    || fail "the desktop build must inherit the passphrase, not pass it in argv"
ok "no workflow places the signing passphrase on a command line"

printf '\n%d checks passed\n' "$PASS"
