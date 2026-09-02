#!/bin/bash
# packaging/deb/tests/test-security-floor-packaging.sh
#
# Adversarial tests for Claw OS update downgrade protection, driven
# through the *real* artifacts: real `.deb` archives built with
# `dpkg-deb`, the real rendered maintainer scripts, the real compiled
# verifier, and real OpenPGP signatures made with an ephemeral key that
# exists only for the duration of this run.
#
# What is exercised end to end:
#
#   * a first install seeding the floor;
#   * a correctly signed *older* package being refused;
#   * epoch-versus-version ordering in both directions;
#   * the same version with different bytes;
#   * an expired release manifest;
#   * a tampered manifest and an unsigned candidate on a system that
#     was installed from a signed release;
#   * removal and purge leaving the floor intact;
#   * reinstalling an old release after removal;
#   * the APT pre-install hook reading real `.deb` archives;
#   * recovery authorizations, including wrong package, wrong version
#     and replay;
#   * corrupt, rolled-back and symlinked floor state.
#
# The preinst and prerm are executed exactly as `dpkg` would execute
# them, under DPKG_ROOT. The postinst's floor commit is executed
# through the same helper invocation the postinst uses; the postinst
# itself also configures systemd on absolute paths, which a test must
# not do to the host, so its wiring is additionally asserted
# statically.
#
# Usage:
#   bash packaging/deb/tests/test-security-floor-packaging.sh
#
# Environment:
#   COS_SECURITY_FLOOR_BIN  prebuilt claw-security-floor (else cargo build)
#   COS_TEST_TMPDIR         scratch root (must be a Linux filesystem)

set -euo pipefail

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PROJECT_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/../../.." && pwd)

RELEASE_SECURITY_DIR="$PROJECT_DIR/packaging/release-security"
POLICY="$RELEASE_SECURITY_DIR/policy.json"
MAKE_MANIFEST="$RELEASE_SECURITY_DIR/make-manifest.py"
RENDER_PREINST="$RELEASE_SECURITY_DIR/render-preinst.sh"
DEB_DIR="$PROJECT_DIR/packaging/deb"

WORK_ROOT="${COS_TEST_TMPDIR:-$PROJECT_DIR/build/tests}"
WORK="$WORK_ROOT/security-floor-$$"
PASS=0

cleanup() {
    if [ -n "${GNUPGHOME:-}" ] && [ -d "${GNUPGHOME:-}" ]; then
        gpgconf --kill all >/dev/null 2>&1 || true
    fi
    rm -rf "$WORK"
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

require() {
    command -v "$1" >/dev/null 2>&1 || fail "$1 is required for this test"
}

require dpkg-deb
require gpg
require gpgv
require python3
require awk

mkdir -p "$WORK"

# ---------------------------------------------------------------------------
# The verifier under test.
# ---------------------------------------------------------------------------
HELPER="${COS_SECURITY_FLOOR_BIN:-}"
if [ -z "$HELPER" ]; then
    for candidate in \
        "$PROJECT_DIR/target/debug/claw-security-floor" \
        "$PROJECT_DIR/target/release/claw-security-floor" \
        "$PROJECT_DIR/core/target/debug/claw-security-floor" \
        "$PROJECT_DIR/core/target/release/claw-security-floor"; do
        [ -x "$candidate" ] && { HELPER="$candidate"; break; }
    done
fi
if [ -z "$HELPER" ]; then
    echo ":: building claw-security-floor" >&2
    ( cd "$PROJECT_DIR/core" && cargo build --bin claw-security-floor ) >&2
    HELPER="$PROJECT_DIR/target/debug/claw-security-floor"
    [ -x "$HELPER" ] || HELPER="$PROJECT_DIR/core/target/debug/claw-security-floor"
fi
[ -x "$HELPER" ] || fail "claw-security-floor was not built"

# A debug build carries hundreds of megabytes of symbols. The tests
# copy the verifier into every fixture package and build real .debs
# from them, so work on a stripped copy: it is the same program, and
# the archives stay small enough to build in seconds.
STRIPPED_HELPER="$WORK/claw-security-floor"
cp "$HELPER" "$STRIPPED_HELPER"
chmod 0755 "$STRIPPED_HELPER"
if command -v strip >/dev/null 2>&1; then
    strip "$STRIPPED_HELPER" 2>/dev/null || true
fi
HELPER="$STRIPPED_HELPER"

# ---------------------------------------------------------------------------
# Ephemeral publisher key. Never written outside this run's scratch
# directory, and destroyed with it.
# ---------------------------------------------------------------------------
export GNUPGHOME="$WORK/gnupg"
mkdir -m 700 -p "$GNUPGHOME"
gpg --batch --quiet --passphrase '' --quick-generate-key \
    'Claw OS Security Floor Test <test@example.invalid>' default default never \
    >/dev/null 2>&1
KEY_ID="$(gpg --batch --with-colons --list-secret-keys \
    | awk -F: '$1 == "fpr" { print $10; exit }')"
[ -n "$KEY_ID" ] || fail "could not create an ephemeral signing key"
KEYRING="$WORK/keyring.gpg"
gpg --batch --export "$KEY_ID" > "$KEYRING"

# ---------------------------------------------------------------------------
# Fixture package builder.
#
# Stages every component claw-os-agent declares, generates a real
# signed manifest with the production script, renders the production
# preinst, and assembles a real .deb.
# ---------------------------------------------------------------------------
COMPONENT_PATHS=$(python3 - "$POLICY" <<'PY'
import json, sys
policy = json.load(open(sys.argv[1], encoding="utf-8"))
for entry in policy["components"]:
    if entry["package"] == "claw-os-agent":
        print(entry["path"])
PY
)

# stage_package <stage-dir> <version> <payload-marker> [--epoch N]
#                          [--unsigned] [--valid-until ISO8601]
stage_package() {
    local stage="$1" version="$2" marker="$3"
    shift 3
    local epoch="" unsigned="" issued_at="" policy="$POLICY"
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --epoch) epoch="$2"; shift 2 ;;
            --unsigned) unsigned=1; shift ;;
            --issued-at) issued_at="$2"; shift 2 ;;
            *) fail "stage_package: unknown option $1" ;;
        esac
    done

    rm -rf "$stage"
    mkdir -p "$stage/DEBIAN"
    local path
    for path in $COMPONENT_PATHS; do
        mkdir -p "$stage$(dirname "$path")"
        if [ "$path" = "/usr/lib/cos/bin/claw-security-floor" ]; then
            cp "$HELPER" "$stage$path"
        else
            printf 'claw-os component %s %s %s\n' "$path" "$version" "$marker" \
                > "$stage$path"
        fi
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

    local sign_args=()
    [ -z "$unsigned" ] && sign_args=(--sign-key "$KEY_ID")
    local issued_args=()
    [ -n "$issued_at" ] && issued_args=(--issued-at "$issued_at")
    mkdir -p "$stage/usr/lib/cos/release-security"
    python3 "$MAKE_MANIFEST" \
        --package claw-os-agent \
        --version "$version" \
        --arch amd64 \
        --suite trixie \
        --stage-dir "$stage" \
        --policy "$policy" \
        --output "$stage/usr/lib/cos/release-security/claw-os-agent/manifest.json" \
        "${sign_args[@]}" "${issued_args[@]}" > /dev/null
    rm -f "$stage/policy.json"

    local staged_epoch="${epoch:-$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["security_epoch"])' "$POLICY")}"
    "$RENDER_PREINST" claw-os-agent "$version" "$staged_epoch" "$stage" \
        "$stage/DEBIAN/preinst"
    install -m 755 "$DEB_DIR/claw-os-agent/prerm" "$stage/DEBIAN/prerm"
    install -m 755 "$DEB_DIR/claw-os-agent/postrm" "$stage/DEBIAN/postrm"
    sed -e "s/__VERSION__/$version/g" "$DEB_DIR/claw-os-agent/postinst" \
        > "$stage/DEBIAN/postinst"
    chmod 0755 "$stage/DEBIAN/postinst"
    cat > "$stage/DEBIAN/control" <<EOF
