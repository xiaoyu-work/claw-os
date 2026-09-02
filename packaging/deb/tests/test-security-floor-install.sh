#!/bin/bash
# packaging/deb/tests/test-security-floor-install.sh
#
# Installation-level tests for Claw OS update downgrade protection,
# driven through the real package manager rather than through
# maintainer scripts invoked by hand:
#
#   * `dpkg --root ... -i` installing agent + base + desktop in one
#     transaction, so package file ownership, maintainer-script
#     ordering and per-package release manifests are exercised the way
#     dpkg actually exercises them;
#   * `apt-get install` against a real signed local repository with the
#     shipped `apt.conf.d` snippet installed, so APT's own
#     `DPkg::Pre-Install-Pkgs` machinery — including the protocol
#     version it selects for our hook — is what runs.
#
# Both run unprivileged: `fakeroot` gives dpkg a root's-eye view of
# ownership and `--force-script-chrootless` sets `DPKG_ROOT` for the
# maintainer scripts, which honour it.
#
# What this cannot prove without a second uid: that an ordinary user is
# *denied* the private floor. The mechanism for that is file mode, and
# the modes are asserted exactly (0700/0600 private, 0755/0644 runtime
# projection), together with the projection's contents.
#
# Usage:
#   bash packaging/deb/tests/test-security-floor-install.sh
#
# Environment:
#   COS_SECURITY_FLOOR_BIN  prebuilt claw-security-floor (else cargo build)
#   COS_TEST_TMPDIR         scratch root (must be a Linux filesystem)

set -euo pipefail

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PROJECT_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/../../.." && pwd)

DEB_DIR="$PROJECT_DIR/packaging/deb"
RELEASE_SECURITY_DIR="$PROJECT_DIR/packaging/release-security"
POLICY="$RELEASE_SECURITY_DIR/policy.json"
MAKE_MANIFEST="$RELEASE_SECURITY_DIR/make-manifest.py"
RENDER_PREINST="$RELEASE_SECURITY_DIR/render-preinst.sh"

WORK_ROOT="${COS_TEST_TMPDIR:-$PROJECT_DIR/build/tests}"
WORK="$WORK_ROOT/security-floor-install-$$"
PASS=0

cleanup() {
    if [ -n "${GNUPGHOME:-}" ] && [ -d "${GNUPGHOME:-}" ]; then
        gpgconf --kill all >/dev/null 2>&1 || true
    fi
    # `COS_TEST_KEEP=1` retains the scratch root: these fixtures are the
    # only way to inspect a real dpkg/apt transaction after the fact.
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

for tool in dpkg dpkg-deb fakeroot gpg gpgv python3 apt-get apt-config apt-ftparchive; do
    command -v "$tool" >/dev/null 2>&1 || fail "$tool is required for this test"
done

mkdir -p "$WORK"

# ---------------------------------------------------------------------------
# The verifier under test, stripped so the fixture archives stay small.
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
STRIPPED="$WORK/claw-security-floor"
cp "$HELPER" "$STRIPPED"
chmod 0755 "$STRIPPED"
command -v strip >/dev/null 2>&1 && strip "$STRIPPED" 2>/dev/null || true
HELPER="$STRIPPED"

# ---------------------------------------------------------------------------
# Ephemeral publisher key.
# ---------------------------------------------------------------------------
export GNUPGHOME="$WORK/gnupg"
mkdir -m 700 -p "$GNUPGHOME"
gpg --batch --quiet --passphrase '' --quick-generate-key \
    'Claw OS Install Test <test@example.invalid>' default default never >/dev/null 2>&1
KEY_ID="$(gpg --batch --with-colons --list-secret-keys \
    | awk -F: '$1 == "fpr" { print $10; exit }')"
[ -n "$KEY_ID" ] || fail "could not create an ephemeral signing key"
KEYRING="$WORK/keyring.gpg"
gpg --batch --export "$KEY_ID" > "$KEYRING"

POLICY_EPOCH="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["security_epoch"])' "$POLICY")"
POLICY_ABI="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["abi"])' "$POLICY")"

component_paths() {
    python3 - "$POLICY" "$1" <<'PY'
import json, sys
policy = json.load(open(sys.argv[1], encoding="utf-8"))
for entry in policy["components"]:
    if entry["package"] == sys.argv[2]:
        print(entry["path"])
PY
}

# stage_package <package> <version> <arch> <marker> <out-dir>
#
# Uses the production maintainer scripts, the production manifest
# generator and the production preinst renderer. The control file
# declares only the Claw OS relationships so dpkg can order the set
# without the distribution dependencies a fixture root does not have.
stage_package() {
    local package="$1" version="$2" arch="$3" marker="$4" out_dir="$5"
    local stage="$WORK/stage-$package-$version-$arch"
    rm -rf "$stage"
    mkdir -p "$stage/DEBIAN"

    local path
    for path in $(component_paths "$package"); do
        mkdir -p "$stage$(dirname "$path")"
        if [ "$path" = "/usr/lib/cos/bin/claw-security-floor" ]; then
            cp "$HELPER" "$stage$path"
        else
            printf 'claw-os component %s %s %s\n' "$path" "$version" "$marker" > "$stage$path"
        fi
        chmod 0755 "$stage$path"
    done

    python3 "$MAKE_MANIFEST" \
        --package "$package" \
        --version "$version" \
        --arch "$arch" \
        --suite trixie \
        --stage-dir "$stage" \
        --policy "$POLICY" \
        --output "$stage/usr/lib/cos/release-security/$package/manifest.json" \
        --sign-key "$KEY_ID" > /dev/null

    "$RENDER_PREINST" "$package" "$version" "$POLICY_EPOCH" "$stage" "$stage/DEBIAN/preinst"
    sed -e "s/__VERSION__/$version/g" "$DEB_DIR/$package/postinst" > "$stage/DEBIAN/postinst"
    chmod 0755 "$stage/DEBIAN/postinst"
    install -m 755 "$DEB_DIR/$package/prerm" "$stage/DEBIAN/prerm"
    if [ -f "$DEB_DIR/$package/postrm" ]; then
        install -m 755 "$DEB_DIR/$package/postrm" "$stage/DEBIAN/postrm"
    fi

    # The APT integration belongs to the agent package.
    if [ "$package" = "claw-os-agent" ]; then
        mkdir -p "$stage/usr/lib/cos/apt" "$stage/etc/apt/apt.conf.d"
        install -m 755 "$DEB_DIR/common/security-floor-hook" \
            "$stage/usr/lib/cos/apt/security-floor-hook"
        install -m 644 "$DEB_DIR/common/50claw-os-security-floor" \
            "$stage/etc/apt/apt.conf.d/50claw-os-security-floor"
        printf '/etc/apt/apt.conf.d/50claw-os-security-floor\n' > "$stage/DEBIAN/conffiles"
    fi

    local depends="" provides=""
    case "$package" in
        claw-os-agent) provides="claw-os-abi-$POLICY_ABI" ;;
        claw-os-base) depends="claw-os-agent, claw-os-abi-$POLICY_ABI" ;;
        claw-os-desktop) depends="claw-os-base, claw-os-abi-$POLICY_ABI" ;;
    esac
    {
        printf 'Package: %s\nVersion: %s\nSection: admin\nPriority: optional\n' \
            "$package" "$version"
        printf 'Architecture: %s\nMaintainer: Claw OS <noreply@github.com>\n' "$arch"
        [ -n "$depends" ] && printf 'Depends: %s\n' "$depends"
        [ -n "$provides" ] && printf 'Provides: %s\n' "$provides"
        printf 'XB-Claw-Os-Security-Epoch: %s\nXB-Claw-Os-Abi: %s\n' \
            "$POLICY_EPOCH" "$POLICY_ABI"
        printf 'Description: install fixture for downgrade-protection tests\n'
    } > "$stage/DEBIAN/control"

    mkdir -p "$out_dir"
    fakeroot dpkg-deb -Znone --root-owner-group --build "$stage" \
        "$out_dir/${package}_${version}_${arch}.deb" >/dev/null
    printf '%s\n' "$out_dir/${package}_${version}_${arch}.deb"
}

