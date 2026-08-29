#!/usr/bin/env bash
# packaging/apt-repo/build-repo.sh — assemble an apt repository at
# build/apt-repo/ from the .debs in build/debs/.
#
# Layout produced (Debian "flat-and-pool" style):
#
#   build/apt-repo/
#   ├── dists/trixie/
#   │   ├── InRelease           (clear-signed Release)
#   │   ├── Release             (always)
#   │   ├── Release.gpg         (detached signature)
#   │   └── main/
#   │       ├── binary-amd64/Packages{,.gz}    (if amd64 .debs present)
#   │       ├── binary-arm64/Packages{,.gz}    (if arm64 .debs present)
#   │       └── binary-all/Packages{,.gz}      (always — Architecture: all)
#   └── pool/main/c/claw-os-agent/claw-os-agent_<v>_<arch>.deb
#       pool/main/c/claw-os-base/claw-os-base_<v>_all.deb
#       pool/main/c/claw-os-desktop/claw-os-desktop_<v>_<arch>.deb
#
# Dual-arch: the script auto-discovers every Architecture: in build/debs/
# and emits one binary-<arch>/ tree per architecture, so an admin can run
# build-debs.sh twice (once on an amd64 host, once on an arm64 host)
# into the same build/debs/ directory and produce a single multi-arch repo.
#
# GPG_KEY_ID is mandatory. Publishing an unsigned repository is forbidden.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
source "$PROJECT_DIR/scripts/lib/git-readonly.sh"

DEBS_DIR="${COS_DEBS_DIR:-$PROJECT_DIR/build/debs}"
REPO_DIR="${COS_APT_REPO_DIR:-$PROJECT_DIR/build/apt-repo}"
BRAND_ASSETS_DIR="$PROJECT_DIR/assets/brand"
SUITE="${SUITE:-trixie}"
COMPONENT="main"
GPG_KEY_ID="${GPG_KEY_ID:-}"
GPG_PASSPHRASE="${GPG_PASSPHRASE:-}"

if [ ! -d "$DEBS_DIR" ] || [ -z "$(ls "$DEBS_DIR"/*.deb 2>/dev/null)" ]; then
    echo "error: no .debs in $DEBS_DIR — run packaging/deb/build-debs.sh first" >&2
    exit 1
fi

if ! command -v apt-ftparchive >/dev/null 2>&1; then
    echo "error: apt-ftparchive not found. Install it with: apt-get install apt-utils" >&2
    exit 1
fi
if ! command -v gpg >/dev/null 2>&1; then
    echo "error: gpg not found. Install it with: apt-get install gnupg" >&2
    exit 1
fi
if [ -z "$GPG_KEY_ID" ]; then
    echo "error: GPG_KEY_ID is required; refusing to build an unsigned apt repository" >&2
    exit 1
fi
if ! gpg --batch --list-secret-keys "$GPG_KEY_ID" >/dev/null 2>&1; then
    echo "error: signing secret key $GPG_KEY_ID is not available" >&2
    exit 1
fi