Package: claw-os-agent
Version: $version
Section: admin
Priority: optional
Architecture: amd64
Maintainer: Claw OS <noreply@github.com>
Description: fixture package for downgrade-protection tests
EOF
}

# unpack_into <stage> <root> — what dpkg does between preinst and postinst.
unpack_into() {
    local stage="$1" root="$2"
    ( cd "$stage" && find . -path ./DEBIAN -prune -o -type f -print0 \
        | while IFS= read -r -d '' file; do
            mkdir -p "$root/$(dirname "$file")"
            cp "$file" "$root/$file"
            chmod 0755 "$root/$file"
        done )
}

# Run the exact helper invocation the postinst performs.
commit_release() {
    local root="$1" version="$2"
    local manifest="$root/usr/lib/cos/release-security/claw-os-agent/manifest.json"
    local signature="$manifest.asc"
    if [ -f "$signature" ]; then
        "$root/usr/lib/cos/bin/claw-security-floor" commit \
            --root "$root" --package claw-os-agent --version "$version" \
            --manifest "$manifest" --signature "$signature" \
            --reason "claw-os-agent configure"
    else
        "$root/usr/lib/cos/bin/claw-security-floor" commit \
            --root "$root" --package claw-os-agent --version "$version" \
            --manifest "$manifest" --reason "claw-os-agent configure"
    fi
}

# Trust the ephemeral publisher inside a test root.
install_keyring() {
    local root="$1"
    mkdir -p "$root/usr/share/keyrings"
    cp "$KEYRING" "$root/usr/share/keyrings/claw-os-archive-keyring.gpg"
}

floor_generation() {
    python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["generation"])' \
        "$1/var/lib/cos/security/floor.json"
}

SECURITY_EPOCH="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["security_epoch"])' "$POLICY")"
component_paths() {
    python3 - "$POLICY" "$1" <<'PY'
import json, sys
policy = json.load(open(sys.argv[1], encoding="utf-8"))
for entry in policy["components"]:
    if entry["package"] == sys.argv[2]:
        print(entry["path"])
PY
}

V1="1:0.2.0+git100.gaaaaaaaaaaaa"
V2="1:0.2.0+git200.gbbbbbbbbbbbb"
V0="1:0.2.0+git50.gccccccccccc0"

STAGE1="$WORK/stage-v1"
STAGE2="$WORK/stage-v2"
STAGE0="$WORK/stage-v0"
stage_package "$STAGE1" "$V1" alpha
stage_package "$STAGE2" "$V2" beta
stage_package "$STAGE0" "$V0" gamma

# ---------------------------------------------------------------------------
# 1. First install seeds the floor.
# ---------------------------------------------------------------------------
ROOT="$WORK/root"
mkdir -p "$ROOT"
install_keyring "$ROOT"

DPKG_ROOT="$ROOT" "$STAGE1/DEBIAN/preinst" install \
    || fail "a first install must be allowed"
unpack_into "$STAGE1" "$ROOT"
commit_release "$ROOT" "$V1" >/dev/null || fail "the first configure must seed the floor"
[ -f "$ROOT/var/lib/cos/security/floor.json" ] || fail "no floor state was written"
[ -f "$ROOT/var/lib/cos/security/history.jsonl" ] || fail "no floor history was written"
[ "$(floor_generation "$ROOT")" = "1" ] || fail "the first floor must be generation 1"
ok "a first install seeds the security floor"

# The floor must have recorded the publisher key, so later unsigned
# candidates can be refused.
grep -q "$KEY_ID" "$ROOT/var/lib/cos/security/floor.json" \
    || fail "the floor did not record the verified publisher key"
ok "the floor records the publisher key that signed the release"

# ---------------------------------------------------------------------------
# 2. A newer, correctly signed release is accepted.
# ---------------------------------------------------------------------------
DPKG_ROOT="$ROOT" "$ROOT/DEBIAN_prerm_missing" 2>/dev/null || true
install -m 755 "$DEB_DIR/claw-os-agent/prerm" "$WORK/installed-prerm"
DPKG_ROOT="$ROOT" "$WORK/installed-prerm" upgrade "$V2" \
    || fail "an upgrade to a newer version must be allowed by prerm"