new_root() {
    local root="$WORK/$1"
    rm -rf "$root"
    mkdir -p "$root/var/lib/dpkg/info" "$root/var/lib/dpkg/updates" \
        "$root/var/lib/dpkg/triggers" "$root/var/lib/dpkg/alternatives" \
        "$root/var/lib/dpkg/parts" "$root/var/log"
    : > "$root/var/lib/dpkg/status"
    : > "$root/var/lib/dpkg/available"
    printf '%s\n' "$root"
}

dpkg_install() {
    local root="$1"
    shift
    fakeroot dpkg --root="$root" --force-not-root --force-script-chrootless \
        --log="$root/var/log/dpkg.log" -i "$@"
}

floor_field() {
    python3 -c '
import json, sys
document = json.load(open(sys.argv[1]))
cursor = document
for key in sys.argv[2].split("."):
    cursor = cursor[key]
print(cursor)
' "$1" "$2"
}

V1="1:0.2.0+git100.gaaaaaaaaaaaa"
V2="1:0.2.0+git200.gbbbbbbbbbbbb"
V0="1:0.2.0+git50.gccccccccccc0"

DEBS="$WORK/debs"
AGENT_V1="$(stage_package claw-os-agent "$V1" amd64 alpha "$DEBS")"
BASE_V1="$(stage_package claw-os-base "$V1" all alpha "$DEBS")"
DESKTOP_V1="$(stage_package claw-os-desktop "$V1" amd64 alpha "$DEBS")"
AGENT_V2="$(stage_package claw-os-agent "$V2" amd64 beta "$DEBS")"
BASE_V2="$(stage_package claw-os-base "$V2" all beta "$DEBS")"
DESKTOP_V2="$(stage_package claw-os-desktop "$V2" amd64 beta "$DEBS")"
AGENT_V0="$(stage_package claw-os-agent "$V0" amd64 gamma "$DEBS")"

# ---------------------------------------------------------------------------
# 1. One transaction installs all three packages.
# ---------------------------------------------------------------------------
ROOT="$(new_root root-multi)"
mkdir -p "$ROOT/usr/share/keyrings"
cp "$KEYRING" "$ROOT/usr/share/keyrings/claw-os-archive-keyring.gpg"

dpkg_install "$ROOT" "$AGENT_V1" "$BASE_V1" "$DESKTOP_V1" >"$WORK/install.log" 2>&1 \
    || { cat "$WORK/install.log" >&2; fail "installing the Claw OS set must succeed"; }
for package in claw-os-agent claw-os-base claw-os-desktop; do
    [ -s "$ROOT/usr/lib/cos/release-security/$package/manifest.json" ] \
        || fail "$package did not install its own release manifest"
    [ -s "$ROOT/usr/lib/cos/release-security/$package/manifest.json.asc" ] \
        || fail "$package did not install its own manifest signature"
done
ok "agent, base and desktop each install their own release manifest"

# No package may own a file another package also owns: dpkg would let
# whichever unpacked last decide what the others' scripts read.
duplicate="$(cat "$ROOT/var/lib/dpkg/info/"claw-os-*.list \
    | grep '/usr/lib/cos/release-security/' | sort | uniq -d || true)"
[ -z "$duplicate" ] \
    || fail "release-security files are owned by more than one package: $duplicate"
ok "no release-security file is owned by two packages"

# Each manifest describes its own package, not whichever was unpacked last.
for package in claw-os-agent claw-os-base claw-os-desktop; do
    named="$(python3 -c '
import json, sys
print(json.load(open(sys.argv[1]))["release"]["package"])
' "$ROOT/usr/lib/cos/release-security/$package/manifest.json")"
    [ "$named" = "$package" ] \
        || fail "$package installed a manifest describing $named"
done
ok "each installed manifest describes its own package"

FLOOR="$ROOT/var/lib/cos/security/floor.json"
[ -s "$FLOOR" ] || fail "the transaction did not record a security floor"
[ "$(floor_field "$FLOOR" generation)" = "3" ] \
    || fail "each package's postinst must commit its own generation"