# Discover every Architecture: in the .deb filenames. Conventional Debian
# filename is `<pkg>_<version>_<arch>.deb`. We extract the final field.
declare -a binary_arches=()
arch_seen=""
for deb in "$DEBS_DIR"/*.deb; do
    name="$(basename "$deb")"
    # claw-os-agent_0.1.0_amd64.deb -> amd64
    deb_arch="${name##*_}"
    deb_arch="${deb_arch%.deb}"
    # Architecture: all packages are surfaced under every binary-<arch>
    # tree by apt's resolver, so we only iterate over real arches here.
    [ "$deb_arch" = "all" ] && continue
    case " $arch_seen " in
        *" $deb_arch "*) ;;
        *) binary_arches+=("$deb_arch"); arch_seen="$arch_seen $deb_arch" ;;
    esac
done

echo ":: building apt repo at $REPO_DIR"
if [ ${#binary_arches[@]} -eq 0 ]; then
    echo ":: arches: all"
else
    echo ":: arches: ${binary_arches[*]} all"
fi

rm -rf "$REPO_DIR"
for a in "${binary_arches[@]}"; do
    mkdir -p "$REPO_DIR/dists/$SUITE/$COMPONENT/binary-$a"
done
mkdir -p "$REPO_DIR/dists/$SUITE/$COMPONENT/binary-all"
mkdir -p "$REPO_DIR/assets/brand"
cp "$BRAND_ASSETS_DIR/clawos-wordmark.png" \
   "$BRAND_ASSETS_DIR/clawos-symbol.png" \
   "$BRAND_ASSETS_DIR/clawos-favicon-64.png" \
   "$BRAND_ASSETS_DIR/clawos-icon-192.png" \
   "$REPO_DIR/assets/brand/"
if [ -f "$BRAND_ASSETS_DIR/og.png" ]; then
    cp "$BRAND_ASSETS_DIR/og.png" "$REPO_DIR/assets/brand/"
fi

# Move each .deb into pool/main/c/<package-name>/.
for deb in "$DEBS_DIR"/*.deb; do
    name="$(basename "$deb")"
    # claw-os-agent_0.1.0_amd64.deb -> claw-os-agent
    pkg="${name%%_*}"
    pool_dir="$REPO_DIR/pool/$COMPONENT/c/$pkg"
    mkdir -p "$pool_dir"
    cp "$deb" "$pool_dir/"
    echo "  :: pool/$COMPONENT/c/$pkg/$name"
done

# Export the publishing key before anything is validated with it: the
# release-security metadata is verified against exactly the keyring the
# repository publishes, not against whatever happens to be in the
# builder's keyring.
gpg --batch --export "$GPG_KEY_ID" > "$REPO_DIR/claw-os-archive-keyring.gpg"
gpg --batch --armor --export "$GPG_KEY_ID" > "$REPO_DIR/claw-os-archive-keyring.asc"
test -s "$REPO_DIR/claw-os-archive-keyring.gpg"

# Freshness metadata. Refuses to publish a set that regresses a
# security epoch or version, replaces a published version with
# different content, or is not mutually compatible.
echo ":: verifying release-security metadata"
"$PROJECT_DIR/packaging/apt-repo/verify-release-security.sh" \
    "$REPO_DIR" "$SUITE" "$COMPONENT" \
    "$REPO_DIR/claw-os-archive-keyring.gpg" \
    "${COS_PREVIOUS_RELEASE_SECURITY_DIR:-$PROJECT_DIR/build/release-security-previous}"

# The verifier reports, in a structured file, whether it actually
# produced and verified a signed baseline marker. `Release` advertises
# the baseline only when it did: a repository that still predates
# downgrade protection must stay honestly unprotected rather than claim
# a guarantee that no artifact backs.
RELEASE_SECURITY_STATUS="$REPO_DIR/.release-security-status"
if [ ! -s "$RELEASE_SECURITY_STATUS" ]; then
    echo "error: verify-release-security.sh reported no status" >&2
    exit 1
fi
baseline_published="$(sed -n 's/^baseline=//p' "$RELEASE_SECURITY_STATUS")"
case "$baseline_published" in
    0|1) ;;
    *)
        echo "error: unusable release-security status '$baseline_published'" >&2
        exit 1
        ;;
esac
if [ "$baseline_published" = "1" ]; then
    test -s "$REPO_DIR/dists/$SUITE/release-security/baseline.json"
    test -s "$REPO_DIR/dists/$SUITE/release-security/baseline.json.asc"
else
    echo "warning: publishing without a release-security baseline marker" >&2
fi
rm -f "$RELEASE_SECURITY_STATUS"

# Generate Packages files. apt-ftparchive packages walks the pool and
# extracts the Architecture field from each .deb's control. The same
# pool feeds every binary-<arch>/ index; apt's client filters by arch
# at install time.
cd "$REPO_DIR"
echo ":: generating Packages indexes"
for a in "${binary_arches[@]}"; do
    apt-ftparchive --arch "$a" packages "pool/$COMPONENT" \
        > "dists/$SUITE/$COMPONENT/binary-$a/Packages"
    gzip -fk9 "dists/$SUITE/$COMPONENT/binary-$a/Packages"
done

# Architecture: all packages need an explicit binary-all index. We pass
# `--arch all` so apt-ftparchive only picks up Architecture: all .debs.
apt-ftparchive --arch all packages "pool/$COMPONENT" \
    > "dists/$SUITE/$COMPONENT/binary-all/Packages"
gzip -fk9 "dists/$SUITE/$COMPONENT/binary-all/Packages"

# Generate the Release file. The Architectures: list determines which
# binary-<arch>/ trees apt will fetch.
echo ":: generating Release"
if [ ${#binary_arches[@]} -eq 0 ]; then
    arch_list="all"
else
    arch_list="${binary_arches[*]} all"
fi

# Freshness of the index itself. `Valid-Until` bounds how long a mirror
# may keep serving this snapshot; APT refuses an expired Release unless
# a client explicitly disables the check, which the Claw OS apt source
# and the shipped apt.conf.d snippet both forbid.
RELEASE_VALID_DAYS="${COS_APT_RELEASE_VALID_DAYS:-30}"
release_date="$(date -u -R)"
valid_until="$(date -u -R -d "+${RELEASE_VALID_DAYS} days")"

cat > "$REPO_DIR/apt-ftparchive-release.conf" <<EOF
APT::FTPArchive::Release::Origin "Claw OS";
APT::FTPArchive::Release::Label "Claw OS";
APT::FTPArchive::Release::Suite "$SUITE";
APT::FTPArchive::Release::Codename "$SUITE";
APT::FTPArchive::Release::Architectures "$arch_list";
APT::FTPArchive::Release::Components "$COMPONENT";
APT::FTPArchive::Release::Description "Claw OS apt repository";
APT::FTPArchive::Release::Date "$release_date";
APT::FTPArchive::Release::Acquire-By-Hash "yes";
EOF

# Publish every index under by-hash/SHA256 as well, so a client that
# fetched a Release can always retrieve exactly the index that Release
# names even while the repository is being replaced.
echo ":: publishing by-hash indexes"
while IFS= read -r -d '' index; do
    index_dir="$(dirname "$index")"
    hash="$(sha256sum "$index" | cut -d' ' -f1)"
    mkdir -p "$index_dir/by-hash/SHA256"
    cp "$index" "$index_dir/by-hash/SHA256/$hash"
done < <(find "dists/$SUITE/$COMPONENT" -type f \
    \( -name 'Packages' -o -name 'Packages.gz' \) -print0)

apt-ftparchive -c="$REPO_DIR/apt-ftparchive-release.conf" \
    release "dists/$SUITE" > "dists/$SUITE/Release"

rm -f "$REPO_DIR/apt-ftparchive-release.conf"

# `apt-ftparchive` does not emit `Valid-Until` on every apt version, and
# publishing without it would silently drop the freshness bound that
# stops a mirror serving this snapshot forever. Insert it beside `Date:`
# ourselves, before the file is signed, and refuse to continue if it is
# not there afterwards.
awk -v valid_until="$valid_until" -v baseline="$baseline_published" '
    /^Valid-Until:/ { next }
    /^Claw-Os-Release-Security-Baseline:/ { next }
    { print }
    /^Date:/ {
        printf "Valid-Until: %s\n", valid_until
        if (baseline == "1") {
            print "Claw-Os-Release-Security-Baseline: 1"
        }
    }
' "dists/$SUITE/Release" > "dists/$SUITE/Release.tmp"
mv "dists/$SUITE/Release.tmp" "dists/$SUITE/Release"
if [ "$(grep -c '^Valid-Until:' "dists/$SUITE/Release")" != "1" ]; then
    echo "error: Release must carry exactly one Valid-Until field" >&2
    exit 1
fi
# The baseline field is what tells the *next* publication that this
# repository already guarantees release-security metadata, so a fetch
# failure can never be mistaken for a fresh repository. It is asserted
# to match what was really published, in both directions.
baseline_fields="$(grep -c '^Claw-Os-Release-Security-Baseline: 1$' \
    "dists/$SUITE/Release" || true)"
if [ "$baseline_fields" != "$baseline_published" ]; then
    echo "error: Release advertises $baseline_fields baseline marker(s) but" >&2
    echo "       the verifier published baseline=$baseline_published" >&2
    exit 1
fi
grep -q '^Acquire-By-Hash: yes' "dists/$SUITE/Release" \
    || echo "warning: this apt-ftparchive did not advertise Acquire-By-Hash" >&2

# `apt-ftparchive release` only hashes the index files APT itself
# fetches, so the release-security metadata would be authenticated only
# by its own detached signatures. That is not enough: a signature says
# who produced a file, not which published snapshot it belongs to, so an
# origin could pair a current index with an older, separately signed
# manifest it kept around. Listing those files in `Release` binds them
# to this snapshot, and the publisher cross-checks them on the next run.
if [ -d "dists/$SUITE/release-security" ]; then
    python3 - "dists/$SUITE/Release" "dists/$SUITE/release-security" <<'PY'
import hashlib
import pathlib
import sys

release = pathlib.Path(sys.argv[1])
metadata = pathlib.Path(sys.argv[2])
base = release.parent

entries = []
for path in sorted(metadata.rglob("*")):
    if not path.is_file():
        continue
    body = path.read_bytes()
    entries.append(
        (path.relative_to(base).as_posix(), hashlib.sha256(body).hexdigest(), len(body))
    )
if not entries:
    raise SystemExit(0)

lines = release.read_text(encoding="utf-8").splitlines()
try:
    start = lines.index("SHA256:")
except ValueError:
    raise SystemExit("error: the generated Release carries no SHA256 section")

end = start + 1
while end < len(lines) and lines[end].startswith(" "):
    end += 1

known = {line.split()[-1] for line in lines[start + 1 : end] if line.split()}
added = [
    f" {digest} {size:>16} {name}"
    for name, digest, size in entries
    if name not in known
]
release.write_text(
    "\n".join(lines[:end] + added + lines[end:]) + "\n", encoding="utf-8"
)
print(f"  :: bound {len(added)} release-security file(s) to the signed index")
PY
fi

# Sign Release in both formats and publish the exact binary keyring that
# Claw OS images pin with signed-by=.
#
# The passphrase never reaches argv: `claw_gpg_*` hands it to gpg on a
# pipe under --passphrase-fd 0.
echo ":: signing with GPG key $GPG_KEY_ID"
source "$PROJECT_DIR/packaging/release-security/gpg-sign.sh"
claw_gpg_sign_detached "$GPG_KEY_ID" "dists/$SUITE/Release" "dists/$SUITE/Release.gpg"
claw_gpg_sign_clear "$GPG_KEY_ID" "dists/$SUITE/Release" "dists/$SUITE/InRelease"
test -s "dists/$SUITE/InRelease"
test -s "dists/$SUITE/Release.gpg"
test -s "$REPO_DIR/claw-os-archive-keyring.gpg"
echo "  :: signed; public key at $REPO_DIR/claw-os-archive-keyring.gpg"

# GitHub Pages homepage. Keep this at the repo root so the APT paths remain
# stable: /dists/... and /pool/... are still served beside the marketing page.
#
# The React/Vite desktop lives under web/. CI builds it before repository
# assembly; its dist/ output includes the desktop and the embedded site.
SITE_DIR="${CLAW_OS_WEB_DIST_DIR:-$PROJECT_DIR/web/dist}"
if [ -f "$SITE_DIR/index.html" ]; then
    echo ":: copying built website from web/dist/"
    find "$SITE_DIR" -mindepth 1 -maxdepth 1 \
        -exec cp -R {} "$REPO_DIR/" \;

    GIT_SHA="$(git_readonly -C "$PROJECT_DIR" rev-parse --short HEAD 2>/dev/null || echo unknown)"
    # `sed -i` differs between BSD (macOS) and GNU. `-i.bak` is portable.
    for f in \
        "$REPO_DIR/site/index.html" \
        "$REPO_DIR/site/style.css" \
        "$REPO_DIR/site/app.js"; do
        [ -f "$f" ] || continue
        sed -i.bak \
            -e "s|@@GIT_SHA@@|$GIT_SHA|g" \
            -e "s|@@SUITE@@|$SUITE|g" \
            "$f"
        rm -f "$f.bak"
    done
else
    echo "error: built website not found at $SITE_DIR; run npm ci and npm run build in web/" >&2
    exit 1
fi

# GitHub Pages should publish the APT repository verbatim, without Jekyll
# filtering paths that begin with underscores or rewriting generated files.
: > "$REPO_DIR/.nojekyll"

echo ""
echo ":: apt repo ready at $REPO_DIR"
echo "   suite=$SUITE component=$COMPONENT arches=$arch_list"