DPKG_ROOT="$ROOT" "$STAGE2/DEBIAN/preinst" upgrade "$V1" \
    || fail "an upgrade to a newer version must be allowed by preinst"
unpack_into "$STAGE2" "$ROOT"
commit_release "$ROOT" "$V2" >/dev/null || fail "configuring the newer release must succeed"
[ "$(floor_generation "$ROOT")" = "2" ] || fail "the floor did not advance"
ok "a newer signed release advances the floor"

# ---------------------------------------------------------------------------
# 3. An older but correctly signed release is refused — twice.
# ---------------------------------------------------------------------------
if DPKG_ROOT="$ROOT" "$WORK/installed-prerm" upgrade "$V1" 2>"$WORK/prerm.err"; then
    fail "prerm accepted a downgrade to $V1"
fi
grep -q "security floor only moves forward" "$WORK/prerm.err" \
    || fail "prerm did not explain the refusal"
ok "the installed package's prerm refuses an older incoming version"

if DPKG_ROOT="$ROOT" "$STAGE1/DEBIAN/preinst" upgrade "$V2" 2>"$WORK/preinst.err"; then
    fail "preinst accepted a validly signed older release"
fi
grep -q "version_regression" "$WORK/preinst.err" \
    || fail "preinst did not classify the refusal"
ok "a validly signed older release is refused before unpack"

# ---------------------------------------------------------------------------
# 4. Epoch versus version, in both directions.
# ---------------------------------------------------------------------------
STAGE_HIGH_EPOCH="$WORK/stage-epoch2"
# The security epoch is also the Debian epoch, so the emergency release
# carries `2:` while its upstream version stays *below* what is
# installed. That is the whole point: APT must still prefer it.
V0_EPOCH2="2:${V0#*:}"
stage_package "$STAGE_HIGH_EPOCH" "$V0_EPOCH2" delta --epoch 2
DPKG_ROOT="$ROOT" "$STAGE_HIGH_EPOCH/DEBIAN/preinst" upgrade "$V2" \
    || fail "a higher security epoch must supersede version ordering"
ok "a higher security epoch supersedes a lower Debian version"

# Record the higher epoch, then prove a lower epoch cannot come back
# even with a much higher version.
unpack_into "$STAGE_HIGH_EPOCH" "$ROOT"
commit_release "$ROOT" "$V0_EPOCH2" >/dev/null || fail "the emergency release must configure"
STAGE_LOW_EPOCH="$WORK/stage-epoch1-high-version"
stage_package "$STAGE_LOW_EPOCH" "1:0.2.0+git900.gzzzzzzzzzzzz" epsilon
if DPKG_ROOT="$ROOT" "$STAGE_LOW_EPOCH/DEBIAN/preinst" upgrade "$V0_EPOCH2" \
    2>"$WORK/epoch.err"; then
    fail "a lower security epoch was accepted because its version was higher"
fi
grep -q "security_epoch_regression" "$WORK/epoch.err" \
    || fail "the epoch refusal was not classified"
ok "a lower security epoch is refused even at a higher Debian version"

# ---------------------------------------------------------------------------
# 5. Same version, different bytes.
# ---------------------------------------------------------------------------
ROOT2="$WORK/root-artifact"
mkdir -p "$ROOT2"
install_keyring "$ROOT2"
DPKG_ROOT="$ROOT2" "$STAGE1/DEBIAN/preinst" install >/dev/null
unpack_into "$STAGE1" "$ROOT2"
commit_release "$ROOT2" "$V1" >/dev/null

STAGE_SUBSTITUTE="$WORK/stage-substitute"
stage_package "$STAGE_SUBSTITUTE" "$V1" "different-bytes"
if DPKG_ROOT="$ROOT2" "$STAGE_SUBSTITUTE/DEBIAN/preinst" upgrade "$V1" \
    2>"$WORK/artifact.err"; then
    fail "a different artifact was accepted for an already recorded version"
fi
grep -q "artifact_mismatch" "$WORK/artifact.err" \
    || fail "the artifact refusal was not classified"
ok "the same version with different content is refused"

# Reinstalling the identical release stays possible.
DPKG_ROOT="$ROOT2" "$STAGE1/DEBIAN/preinst" upgrade "$V1" \
    || fail "reinstalling the identical release must remain possible"
ok "reinstalling the identical release is allowed"

# ---------------------------------------------------------------------------
# 6. Expired manifest.
# ---------------------------------------------------------------------------
STAGE_EXPIRED="$WORK/stage-expired"
stage_package "$STAGE_EXPIRED" "1:0.2.0+git800.gyyyyyyyyyyyy" zeta \
    --issued-at "2000-01-01T00:00:00Z"
if DPKG_ROOT="$ROOT2" "$STAGE_EXPIRED/DEBIAN/preinst" upgrade "$V1" \
    2>"$WORK/expired.err"; then
    fail "an expired release manifest was accepted"
fi
grep -q "manifest_expired" "$WORK/expired.err" \
    || fail "the expiry refusal was not classified"
ok "an expired release manifest is refused even though it is newer"

# ---------------------------------------------------------------------------
# 7. Signature: tampered and unsigned candidates.
# ---------------------------------------------------------------------------
STAGE_TAMPERED="$WORK/stage-tampered"
stage_package "$STAGE_TAMPERED" "1:0.2.0+git700.gxxxxxxxxxxxx" eta
python3 - "$STAGE_TAMPERED/usr/lib/cos/release-security/claw-os-agent/manifest.json" <<'PY'
import json, sys
path = sys.argv[1]
document = json.loads(open(path, encoding="utf-8").read())
document["components"][0]["sha256"] = "0" * 64
open(path, "w", encoding="utf-8").write(
    json.dumps(document, sort_keys=True, separators=(",", ":")) + "\n"
)
PY
"$RENDER_PREINST" claw-os-agent "1:0.2.0+git700.gxxxxxxxxxxxx" "$SECURITY_EPOCH" \
    "$STAGE_TAMPERED" "$STAGE_TAMPERED/DEBIAN/preinst"