for package in claw-os-agent claw-os-base claw-os-desktop; do
    [ "$(floor_field "$FLOOR" "packages.$package.version")" = "$V1" ] \
        || fail "$package did not record its own version in the floor"
done
ok "each package's postinst commits its own release into the floor"

# Every component of every package is measured, not just the agent's.
for name in clawd cos-init cos-agent-ui; do
    floor_field "$FLOOR" "components.$name.sha256" >/dev/null \
        || fail "component $name was not recorded"
done
ok "components from all three packages are recorded"

# ---------------------------------------------------------------------------
# 2. The unprivileged runtime projection.
# ---------------------------------------------------------------------------
RUNTIME="$ROOT/var/lib/cos-security/runtime-floor.json"
[ -s "$RUNTIME" ] || fail "no unprivileged runtime floor was published"
[ "$(stat -c '%a' "$ROOT/var/lib/cos-security")" = "755" ] \
    || fail "the runtime directory must be traversable by everyone"
[ "$(stat -c '%a' "$RUNTIME")" = "644" ] \
    || fail "the runtime floor must be readable by everyone"
[ "$(stat -c '%a' "$ROOT/var/lib/cos/security")" = "700" ] \
    || fail "the authoritative floor directory must stay private"
[ "$(stat -c '%a' "$FLOOR")" = "600" ] \
    || fail "the authoritative floor file must stay private"
[ "$(stat -c '%a' "$ROOT/var/lib/cos/security/history.jsonl")" = "600" ] \
    || fail "the floor history must stay private"
[ "$(stat -c '%a' "$ROOT/var/lib/cos/security/recovery")" = "700" ] \
    || fail "recovery authorizations must stay private"
ok "the private floor stays 0700/0600 and the runtime projection is 0755/0644"

for forbidden in trusted_keys revoked_digests previous_sha256 recovery; do
    grep -Fq "$forbidden" "$RUNTIME" \
        && fail "the runtime projection exposes $forbidden"