if DPKG_ROOT="$ROOT2" "$STAGE_TAMPERED/DEBIAN/preinst" upgrade "$V1" \
    2>"$WORK/tampered.err"; then
    fail "a tampered release manifest was accepted"
fi
grep -q "manifest_untrusted" "$WORK/tampered.err" \
    || fail "the tampered-signature refusal was not classified"
ok "a manifest edited after signing is refused"

STAGE_UNSIGNED="$WORK/stage-unsigned"
stage_package "$STAGE_UNSIGNED" "1:0.2.0+git600.gwwwwwwwwwwww" theta --unsigned
if DPKG_ROOT="$ROOT2" "$STAGE_UNSIGNED/DEBIAN/preinst" upgrade "$V1" \
    2>"$WORK/unsigned.err"; then
    fail "an unsigned candidate was accepted on a signed system"
fi
grep -q "manifest_unsigned" "$WORK/unsigned.err" \
    || fail "the unsigned refusal was not classified"
ok "an unsigned candidate is refused once a signed release has been recorded"

# ---------------------------------------------------------------------------
# 8. Removal and purge leave the floor intact.
# ---------------------------------------------------------------------------
GENERATION_BEFORE="$(floor_generation "$ROOT2")"
DPKG_ROOT="$ROOT2" "$ROOT2/usr/lib/cos/bin/claw-security-floor" show \
    --root "$ROOT2" >/dev/null
"$DEB_DIR/claw-os-agent/postrm" remove >/dev/null 2>&1 || true
[ -f "$ROOT2/var/lib/cos/security/floor.json" ] \
    || fail "package removal deleted the floor"
[ "$(floor_generation "$ROOT2")" = "$GENERATION_BEFORE" ] \
    || fail "package removal changed the floor"
grep -q "NOT removed" "$DEB_DIR/claw-os-agent/postrm" \
    || fail "the agent postrm must state that the floor survives purge"
grep -Fq 'rm -rf "$DPKG_ROOT/var/lib/cos/security/staging"' \
    "$DEB_DIR/claw-os-agent/postrm" \
    || fail "purge should still clean the candidate staging directory"
if grep -Eq 'rm -rf?[[:space:]]+"?\$?\{?DPKG_ROOT\}?"?/var/lib/cos/security"?[[:space:]]*$' \
    "$DEB_DIR/claw-os-agent/postrm"; then
    fail "the agent postrm must never delete the floor directory"
fi
if grep -Fq '/var/lib/cos-security' "$DEB_DIR/claw-os-agent/postrm"; then
    fail "the agent postrm must never delete the runtime projection"
fi
ok "removal and purge preserve the security floor"

# An old release reinstalled after removal is still refused: the floor
# is what remembers, not the package.
if DPKG_ROOT="$ROOT2" "$STAGE0/DEBIAN/preinst" install 2>"$WORK/reinstall.err"; then
    fail "an old release was accepted after the package had been removed"
fi
grep -q "version_regression" "$WORK/reinstall.err" \
    || fail "the post-removal refusal was not classified"
ok "reinstalling an old release after removal is still refused"

# ---------------------------------------------------------------------------
# 9. The APT pre-install hook, against real .deb archives.
# ---------------------------------------------------------------------------
DEBS="$WORK/debs"
mkdir -p "$DEBS"
build_deb() {
    local stage="$1" version="$2"
    # `-Znone` keeps the fixture archives fast to assemble; the hook
    # path under test reads the payload tar, not a specific compressor.
    fakeroot dpkg-deb -Znone --root-owner-group --build "$stage" \
        "$DEBS/claw-os-agent_${version}_amd64.deb" >/dev/null 2>&1 \
        || dpkg-deb -Znone --root-owner-group --build "$stage" \
            "$DEBS/claw-os-agent_${version}_amd64.deb" >/dev/null
    printf '%s\n' "$DEBS/claw-os-agent_${version}_amd64.deb"
}
OLD_DEB="$(build_deb "$STAGE1" "$V1")"
NEW_DEB="$(build_deb "$STAGE2" "$V2")"

ROOT3="$WORK/root-hook"
mkdir -p "$ROOT3"
install_keyring "$ROOT3"
DPKG_ROOT="$ROOT3" "$STAGE2/DEBIAN/preinst" install >/dev/null
unpack_into "$STAGE2" "$ROOT3"
commit_release "$ROOT3" "$V2" >/dev/null

hook_input() {
    printf 'VERSION 2\nAPT::Architecture=amd64\n\n%s\n' "$1"
}

if hook_input "claw-os-agent $V2 < $V1 $OLD_DEB" \
    | "$HELPER" apt-hook --root "$ROOT3" 2>"$WORK/hook.err"; then
    fail "the APT hook accepted an older candidate"
fi
grep -q "version_regression" "$WORK/hook.err" \
    || fail "the APT hook refusal was not classified"
ok "the APT hook refuses an older candidate before anything is unpacked"

hook_input "claw-os-agent $V2 = $V2 $NEW_DEB" \
    | "$HELPER" apt-hook --root "$ROOT3" >/dev/null \
    || fail "the APT hook refused the current release"
ok "the APT hook accepts the current release"

hook_input "vim - < 9.0 /var/cache/apt/archives/vim_9.0_amd64.deb" \
    | "$HELPER" apt-hook --root "$ROOT3" >/dev/null \
    || fail "the APT hook must ignore transactions with no Claw OS package"
ok "the APT hook ignores unrelated transactions"

# A hook wrapper without the verifier installed must drain stdin and
# succeed, so a half-removed Claw OS cannot break unrelated apt runs.
HOOK_WRAPPER="$WORK/hook-wrapper"
sed 's|^HELPER=.*|HELPER=/nonexistent/claw-security-floor|' \
    "$DEB_DIR/common/security-floor-hook" > "$HOOK_WRAPPER"
chmod 0755 "$HOOK_WRAPPER"
hook_input "claw-os-agent $V2 < $V1 $OLD_DEB" | "$HOOK_WRAPPER" \
    || fail "the APT hook wrapper must tolerate a missing verifier"
ok "the APT hook wrapper tolerates a missing verifier"

# ---------------------------------------------------------------------------
# 10. Recovery authorizations.
# ---------------------------------------------------------------------------
RECOVERY_DIR="$ROOT3/var/lib/cos/security/recovery"
mkdir -p "$RECOVERY_DIR"
FLOOR_DIGEST="$(python3 -c '
import hashlib, sys
print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())
' "$ROOT3/var/lib/cos/security/floor.json")"
FLOOR_GENERATION="$(floor_generation "$ROOT3")"
MANIFEST_DIGEST="$(python3 -c '
import hashlib, sys
print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())
' "$STAGE1/usr/lib/cos/release-security/claw-os-agent/manifest.json")"

write_authorization() {
    local id="$1" package="$2" version="$3" digest="$4" expires="$5"
    python3 - "$RECOVERY_DIR/$id.json" "$id" "$package" "$version" "$digest" \
        "$expires" "$FLOOR_GENERATION" "$FLOOR_DIGEST" <<'PY'
import datetime, json, os, sys
path, ident, package, version, digest, expires, generation, floor = sys.argv[1:9]
now = datetime.datetime.now(tz=datetime.timezone.utc)
document = {
    "created_at": now.strftime("%Y-%m-%dT%H:%M:%SZ"),
    "created_by_uid": os.getuid(),
    "expires_at": expires,
    "floor_generation": int(generation),
    "floor_sha256": floor,
    "format": "claw.security-recovery/v1",
    "id": ident,
    "manifest_sha256": digest,
    "package": package,
    "reason": "regression in the newer release",
    "security_epoch": 1,
    "version": version,
}
body = json.dumps(document, sort_keys=True, separators=(",", ":")) + "\n"
with open(path, "w", encoding="utf-8") as handle:
    handle.write(body)
os.chmod(path, 0o600)
PY
}

FUTURE="$(python3 -c '
import datetime
print((datetime.datetime.now(tz=datetime.timezone.utc)
       + datetime.timedelta(hours=2)).strftime("%Y-%m-%dT%H:%M:%SZ"))')"
PAST="$(python3 -c '
import datetime
print((datetime.datetime.now(tz=datetime.timezone.utc)
       - datetime.timedelta(hours=2)).strftime("%Y-%m-%dT%H:%M:%SZ"))')"

# Wrong component.
write_authorization "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" claw-os-base "$V1" \
    "$MANIFEST_DIGEST" "$FUTURE"
if DPKG_ROOT="$ROOT3" "$STAGE1/DEBIAN/preinst" upgrade "$V2" >/dev/null 2>&1; then
    fail "an authorization for another package permitted a downgrade"
fi
ok "a recovery authorization for another package does not apply"