done
[ "$(python3 -c '
import json, sys
print(json.load(open(sys.argv[1]))["floor_generation"])
' "$RUNTIME")" = "3" ] || fail "the projection does not track the committed generation"
ok "the runtime projection carries no recovery or trust material"

# The projection is what an unprivileged binary reads. `runtime-check`
# is exactly the path cos/claw-agentd take at startup, and it never
# repairs anything.
"$HELPER" runtime-check --root "$ROOT" >/dev/null \
    || fail "an unprivileged process must be able to satisfy the runtime floor"
ok "an unprivileged process can read and satisfy the runtime floor"

cp "$RUNTIME" "$WORK/runtime-backup.json"
python3 - "$RUNTIME" <<'PY'
import json, sys
path = sys.argv[1]
document = json.loads(open(path, encoding="utf-8").read())
document["security_epoch"] = 99
open(path, "w", encoding="utf-8").write(
    json.dumps(document, sort_keys=True, separators=(",", ":")) + "\n"
)
PY
if "$HELPER" runtime-check --root "$ROOT" >/dev/null 2>&1; then
    fail "a tampered runtime projection was accepted"
fi
ok "a tampered runtime projection is refused by unprivileged callers"

# The privileged pass owns the projection and repairs it.
"$HELPER" verify-installed --root "$ROOT" --scope epoch >/dev/null \
    || fail "the privileged pass must repair a tampered projection"
cmp -s "$RUNTIME" "$WORK/runtime-backup.json" \
    || fail "the repaired projection does not match the authority"
ok "the privileged pass repairs a tampered projection"

rm -f "$RUNTIME"
if "$HELPER" runtime-check --root "$ROOT" >/dev/null 2>&1; then
    fail "a deleted runtime projection was accepted"
fi
"$HELPER" verify-installed --root "$ROOT" --scope epoch >/dev/null \
    || fail "the privileged pass must repair a deleted projection"
[ -s "$RUNTIME" ] || fail "the deleted projection was not republished"
ok "a deleted projection fails closed and is republished from the authority"

# ---------------------------------------------------------------------------
# 3. Upgrading the whole set in one transaction.
# ---------------------------------------------------------------------------
dpkg_install "$ROOT" "$AGENT_V2" "$BASE_V2" "$DESKTOP_V2" >"$WORK/upgrade.log" 2>&1 \
    || { cat "$WORK/upgrade.log" >&2; fail "upgrading the Claw OS set must succeed"; }
for package in claw-os-agent claw-os-base claw-os-desktop; do
    [ "$(floor_field "$FLOOR" "packages.$package.version")" = "$V2" ] \
        || fail "$package did not advance its own floor entry"
done
[ "$(floor_field "$FLOOR" generation)" = "6" ] \
    || fail "an ordered upgrade must record one generation per package"
[ "$(python3 -c '
import json, sys
print(json.load(open(sys.argv[1]))["floor_generation"])
' "$RUNTIME")" = "6" ] || fail "the projection did not follow the upgrade"
ok "an ordered multi-package upgrade advances every package's floor"

# The deferred-start message is the service-coherence signal: the agent
# refuses to start the broker while the set is still mixed.
grep -q 'deferring clawd start' "$WORK/upgrade.log" && \
    fail "no start should be deferred when the whole set moves together"
"$HELPER" service-gate --root "$ROOT" --package claw-os-agent \
    --manifest "$ROOT/usr/lib/cos/release-security/claw-os-agent/manifest.json" \
    --installed "claw-os-base=$V2" --installed "claw-os-desktop=$V2" >/dev/null \
    || fail "a coherent installed set must pass the service gate"
if "$HELPER" service-gate --root "$ROOT" --package claw-os-agent \
    --manifest "$ROOT/usr/lib/cos/release-security/claw-os-agent/manifest.json" \
    --installed "claw-os-base=0.1.0" >/dev/null 2>&1; then
    fail "an incompatible installed set must fail the service gate"
fi
ok "service coherence is decided from the installed set"

# ---------------------------------------------------------------------------
# 4. Downgrading one package out of the set.
# ---------------------------------------------------------------------------
if dpkg_install "$ROOT" "$AGENT_V0" >"$WORK/downgrade.log" 2>&1; then
    fail "dpkg accepted a downgrade of claw-os-agent"
fi
grep -q "security floor only moves forward" "$WORK/downgrade.log" \
    || { cat "$WORK/downgrade.log" >&2; fail "dpkg did not report the floor refusal"; }
[ "$(floor_field "$FLOOR" "packages.claw-os-agent.version")" = "$V2" ] \
    || fail "a refused downgrade must not change the floor"
ok "dpkg refuses a downgrade of one package in an installed set"

# ---------------------------------------------------------------------------
# 5. Real APT, real hook registration, real transaction.
# ---------------------------------------------------------------------------
REPO="$WORK/repo"
mkdir -p "$REPO"
cp "$DEBS"/*.deb "$REPO/"
# An unrelated package, to prove the hook never blocks the rest of the
# system.
UNRELATED="$WORK/stage-unrelated"
rm -rf "$UNRELATED"
mkdir -p "$UNRELATED/DEBIAN" "$UNRELATED/usr/share/unrelated"
echo unrelated > "$UNRELATED/usr/share/unrelated/file"
cat > "$UNRELATED/DEBIAN/control" <<EOF
Package: unrelated-pkg
Version: 1.0
Section: admin
Priority: optional
Architecture: amd64
Maintainer: Claw OS <noreply@github.com>
Description: unrelated package
EOF
fakeroot dpkg-deb -Znone --root-owner-group --build "$UNRELATED" \
    "$REPO/unrelated-pkg_1.0_amd64.deb" >/dev/null

( cd "$REPO" && apt-ftparchive packages . > Packages \
    && apt-ftparchive -o APT::FTPArchive::Release::Suite=./ release . > Release )
gpg --batch --yes --pinentry-mode loopback --default-key "$KEY_ID" \
    --clearsign -o "$REPO/InRelease" "$REPO/Release"

APT_ROOT="$(new_root root-apt)"
mkdir -p "$APT_ROOT/usr/share/keyrings" "$WORK/apt/state/lists/partial" \
    "$WORK/apt/cache/archives/partial" "$WORK/apt/log" "$WORK/apt/etc"
cp "$KEYRING" "$APT_ROOT/usr/share/keyrings/claw-os-archive-keyring.gpg"
printf 'deb [signed-by=%s] file://%s ./\n' \
    "$APT_ROOT/usr/share/keyrings/claw-os-archive-keyring.gpg" "$REPO" \
    > "$WORK/apt/sources.list"

cat > "$WORK/apt/dpkg-wrapper" <<EOF
#!/bin/sh
exec fakeroot dpkg --root="$APT_ROOT" --force-not-root --force-script-chrootless "\$@"
EOF
chmod 0755 "$WORK/apt/dpkg-wrapper"

# The shipped apt.conf.d snippet is what registers the hook. Rewrite
# only the absolute paths so it can run against the test root, and
# assert that APT ends up with exactly the intended keys.
sed -e "s|/usr/lib/cos/apt/security-floor-hook|$WORK/apt/hook|g" \
    "$DEB_DIR/common/50claw-os-security-floor" > "$WORK/apt/etc/50claw-os-security-floor"
# The hook APT runs is the *shipped* script, with only its absolute
# paths retargeted at the test root. Its decision logic — exec the
# verifier, or drain and succeed only when there is no floor — is
# therefore the logic under test.
cat > "$WORK/apt/helper-shim" <<EOF
#!/bin/sh
exec "$HELPER" "\$@" --root "$APT_ROOT"
EOF
chmod 0755 "$WORK/apt/helper-shim"
sed -e "s|^HELPER=.*|HELPER=$WORK/apt/helper-shim|" \
    -e "s|^FLOOR=.*|FLOOR=$APT_ROOT/var/lib/cos/security/floor.json|" \
    -e "s|^RUNTIME_FLOOR=.*|RUNTIME_FLOOR=$APT_ROOT/var/lib/cos-security/runtime-floor.json|" \
    "$DEB_DIR/common/security-floor-hook" > "$WORK/apt/hook"
chmod 0755 "$WORK/apt/hook"
grep -q "$WORK/apt/helper-shim" "$WORK/apt/hook" \
    || fail "the retargeted hook does not point at the test verifier"

apt_run() {
    # `-c` is how a configuration *file* is added: `Dir::Etc::parts` is
    # read before `-o` options are applied, so registering the hook
    # through it would silently do nothing.
    apt-get \
        -c "$WORK/apt/etc/50claw-os-security-floor" \
        -o "Dir::Etc::sourcelist=$WORK/apt/sources.list" \
        -o "Dir::Etc::sourceparts=-" \
        -o "Dir::Etc::preferencesparts=-" \
        -o "Dir::State=$WORK/apt/state" \
        -o "Dir::State::status=$APT_ROOT/var/lib/dpkg/status" \
        -o "Dir::Cache=$WORK/apt/cache" \
        -o "Dir::Log=$WORK/apt/log" \
        -o "Dir::Bin::dpkg=$WORK/apt/dpkg-wrapper" \
        -o "APT::Architecture=amd64" \
        -o "APT::Architectures=amd64" \
        -o "Debug::NoLocking=1" \
        -o "APT::Sandbox::User=root" \
        "$@"
}

registered="$(apt-config -c "$WORK/apt/etc/50claw-os-security-floor" dump \
    | grep -c "DPkg::Tools::options::$WORK/apt/hook::Version \"2\";" || true)"
[ "$registered" = "1" ] \
    || fail "the apt.conf snippet must register the hook's protocol version"
hook_registered="$(apt-config -c "$WORK/apt/etc/50claw-os-security-floor" dump \
    | grep -c "DPkg::Pre-Install-Pkgs:: \"$WORK/apt/hook\";" || true)"
[ "$hook_registered" = "1" ] \
    || fail "the apt.conf snippet must register the hook as a single token"
ok "APT registers the hook as one executable token with protocol version 2"

apt_run update -qq >/dev/null 2>&1 || fail "apt-get update against the signed test repo failed"

# Whether APT honours a hook's exit status is a property of the APT in
# use, and the design does not depend on a guess: measure it, and hold
# the shipped hook to that standard.
cat > "$WORK/apt/refusing-hook" <<'EOF'
#!/bin/sh
cat > /dev/null
exit 1
EOF
chmod 0755 "$WORK/apt/refusing-hook"
if apt_run -o "DPkg::Pre-Install-Pkgs::=$WORK/apt/refusing-hook" \
    -o "DPkg::Tools::Options::$WORK/apt/refusing-hook::Version=2" \
    -y install unrelated-pkg >"$WORK/apt-hookveto.log" 2>&1; then
    fail "this APT ignored a Pre-Install-Pkgs refusal; the hook cannot be relied on"
fi
ok "APT aborts the transaction when the pre-install hook refuses"

# A genuine pre-install bootstrap: no floor, and the verifier not yet
# installed. The shipped hook must drain APT's list and succeed.
mv "$WORK/apt/helper-shim" "$WORK/apt/helper-shim.hidden"
apt_run -y install unrelated-pkg --reinstall >"$WORK/apt-unrelated.log" 2>&1 \
    || { cat "$WORK/apt-unrelated.log" >&2; fail "an unrelated install must not be blocked"; }
ok "an unrelated package installs while Claw OS is absent"

mv "$WORK/apt/helper-shim.hidden" "$WORK/apt/helper-shim"
apt_run -y install "claw-os-agent=$V2" >"$WORK/apt-bootstrap.log" 2>&1 \
    || { cat "$WORK/apt-bootstrap.log" >&2; fail "the first Claw OS install must succeed"; }
[ -s "$APT_ROOT/var/lib/cos/security/floor.json" ] \
    || fail "the first apt install did not record a floor"
ok "apt installs the current Claw OS release and records the floor"

apt_run -y install unrelated-pkg --reinstall >"$WORK/apt-unrelated2.log" 2>&1 \
    || { cat "$WORK/apt-unrelated2.log" >&2; fail "unrelated installs must keep working"; }
ok "unrelated package operations keep working on a protected machine"

if apt_run -y --allow-downgrades install "claw-os-agent=$V1" \
    >"$WORK/apt-downgrade.log" 2>&1; then
    fail "apt installed an older Claw OS release"
fi
grep -q "version_regression" "$WORK/apt-downgrade.log" \
    || { cat "$WORK/apt-downgrade.log" >&2; fail "the APT hook did not report the refusal"; }
[ "$(floor_field "$APT_ROOT/var/lib/cos/security/floor.json" \
    "packages.claw-os-agent.version")" = "$V2" ] \
    || fail "a refused apt transaction must not change the floor"
ok "apt aborts a downgrade before anything is unpacked"

# A tampered candidate: same version, different bytes.
TAMPERED="$WORK/tampered"
mkdir -p "$TAMPERED"
TAMPERED_DEB="$(stage_package claw-os-agent "$V2" amd64 "different-bytes" "$TAMPERED")"
cp "$TAMPERED_DEB" "$REPO/"
( cd "$REPO" && apt-ftparchive packages . > Packages \
    && apt-ftparchive -o APT::FTPArchive::Release::Suite=./ release . > Release )
gpg --batch --yes --pinentry-mode loopback --default-key "$KEY_ID" \
    --clearsign -o "$REPO/InRelease" "$REPO/Release"
apt_run update -qq >/dev/null 2>&1
if apt_run -y --reinstall install "claw-os-agent=$V2" \
    >"$WORK/apt-tampered.log" 2>&1; then
    fail "apt installed a substituted artifact for a recorded version"
fi
grep -q "artifact_mismatch" "$WORK/apt-tampered.log" \
    || { cat "$WORK/apt-tampered.log" >&2; fail "the refusal did not name the substitution"; }
ok "apt aborts a same-version artifact substitution"

# The hook is the earliest detector, and it must have seen and recorded
# both refusals with the real APT v2 payload — package name, both
# versions and the archive path.
JOURNAL="$APT_ROOT/var/log/cos/security-floor.jsonl"
[ -s "$JOURNAL" ] || fail "the hook recorded no decisions"
grep -q '"stage":"apt-hook"' "$JOURNAL" \
    || fail "the APT hook did not record its decisions"
grep -q '"class":"version_regression"' "$JOURNAL" \
    || fail "the APT hook did not detect the downgrade"
grep -q '"class":"artifact_mismatch"' "$JOURNAL" \
    || fail "the APT hook did not detect the substitution"
ok "the APT hook detects and journals both refusals from the real v2 payload"

# With a floor established, a missing verifier must not become a silent
# bypass: the hook refuses, and so does dpkg.
mv "$WORK/apt/helper-shim" "$WORK/apt/helper-shim.hidden"
if apt_run -y install unrelated-pkg --reinstall >"$WORK/apt-noverifier.log" 2>&1; then
    cat "$WORK/apt-noverifier.log" >&2
    fail "a protected machine accepted a transaction with the verifier removed"
fi
grep -q "recorded update-security state" "$WORK/apt-noverifier.log" \
    || { cat "$WORK/apt-noverifier.log" >&2; fail "the hook did not explain the refusal"; }
mv "$WORK/apt/helper-shim.hidden" "$WORK/apt/helper-shim"
ok "apt refuses any transaction once the verifier is removed from a protected machine"

NOVERIFIER_ROOT="$(new_root root-noverifier)"
mkdir -p "$NOVERIFIER_ROOT/usr/share/keyrings"
cp "$KEYRING" "$NOVERIFIER_ROOT/usr/share/keyrings/claw-os-archive-keyring.gpg"
dpkg_install "$NOVERIFIER_ROOT" "$AGENT_V2" >/dev/null 2>&1 \
    || fail "seeding the no-verifier root failed"
rm -f "$NOVERIFIER_ROOT/usr/lib/cos/bin/claw-security-floor"
if dpkg_install "$NOVERIFIER_ROOT" "$AGENT_V1" >"$WORK/dpkg-noverifier.log" 2>&1; then
    fail "dpkg installed an older release with the verifier removed"
fi
grep -q "verifier" "$WORK/dpkg-noverifier.log" \
    || { cat "$WORK/dpkg-noverifier.log" >&2; fail "dpkg did not report the missing verifier"; }
ok "dpkg independently blocks installs when the verifier is removed"

# ---------------------------------------------------------------------------
# 5. The shared manifest-binding verifier the publication workflows run.
#
# Every publish workflow calls this one script, so it has to accept a
# genuine package of each shape and reject each way a manifest can stop
# describing the artifact that carries it.
# ---------------------------------------------------------------------------
VERIFY_MANIFEST="$RELEASE_SECURITY_DIR/verify-package-manifest.sh"
[ -x "$VERIFY_MANIFEST" ] || fail "the shared manifest verifier is not executable"

"$VERIFY_MANIFEST" "$AGENT_V1" --arch amd64 --require-signature --keyring "$KEYRING" \
    >/dev/null || fail "the verifier rejected a genuine agent package"
"$VERIFY_MANIFEST" "$BASE_V1" --arch all --require-signature --keyring "$KEYRING" \
    >/dev/null || fail "the verifier rejected a genuine base package"
"$VERIFY_MANIFEST" "$DESKTOP_V1" --arch amd64 --require-signature --keyring "$KEYRING" \
    >/dev/null || fail "the verifier rejected a genuine desktop package"
ok "the shared verifier accepts a genuine agent, base and desktop package"

# repack <source-deb> <out-deb> — unpack, let the caller edit, rebuild.
repack() {
    local source="$1" out="$2" edit="$3"
    local dir="$WORK/repack-$(basename "$out" .deb)"
    rm -rf "$dir"
    mkdir -p "$dir"
    dpkg-deb -R "$source" "$dir"
    ( cd "$dir" && eval "$edit" )
    fakeroot dpkg-deb -Znone --root-owner-group --build "$dir" "$out" >/dev/null
    printf '%s\n' "$out"
}

RELABEL="$(repack "$AGENT_V1" "$WORK/verify-relabel.deb" \
    "sed -i 's/^Version: .*/Version: 1:9.9.9/' DEBIAN/control")"
if "$VERIFY_MANIFEST" "$RELABEL" >"$WORK/verify-relabel.log" 2>&1; then
    fail "the verifier accepted a package relabelled to another version"
fi
grep -q "names version" "$WORK/verify-relabel.log" \
    || fail "the relabel refusal was not explained"
ok "the shared verifier refuses a package relabelled to another version"

SWAPPED="$(repack "$AGENT_V1" "$WORK/verify-swapped.deb" \
    "cp -a usr/lib/cos/release-security/claw-os-agent usr/lib/cos/release-security/claw-os-base")"
if "$VERIFY_MANIFEST" "$SWAPPED" >"$WORK/verify-swapped.log" 2>&1; then
    fail "the verifier accepted a package shipping another package's manifest directory"
fi
grep -q "another package's manifest directory" "$WORK/verify-swapped.log" \
    || fail "the stray-manifest refusal was not explained"
ok "the shared verifier refuses a package carrying a sibling's manifest directory"

SUBSTITUTED="$(repack "$AGENT_V1" "$WORK/verify-substituted.deb" \
    "printf 'replaced\n' > usr/local/bin/clawd")"
if "$VERIFY_MANIFEST" "$SUBSTITUTED" >"$WORK/verify-substituted.log" 2>&1; then
    fail "the verifier accepted a package whose component bytes changed"
fi
grep -q "does not match the digest" "$WORK/verify-substituted.log" \
    || fail "the component substitution was not explained"
ok "the shared verifier refuses a component that no longer matches its manifest"

# A security epoch APT cannot see is not enforceable: the Debian epoch
# has to carry it.
NO_EPOCH_DIR="$WORK/stage-no-debian-epoch"
NO_EPOCH_DEB="$WORK/verify-no-epoch.deb"
rm -rf "$NO_EPOCH_DIR"
mkdir -p "$NO_EPOCH_DIR"
dpkg-deb -R "$AGENT_V1" "$NO_EPOCH_DIR"
python3 - "$NO_EPOCH_DIR" "${V1#*:}" <<'PY'
import json
import pathlib
import re
import subprocess
import sys

stage, plain_version = pathlib.Path(sys.argv[1]), sys.argv[2]
control = stage / "DEBIAN/control"
control.write_text(
    re.sub(r"^Version: .*$", f"Version: {plain_version}", control.read_text(), flags=re.M)
)
manifest = stage / "usr/lib/cos/release-security/claw-os-agent/manifest.json"
document = json.loads(manifest.read_text(encoding="utf-8"))
document["release"]["version"] = plain_version
body = json.dumps(document, sort_keys=True, separators=(",", ":"), ensure_ascii=False) + "\n"
manifest.write_text(body, encoding="utf-8")
preinst = stage / "DEBIAN/preinst"
preinst.write_text(preinst.read_text(encoding="utf-8").replace(f"1:{plain_version}", plain_version))
subprocess.run(["gpg", "--batch", "--yes", "--detach-sign", "--armor",
                "-o", str(manifest) + ".asc", str(manifest)], check=True)
PY
fakeroot dpkg-deb -Znone --root-owner-group --build "$NO_EPOCH_DIR" "$NO_EPOCH_DEB" >/dev/null
if "$VERIFY_MANIFEST" "$NO_EPOCH_DEB" >"$WORK/verify-no-epoch.log" 2>&1; then
    fail "the verifier accepted a release whose Debian epoch hides its security epoch"
fi
grep -q "Debian epoch" "$WORK/verify-no-epoch.log" \
    || fail "the missing Debian epoch was not explained"
ok "the shared verifier refuses a security epoch APT could not act on"

# ---------------------------------------------------------------------------
# 6. The publication workflows use that one script and keep no copy.
# ---------------------------------------------------------------------------
for workflow in publish-agent-package publish-base-package publish-desktop-package; do
    file="$PROJECT_DIR/.github/workflows/$workflow.yml"
    grep -Fq 'packaging/release-security/verify-package-manifest.sh' "$file" \
        || fail "$workflow does not call the shared manifest verifier"
    grep -Fq 'set -euo pipefail' "$file" \
        || fail "$workflow has no strict-mode verification step"
    ! grep -q 'manifest is not in canonical encoding' "$file" \
        || fail "$workflow still carries an inline copy of the manifest checks"
    ! grep -q 'manifest_extrac[^t]' "$file" \
        || fail "$workflow still references a mistyped extraction directory"
done
ok "all three publication workflows share one manifest verifier"

# ---------------------------------------------------------------------------
# 7. A higher security epoch actually wins APT's own candidate selection.
#
# The whole emergency mechanism depends on this: a release whose upstream
# version sorts *lower* than what is installed must still be the one APT
# offers. That only works because the security epoch is published as the
# Debian epoch, so this is checked against real apt, not a comparison
# helper.
# ---------------------------------------------------------------------------
EMERGENCY_VERSION="2:0.2.0+git10.gemergency00"
EMERGENCY_POLICY="$WORK/policy-epoch2.json"
python3 - "$POLICY" "$EMERGENCY_POLICY" <<'PY'
import json
import sys

policy = json.load(open(sys.argv[1], encoding="utf-8"))
policy["security_epoch"] = 2
json.dump(policy, open(sys.argv[2], "w", encoding="utf-8"))
PY

EMERGENCY_STAGE="$WORK/stage-emergency"
rm -rf "$EMERGENCY_STAGE"
mkdir -p "$EMERGENCY_STAGE/DEBIAN"
for path in $(component_paths claw-os-agent); do
    mkdir -p "$EMERGENCY_STAGE$(dirname "$path")"
    if [ "$path" = "/usr/lib/cos/bin/claw-security-floor" ]; then
        cp "$HELPER" "$EMERGENCY_STAGE$path"
    else
        printf 'claw-os component %s %s emergency\n' "$path" "$EMERGENCY_VERSION" \
            > "$EMERGENCY_STAGE$path"
    fi
    chmod 0755 "$EMERGENCY_STAGE$path"
done
python3 "$MAKE_MANIFEST" \
    --package claw-os-agent \
    --version "$EMERGENCY_VERSION" \
    --arch amd64 \
    --suite trixie \
    --stage-dir "$EMERGENCY_STAGE" \
    --policy "$EMERGENCY_POLICY" \
    --output "$EMERGENCY_STAGE/usr/lib/cos/release-security/claw-os-agent/manifest.json" \
    --sign-key "$KEY_ID" > /dev/null
"$RENDER_PREINST" claw-os-agent "$EMERGENCY_VERSION" 2 \
    "$EMERGENCY_STAGE" "$EMERGENCY_STAGE/DEBIAN/preinst"
{
    printf 'Package: claw-os-agent\nVersion: %s\n' "$EMERGENCY_VERSION"
    printf 'Section: admin\nPriority: optional\nArchitecture: amd64\n'
    printf 'Maintainer: Claw OS <noreply@github.com>\n'
    printf 'Provides: claw-os-abi-%s\n' "$POLICY_ABI"
    printf 'XB-Claw-Os-Security-Epoch: 2\nXB-Claw-Os-Abi: %s\n' "$POLICY_ABI"
    printf 'Description: emergency release fixture\n'
} > "$EMERGENCY_STAGE/DEBIAN/control"
fakeroot dpkg-deb -Znone --root-owner-group --build "$EMERGENCY_STAGE" \
    "$REPO/claw-os-agent_2%3a0.2.0+git10.gemergency00_amd64.deb" >/dev/null

( cd "$REPO" && apt-ftparchive packages . > Packages \
    && apt-ftparchive -o APT::FTPArchive::Release::Suite=./ release . > Release )
gpg --batch --yes --pinentry-mode loopback --default-key "$KEY_ID" \
    --clearsign -o "$REPO/InRelease" "$REPO/Release"
apt_run update >/dev/null 2>&1

candidate="$(apt-cache \
    -o "Dir::Etc::sourcelist=$WORK/apt/sources.list" \
    -o "Dir::Etc::sourceparts=-" \
    -o "Dir::Etc::preferencesparts=-" \
    -o "Dir::State=$WORK/apt/state" \
    -o "Dir::State::status=$APT_ROOT/var/lib/dpkg/status" \
    -o "Dir::Cache=$WORK/apt/cache" \
    -o "APT::Architecture=amd64" \
    -o "APT::Architectures=amd64" \
    policy claw-os-agent 2>/dev/null \
    | awk '/Candidate:/ { print $2; exit }' || true)"
[ "$candidate" = "$EMERGENCY_VERSION" ] \
    || fail "apt chose $candidate, not the higher security epoch $EMERGENCY_VERSION"
ok "apt selects a higher security epoch over a higher upstream version"

# ---------------------------------------------------------------------------
# 8. Signature requirements and token-exact ABI matching.
# ---------------------------------------------------------------------------
# An unsigned package is a legitimate local build, but it must never be
# publishable: publication asks for the signature explicitly.
UNSIGNED_DIR="$WORK/repack-verify-unsigned"
UNSIGNED_DEB="$WORK/verify-unsigned.deb"
rm -rf "$UNSIGNED_DIR"
mkdir -p "$UNSIGNED_DIR"
dpkg-deb -R "$AGENT_V1" "$UNSIGNED_DIR"
rm -f "$UNSIGNED_DIR/usr/lib/cos/release-security/claw-os-agent/manifest.json.asc"
fakeroot dpkg-deb -Znone --root-owner-group --build "$UNSIGNED_DIR" \
    "$UNSIGNED_DEB" >/dev/null

"$VERIFY_MANIFEST" "$UNSIGNED_DEB" >/dev/null \
    || fail "an unsigned local build must still verify structurally"
if "$VERIFY_MANIFEST" "$UNSIGNED_DEB" --require-signature \
    >"$WORK/verify-unsigned.log" 2>&1; then
    fail "an unsigned package was accepted for publication"
fi
grep -q "unsigned release manifest" "$WORK/verify-unsigned.log" \
    || fail "the unsigned refusal was not explained"
ok "an unsigned build is refused when publication requires a signature"

# The signature has to verify against the *intended* key, not merely be
# a signature.
gpg --batch --quiet --passphrase '' --quick-generate-key \
    'Claw OS Impostor <impostor@example.invalid>' default default never \
    >/dev/null 2>&1
# Select by uid: `--list-secret-keys` prints an `fpr` line for every
# subkey too, so filtering by "not the other fingerprint" would happily
# pick a subkey of the *signing* key and prove nothing.
IMPOSTOR_KEY="$(gpg --batch --with-colons --list-secret-keys impostor@example.invalid \
    | awk -F: '$1 == "fpr" { print $10; exit }')"
[ -n "$IMPOSTOR_KEY" ] || fail "could not create a second ephemeral key"
IMPOSTOR_KEYRING="$WORK/impostor-keyring.gpg"
gpg --batch --export "$IMPOSTOR_KEY" > "$IMPOSTOR_KEYRING"

if "$VERIFY_MANIFEST" "$AGENT_V1" --require-signature \
    --keyring "$IMPOSTOR_KEYRING" >"$WORK/verify-wrongkey.log" 2>&1; then
    fail "a manifest signed by another key was accepted"
fi
grep -q "does not verify" "$WORK/verify-wrongkey.log" \
    || fail "the wrong-key refusal was not explained"
"$VERIFY_MANIFEST" "$AGENT_V1" --require-signature --keyring "$KEYRING" \
    >/dev/null || fail "the genuine signing key must still verify"
ok "publication pins the signature to the intended key"

# `claw-os-abi-1` must not be satisfied by `claw-os-abi-12`: those are
# different, incompatible ABI generations.
ABI_DIR="$WORK/repack-verify-abi"
ABI_DEB="$WORK/verify-abi.deb"
rm -rf "$ABI_DIR"
mkdir -p "$ABI_DIR"
dpkg-deb -R "$AGENT_V1" "$ABI_DIR"
sed -i "s/^Provides: claw-os-abi-$POLICY_ABI\$/Provides: claw-os-abi-${POLICY_ABI}2/" \
    "$ABI_DIR/DEBIAN/control"
grep -q "^Provides: claw-os-abi-${POLICY_ABI}2\$" "$ABI_DIR/DEBIAN/control" \
    || fail "the ABI fixture was not rewritten"
fakeroot dpkg-deb -Znone --root-owner-group --build "$ABI_DIR" "$ABI_DEB" >/dev/null
if "$VERIFY_MANIFEST" "$ABI_DEB" >"$WORK/verify-abi.log" 2>&1; then
    fail "claw-os-abi-${POLICY_ABI}2 was accepted as claw-os-abi-$POLICY_ABI"
fi
grep -q "must provide claw-os-abi-$POLICY_ABI" "$WORK/verify-abi.log" \
    || fail "the ABI mismatch was not explained"

# The real relationship syntax — commas, versions, architecture
# qualifiers and alternatives — must still match.
ABI_OK_DIR="$WORK/repack-verify-abi-ok"
ABI_OK_DEB="$WORK/verify-abi-ok.deb"
rm -rf "$ABI_OK_DIR"
mkdir -p "$ABI_OK_DIR"
dpkg-deb -R "$BASE_V1" "$ABI_OK_DIR"
sed -i "s|^Depends: .*|Depends: claw-os-agent (>= 1:0.2.0), claw-os-abi-$POLICY_ABI:any, adduser|" \
    "$ABI_OK_DIR/DEBIAN/control"
fakeroot dpkg-deb -Znone --root-owner-group --build "$ABI_OK_DIR" "$ABI_OK_DEB" >/dev/null
"$VERIFY_MANIFEST" "$ABI_OK_DEB" >/dev/null \
    || fail "a normal Debian relationship field must satisfy the ABI check"
ok "the ABI relationship is matched token-exactly, not by substring"

# ---------------------------------------------------------------------------
# 9. Publication workflows demand a verified signature before upload.
# ---------------------------------------------------------------------------
for workflow in publish-agent-package publish-base-package publish-desktop-package; do
    file="$PROJECT_DIR/.github/workflows/$workflow.yml"
    grep -Fq -- '--require-signature' "$file" \
        || fail "$workflow does not require a signed release manifest"
    grep -Fq -- '--keyring "$CLAW_OS_RELEASE_SECURITY_KEYRING"' "$file" \
        || fail "$workflow does not pin the verification keyring"
    grep -Fq 'CLAW_OS_RELEASE_SECURITY_KEYRING=' "$file" \
        || fail "$workflow does not export the intended verification keyring"
    # The keyring must come from the key that was just imported for
    # signing, not from whatever the runner happens to trust.
    grep -Fq 'gpg --batch --export "$key_id" > "$keyring"' "$file" \
        || fail "$workflow does not derive the keyring from the imported key"
    python3 - "$file" <<'PY'
import sys

import yaml

document = yaml.safe_load(open(sys.argv[1], encoding="utf-8"))
for job in (document.get("jobs") or {}).values():
    steps = job.get("steps") or []
    for index, step in enumerate(steps):
        script = step.get("run") or ""
        if "verify-package-manifest.sh" not in script:
            continue
        assert "set -euo pipefail" in script, (
            f"{step.get('name')} runs the verifier without strict mode"
        )
        assert "--require-signature" in script, (
            f"{step.get('name')} runs the verifier without --require-signature"
        )
        uploads = [
            later
            for later in steps[index + 1 :]
            if "upload-artifact" in (later.get("uses") or "")
        ]
        assert uploads, f"{step.get('name')} does not precede an artifact upload"
        break
    else:
        continue
    break
else:
    raise SystemExit("no job verifies the package manifest before upload")
PY
done
ok "every publication workflow verifies a signed manifest before upload"

printf '\n%d checks passed\n' "$PASS"