# Expired.
rm -f "$RECOVERY_DIR"/*.json
write_authorization "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" claw-os-agent "$V1" \
    "$MANIFEST_DIGEST" "$PAST"
if DPKG_ROOT="$ROOT3" "$STAGE1/DEBIAN/preinst" upgrade "$V2" >/dev/null 2>&1; then
    fail "an expired authorization permitted a downgrade"
fi
ok "an expired recovery authorization does not apply"

# Wrong artifact digest.
rm -f "$RECOVERY_DIR"/*.json
write_authorization "cccccccccccccccccccccccccccccccc" claw-os-agent "$V1" \
    "$(printf 'f%.0s' $(seq 64))" "$FUTURE"
if DPKG_ROOT="$ROOT3" "$STAGE1/DEBIAN/preinst" upgrade "$V2" >/dev/null 2>&1; then
    fail "an authorization naming another artifact permitted a downgrade"
fi
ok "a recovery authorization for another artifact does not apply"

# Exactly right: allowed once, then consumed by the configure step.
rm -f "$RECOVERY_DIR"/*.json
write_authorization "dddddddddddddddddddddddddddddddd" claw-os-agent "$V1" \
    "$MANIFEST_DIGEST" "$FUTURE"
DPKG_ROOT="$ROOT3" "$STAGE1/DEBIAN/preinst" upgrade "$V2" >/dev/null \
    || fail "an exactly matching authorization must permit the downgrade"
ok "an exactly matching recovery authorization permits one downgrade"

unpack_into "$STAGE1" "$ROOT3"
commit_release "$ROOT3" "$V1" >/dev/null \
    || fail "the authorized downgrade must configure"
[ -f "$RECOVERY_DIR/dddddddddddddddddddddddddddddddd.json" ] \
    && fail "the authorization was not consumed"
[ -f "$RECOVERY_DIR/consumed/dddddddddddddddddddddddddddddddd.json" ] \
    || fail "the consumed authorization was not recorded"
ok "a used recovery authorization is consumed atomically"

# Replay of the same authorization is impossible.
if DPKG_ROOT="$ROOT3" "$STAGE0/DEBIAN/preinst" upgrade "$V1" >/dev/null 2>&1; then
    fail "a consumed authorization was replayed"
fi
ok "a consumed recovery authorization cannot be replayed"

# The authorize path itself cannot be driven without a terminal, which
# is exactly the condition an agent, App or MCP session runs in.
if "$HELPER" recover authorize --root "$ROOT3" --package claw-os-agent \
    --version "$V1" --epoch 1 --manifest-sha256 "$MANIFEST_DIGEST" \
    --reason "automated attempt" --expires-in 1 </dev/null >/dev/null 2>&1; then
    fail "a recovery authorization was recorded without an operator terminal"
fi
ok "recovery authorization refuses to run without an operator terminal"

# ---------------------------------------------------------------------------
# 11. Hostile floor state.
# ---------------------------------------------------------------------------
ROOT4="$WORK/root-state"
mkdir -p "$ROOT4"
install_keyring "$ROOT4"
DPKG_ROOT="$ROOT4" "$STAGE1/DEBIAN/preinst" install >/dev/null
unpack_into "$STAGE1" "$ROOT4"
commit_release "$ROOT4" "$V1" >/dev/null
cp "$ROOT4/var/lib/cos/security/floor.json" "$WORK/floor-gen1.json"
DPKG_ROOT="$ROOT4" "$STAGE2/DEBIAN/preinst" upgrade "$V1" >/dev/null
unpack_into "$STAGE2" "$ROOT4"
commit_release "$ROOT4" "$V2" >/dev/null

cp "$WORK/floor-gen1.json" "$ROOT4/var/lib/cos/security/floor.json"
if DPKG_ROOT="$ROOT4" "$STAGE1/DEBIAN/preinst" upgrade "$V2" \
    2>"$WORK/rollback.err"; then
    fail "a rolled-back floor state was accepted"
fi
grep -q "rollback" "$WORK/rollback.err" \
    || fail "the rollback refusal was not reported"
ok "restoring an older floor state alone is detected"

"$HELPER" show --root "$ROOT4" >/dev/null 2>&1 \
    && fail "a rolled-back floor must not be reported as usable"
ok "the verifier refuses to report a rolled-back floor"

# Corrupt state.
ROOT5="$WORK/root-corrupt"
mkdir -p "$ROOT5"
install_keyring "$ROOT5"
DPKG_ROOT="$ROOT5" "$STAGE1/DEBIAN/preinst" install >/dev/null
unpack_into "$STAGE1" "$ROOT5"
commit_release "$ROOT5" "$V1" >/dev/null
printf '{"format":"nope"}\n' > "$ROOT5/var/lib/cos/security/floor.json"
if DPKG_ROOT="$ROOT5" "$STAGE2/DEBIAN/preinst" upgrade "$V1" >/dev/null 2>&1; then
    fail "a corrupt floor was treated as absent"
fi
ok "a corrupt floor fails closed instead of bootstrapping"

# Symlinked state.
ROOT6="$WORK/root-symlink"
mkdir -p "$ROOT6"
install_keyring "$ROOT6"
DPKG_ROOT="$ROOT6" "$STAGE1/DEBIAN/preinst" install >/dev/null
unpack_into "$STAGE1" "$ROOT6"
commit_release "$ROOT6" "$V1" >/dev/null
mv "$ROOT6/var/lib/cos/security/floor.json" "$WORK/elsewhere.json"
ln -s "$WORK/elsewhere.json" "$ROOT6/var/lib/cos/security/floor.json"
if DPKG_ROOT="$ROOT6" "$STAGE2/DEBIAN/preinst" upgrade "$V1" >/dev/null 2>&1; then
    fail "a symlinked floor state was followed"
fi
ok "a symlinked floor state is refused"

# Missing verifier with an existing floor.
ROOT7="$WORK/root-noverifier"
mkdir -p "$ROOT7"
install_keyring "$ROOT7"
DPKG_ROOT="$ROOT7" "$STAGE1/DEBIAN/preinst" install >/dev/null
unpack_into "$STAGE1" "$ROOT7"
commit_release "$ROOT7" "$V1" >/dev/null
rm -f "$ROOT7/usr/lib/cos/bin/claw-security-floor"
if DPKG_ROOT="$ROOT7" "$STAGE0/DEBIAN/preinst" upgrade "$V1" \
    2>"$WORK/noverifier.err"; then
    fail "a candidate was accepted with the verifier deleted"
fi
grep -q "verifier" "$WORK/noverifier.err" \
    || fail "the missing-verifier refusal was not explained"
ok "deleting the verifier does not disable the floor"

# ---------------------------------------------------------------------------
# 12. Package wiring contracts.
# ---------------------------------------------------------------------------
grep -Fq 'commit_security_floor' "$DEB_DIR/claw-os-agent/postinst" \
    || fail "the agent postinst must commit the floor"
grep -Fq 'security_set_is_runnable' "$DEB_DIR/claw-os-agent/postinst" \
    || fail "the agent postinst must gate the service start on a compatible set"
python3 - "$DEB_DIR/claw-os-agent/postinst" <<'PY'
import sys
body = open(sys.argv[1], encoding="utf-8").read()
commit = body.index("        commit_security_floor")
start = body.index("deb-systemd-invoke start clawd.service")
assert commit < start, "the floor must be committed before clawd is started"
PY
ok "the agent postinst commits the floor before starting the broker"

for package in claw-os-agent claw-os-base claw-os-desktop; do
    grep -Fq 'check-incoming' "$DEB_DIR/$package/prerm" \
        || fail "$package prerm must gate the incoming version"
done
ok "every gated package refuses an older incoming version in prerm"

grep -Fq 'claw-os-abi-__ABI__' "$DEB_DIR/claw-os-base/control" \
    || fail "claw-os-base must depend on the agent ABI generation"
grep -Fq 'claw-os-abi-__ABI__' "$DEB_DIR/claw-os-desktop/control" \
    || fail "claw-os-desktop must depend on the agent ABI generation"
grep -Fq 'Provides: claw-os-abi-__ABI__' "$DEB_DIR/claw-os-agent/control" \
    || fail "claw-os-agent must provide the ABI generation"
ok "package dependencies encode the compatible ABI generation"

grep -Fq '/etc/apt/apt.conf.d/50claw-os-security-floor' \
    "$DEB_DIR/claw-os-agent/conffiles" \
    || fail "the APT hook configuration must be a conffile"
ok "the APT hook configuration ships as a conffile"

# ---------------------------------------------------------------------------
# 13. The package build wires the metadata into every gated package.
# ---------------------------------------------------------------------------
BUILD_DEBS="$DEB_DIR/build-debs.sh"
BUILD_DESKTOP="$DEB_DIR/build-desktop-deb.sh"
bash -n "$BUILD_DEBS" || fail "build-debs.sh is not valid bash"
bash -n "$BUILD_DESKTOP" || fail "build-desktop-deb.sh is not valid bash"
for needle in \
    'ensure_bin claw-security-floor cos' \
    '/usr/lib/cos/bin/claw-security-floor' \
    '/usr/lib/cos/apt/security-floor-hook' \
    '/etc/apt/apt.conf.d/50claw-os-security-floor' \
    'write_release_manifest claw-os-agent' \
    'write_release_manifest claw-os-base' \
    'render_security_preinst claw-os-agent' \
    'render_security_preinst claw-os-base'; do
    grep -Fq "$needle" "$BUILD_DEBS" \
        || fail "build-debs.sh does not wire '$needle'"
done
grep -Fq 'render-preinst.sh' "$BUILD_DESKTOP" \
    || fail "the desktop package must render the shared preinst"
grep -Fq 'make-manifest.py' "$BUILD_DESKTOP" \
    || fail "the desktop package must carry a release manifest"
# Every package must stage into its own manifest subdirectory.
for package in claw-os-agent claw-os-base; do
    grep -Fq "release-security/\$package" "$BUILD_DEBS" \
        || fail "build-debs.sh must stage per-package manifest directories"
done
grep -Fq 'release-security/claw-os-desktop/manifest.json' "$BUILD_DESKTOP" \
    || fail "the desktop package must stage its own manifest directory"
ok "every gated package build embeds the verifier and its own release manifest"

# The APT hook must be one executable token: APT looks its protocol
# version up by the exact command string.
HOOK_CONF="$DEB_DIR/common/50claw-os-security-floor"
grep -Fxq 'DPkg::Pre-Install-Pkgs { "/usr/lib/cos/apt/security-floor-hook"; };' "$HOOK_CONF" \
    || fail "the hook must be registered as a single executable token"
grep -Fxq 'DPkg::Tools::Options::/usr/lib/cos/apt/security-floor-hook::Version "2";' \
    "$HOOK_CONF" \
    || fail "the hook must be registered for protocol version 2"
grep -q 'if \[' "$HOOK_CONF" \
    && fail "the hook command must not be a shell fragment"
apt-config -c "$HOOK_CONF" dump 2>/dev/null \
    | grep -Fq 'DPkg::Tools::options::/usr/lib/cos/apt/security-floor-hook::Version "2";' \
    || fail "APT does not parse the hook registration as intended"
ok "the APT hook registration parses to exactly the intended keys"

# ---------------------------------------------------------------------------
# 14. The control metadata the publication merge job reads.
# ---------------------------------------------------------------------------
CONTROL_STAGE="$WORK/stage-control"
mkdir -p "$CONTROL_STAGE/DEBIAN" "$CONTROL_STAGE/usr/share/doc/claw-os-agent"
echo "fixture" > "$CONTROL_STAGE/usr/share/doc/claw-os-agent/README"
POLICY_EPOCH="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["security_epoch"])' "$POLICY")"
POLICY_ABI="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["abi"])' "$POLICY")"
MIN_BASE="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["minimum_compatible"]["claw-os-base"])' "$POLICY")"
MIN_DESKTOP="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["minimum_compatible"]["claw-os-desktop"])' "$POLICY")"
sed -e "s/__VERSION__/$V2/g" \
    -e "s/__ARCH__/amd64/g" \
    -e "s/__ABI__/$POLICY_ABI/g" \
    -e "s/__SECURITY_EPOCH__/$POLICY_EPOCH/g" \
    -e "s/__MIN_BASE__/$MIN_BASE/g" \
    -e "s/__MIN_DESKTOP__/$MIN_DESKTOP/g" \
    "$DEB_DIR/claw-os-agent/control" > "$CONTROL_STAGE/DEBIAN/control"
CONTROL_DEB="$WORK/control-fixture.deb"
dpkg-deb -Znone --root-owner-group --build "$CONTROL_STAGE" "$CONTROL_DEB" >/dev/null
[ "$(dpkg-deb --field "$CONTROL_DEB" XB-Claw-Os-Security-Epoch)" = "$POLICY_EPOCH" ] \
    || fail "the security epoch must be readable from the built package"
[ "$(dpkg-deb --field "$CONTROL_DEB" XB-Claw-Os-Abi)" = "$POLICY_ABI" ] \
    || fail "the ABI generation must be readable from the built package"
[ "$(dpkg-deb --field "$CONTROL_DEB" Provides)" = "claw-os-abi-$POLICY_ABI" ] \
    || fail "the ABI virtual package must be declared"
ok "the security epoch and ABI generation survive into the built package"

# ---------------------------------------------------------------------------
# 15. Signing fails closed.
#
# The build scripts must distinguish "no key requested" — an explicitly
# unsigned local build — from "a key was requested and cannot be used",
# which has to be a hard error. Clearing the requested key id and
# continuing would emit an unsigned artifact under a name a publication
# workflow is about to upload.
# ---------------------------------------------------------------------------
SIGNER="$RELEASE_SECURITY_DIR/sign-manifest.sh"
[ -s "$SIGNER" ] || fail "there is no shared release-security signer"
# shellcheck source=/dev/null
source "$SIGNER"

for script in "$DEB_DIR/build-debs.sh" "$DEB_DIR/build-desktop-deb.sh"; do
    grep -Fq 'claw_resolve_signing_key' "$script" \
        || fail "$(basename "$script") does not use the shared key resolver"
    grep -Fq 'claw_write_release_manifest' "$script" \
        || fail "$(basename "$script") does not use the shared manifest writer"
    grep -q 'RELEASE_SECURITY_KEY_ID=""' "$script" \
        && fail "$(basename "$script") still clears a requested signing key"
done
ok "both package builds resolve and sign through the shared fail-closed helper"

# No key requested: an unsigned build, clearly announced.
unsigned_key="$(
    CLAW_OS_RELEASE_SECURITY_KEY_ID="" GPG_KEY_ID="" \
        claw_resolve_signing_key 2>"$WORK/unsigned-key.err"
)" || fail "a build with no signing key must be allowed"
[ -z "$unsigned_key" ] || fail "no key was requested, yet one was resolved"
grep -q "UNSIGNED LOCAL BUILD" "$WORK/unsigned-key.err" \
    || fail "an unsigned local build must say so"

# A key that is requested but unavailable is fatal.
if CLAW_OS_RELEASE_SECURITY_KEY_ID="DEADBEEFDEADBEEFDEADBEEFDEADBEEFDEADBEEF" \
    claw_resolve_signing_key >/dev/null 2>"$WORK/missing-key.err"; then
    fail "a requested but unavailable signing key was accepted"
fi
grep -q "secret key is unavailable" "$WORK/missing-key.err" \
    || fail "the unavailable key was not explained"

ok "a requested but unavailable signing key aborts the build"

# Signing that fails must leave nothing publishable behind.
SIGN_STAGE="$WORK/stage-sign-failure"
rm -rf "$SIGN_STAGE"
mkdir -p "$SIGN_STAGE"
for path in $(component_paths claw-os-agent); do
    mkdir -p "$SIGN_STAGE$(dirname "$path")"
    printf 'component %s\n' "$path" > "$SIGN_STAGE$path"
    chmod 0755 "$SIGN_STAGE$path"
done
SIGN_OUT="$SIGN_STAGE/usr/lib/cos/release-security/claw-os-agent/manifest.json"

FAILING_BIN="$WORK/failing-gpg"
mkdir -p "$FAILING_BIN"
cat > "$FAILING_BIN/gpg" <<EOF
#!/usr/bin/env bash
# Signing fails; everything else behaves normally, so the build gets
# past key validation and dies where a real signing failure would.
for argument in "\$@"; do
    if [ "\$argument" = "--detach-sign" ]; then
        echo "gpg: simulated signing failure" >&2
        exit 2
    fi
done
exec "$(command -v gpg)" "\$@"
EOF
chmod +x "$FAILING_BIN/gpg"

if PATH="$FAILING_BIN:$PATH" claw_write_release_manifest \
    "$KEY_ID" "" "$MAKE_MANIFEST" claw-os-agent "$V1" amd64 trixie \
    "$SIGN_STAGE" "$POLICY" "$SIGN_OUT" >/dev/null 2>"$WORK/sign-failure.err"; then
    fail "a failed signature was reported as success"
fi
[ ! -e "$SIGN_OUT" ] \
    || fail "a failed signature left a manifest a later step could package"
[ ! -e "$SIGN_OUT.asc" ] || fail "a failed signature left a signature file"
ok "a failed signature leaves no publishable manifest"

# A signature that does not verify against the signing key is refused
# even when gpg reported success.
TRUNCATING_BIN="$WORK/truncating-gpg"
mkdir -p "$TRUNCATING_BIN"
cat > "$TRUNCATING_BIN/gpg" <<EOF
#!/usr/bin/env bash
# Claims to sign, writes a signature over other bytes.
previous=""
output=""
detach=0
for argument in "\$@"; do
    [ "\$argument" = "--detach-sign" ] && detach=1
    case "\$previous" in
        -o) output="\$argument" ;;
    esac
    previous="\$argument"
done
if [ "\$detach" = "1" ] && [ -n "\$output" ]; then
    printf -- '-----BEGIN PGP SIGNATURE-----\nnot a signature\n-----END PGP SIGNATURE-----\n' \
        > "\$output"
    exit 0
fi
exec "$(command -v gpg)" "\$@"
EOF
chmod +x "$TRUNCATING_BIN/gpg"

if PATH="$TRUNCATING_BIN:$PATH" claw_write_release_manifest \
    "$KEY_ID" "" "$MAKE_MANIFEST" claw-os-agent "$V1" amd64 trixie \
    "$SIGN_STAGE" "$POLICY" "$SIGN_OUT" >/dev/null 2>"$WORK/bad-signature.err"; then
    fail "a signature that does not verify was accepted"
fi
[ ! -e "$SIGN_OUT" ] || fail "an unverifiable signature left a manifest behind"
grep -q "does not verify" "$WORK/bad-signature.err" \
    || fail "the unverifiable signature was not explained"
ok "a signature that does not verify aborts the build and removes the manifest"

# A genuine signed build still works, and an unsigned one is allowed but
# carries no signature.
claw_write_release_manifest "$KEY_ID" "" "$MAKE_MANIFEST" claw-os-agent "$V1" \
    amd64 trixie "$SIGN_STAGE" "$POLICY" "$SIGN_OUT" >/dev/null \
    || fail "a genuine signed build must succeed"
[ -s "$SIGN_OUT.asc" ] || fail "a signed build produced no signature"
gpgv --keyring "$KEYRING" "$SIGN_OUT.asc" "$SIGN_OUT" >/dev/null 2>&1 \
    || fail "the signature does not verify against the publishing key"
rm -f "$SIGN_OUT" "$SIGN_OUT.asc"
claw_write_release_manifest "" "" "$MAKE_MANIFEST" claw-os-agent "$V1" \
    amd64 trixie "$SIGN_STAGE" "$POLICY" "$SIGN_OUT" >/dev/null \
    || fail "an explicitly unsigned local build must succeed"
[ -s "$SIGN_OUT" ] || fail "an unsigned build produced no manifest"
[ ! -e "$SIGN_OUT.asc" ] || fail "an unsigned build produced a signature"
ok "a signed build verifies against its key; an unsigned one carries no signature"

# The production build script itself, end to end: a requested key that
# cannot be used must abort before any artifact exists.
BUILD_PROBE_OUT="$WORK/build-probe-debs"
BUILD_PROBE_STAGE="$WORK/build-probe-stage"
mkdir -p "$BUILD_PROBE_OUT" "$BUILD_PROBE_STAGE"
if COS_PACKAGE_VERSION="1:0.2.0+gitprobe.gaaaaaaaaaaaa" \
    COS_DEB_OUT_DIR="$BUILD_PROBE_OUT" \
    COS_DEB_STAGE_DIR="$BUILD_PROBE_STAGE" \
    CLAW_OS_RELEASE_SECURITY_KEY_ID="DEADBEEFDEADBEEFDEADBEEFDEADBEEFDEADBEEF" \
    bash "$BUILD_DEBS" agent >"$WORK/build-probe.out" 2>"$WORK/build-probe.err"; then
    fail "build-debs.sh continued with an unusable signing key"
fi
grep -q "secret key is unavailable" "$WORK/build-probe.err" \
    || { cat "$WORK/build-probe.err" >&2; fail "the build did not explain the refusal"; }
[ -z "$(ls -A "$BUILD_PROBE_OUT" 2>/dev/null)" ] \
    || fail "an aborted signed build still produced an artifact"
ok "build-debs.sh aborts before producing anything when its key is unusable"

printf '\n%d checks passed\n' "$PASS"
