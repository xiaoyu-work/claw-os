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
#   └── pool/main/c/claw-os-base/claw-os-base_<v>_<arch>.deb
#       pool/main/c/claw-os-browser/claw-os-browser_<v>_<arch>.deb
#       pool/main/c/claw-os-systemd/claw-os-systemd_<v>_all.deb
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

DEBS_DIR="$PROJECT_DIR/build/debs"
REPO_DIR="$PROJECT_DIR/build/apt-repo"
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
    # claw-os-base_0.1.0_amd64.deb -> amd64
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

if [ ${#binary_arches[@]} -eq 0 ]; then
    echo "error: no architecture-specific .debs found in $DEBS_DIR" >&2
    exit 1
fi

echo ":: building apt repo at $REPO_DIR"
echo ":: arches: ${binary_arches[*]}"

rm -rf "$REPO_DIR"
for a in "${binary_arches[@]}"; do
    mkdir -p "$REPO_DIR/dists/$SUITE/$COMPONENT/binary-$a"
done
mkdir -p "$REPO_DIR/dists/$SUITE/$COMPONENT/binary-all"
mkdir -p "$REPO_DIR/assets/brand"
cp "$BRAND_ASSETS_DIR/clawos-wordmark.png" \
   "$BRAND_ASSETS_DIR/clawos-favicon-64.png" \
   "$REPO_DIR/assets/brand/"
if [ -f "$BRAND_ASSETS_DIR/og.png" ]; then
    cp "$BRAND_ASSETS_DIR/og.png" "$REPO_DIR/assets/brand/"
fi

# Move each .deb into pool/main/c/<package-name>/.
for deb in "$DEBS_DIR"/*.deb; do
    name="$(basename "$deb")"
    # claw-os-base_0.1.0_amd64.deb -> claw-os-base
    pkg="${name%%_*}"
    pool_dir="$REPO_DIR/pool/$COMPONENT/c/$pkg"
    mkdir -p "$pool_dir"
    cp "$deb" "$pool_dir/"
    echo "  :: pool/$COMPONENT/c/$pkg/$name"
done

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
arch_list="${binary_arches[*]} all"
cat > "$REPO_DIR/apt-ftparchive-release.conf" <<EOF
APT::FTPArchive::Release::Origin "Claw OS";
APT::FTPArchive::Release::Label "Claw OS";
APT::FTPArchive::Release::Suite "$SUITE";
APT::FTPArchive::Release::Codename "$SUITE";
APT::FTPArchive::Release::Architectures "$arch_list";
APT::FTPArchive::Release::Components "$COMPONENT";
APT::FTPArchive::Release::Description "Claw OS apt repository";
EOF

apt-ftparchive -c="$REPO_DIR/apt-ftparchive-release.conf" \
    release "dists/$SUITE" > "dists/$SUITE/Release"

rm -f "$REPO_DIR/apt-ftparchive-release.conf"

# Sign Release in both formats and publish the exact binary keyring that
# Claw OS images pin with signed-by=.
echo ":: signing with GPG key $GPG_KEY_ID"
gpg_args=(--batch --yes --pinentry-mode loopback --default-key "$GPG_KEY_ID")
if [ -n "$GPG_PASSPHRASE" ]; then
    gpg_args+=(--passphrase "$GPG_PASSPHRASE")
fi
gpg "${gpg_args[@]}" --detach-sign \
    --armor -o "dists/$SUITE/Release.gpg" "dists/$SUITE/Release"
gpg "${gpg_args[@]}" --clearsign \
    -o "dists/$SUITE/InRelease" "dists/$SUITE/Release"
gpg --batch --export "$GPG_KEY_ID" > "$REPO_DIR/claw-os-archive-keyring.gpg"
gpg --batch --armor --export "$GPG_KEY_ID" > "$REPO_DIR/claw-os-archive-keyring.asc"
test -s "dists/$SUITE/InRelease"
test -s "dists/$SUITE/Release.gpg"
test -s "$REPO_DIR/claw-os-archive-keyring.gpg"
echo "  :: signed; public key at $REPO_DIR/claw-os-archive-keyring.gpg"

# GitHub Pages homepage. Keep this at the repo root so the APT paths remain
# stable: /dists/... and /pool/... are still served beside the marketing page.
#
# The marketing site lives under packaging/apt-repo/site/ as plain HTML/CSS/JS
# so it can be iterated on without escaping bash heredocs. We copy it into the
# repo root and substitute build-time tokens (git sha, suite) in-place.
SITE_DIR="$SCRIPT_DIR/site"
if [ -d "$SITE_DIR" ]; then
    echo ":: copying marketing site from packaging/apt-repo/site/"
    # Avoid copying the OG generator script into the published repo.
    find "$SITE_DIR" -mindepth 1 -maxdepth 1 \
        ! -name '*.py' -exec cp -R {} "$REPO_DIR/" \;

    GIT_SHA="$(git -C "$PROJECT_DIR" rev-parse --short HEAD 2>/dev/null || echo unknown)"
    # `sed -i` differs between BSD (macOS) and GNU. `-i.bak` is portable.
    for f in "$REPO_DIR/index.html" "$REPO_DIR/style.css" "$REPO_DIR/app.js"; do
        [ -f "$f" ] || continue
        sed -i.bak \
            -e "s|@@GIT_SHA@@|$GIT_SHA|g" \
            -e "s|@@SUITE@@|$SUITE|g" \
            "$f"
        rm -f "$f.bak"
    done
else
    echo "warning: $SITE_DIR not found — apt repo will not have a homepage" >&2
fi

# GitHub Pages should publish the APT repository verbatim, without Jekyll
# filtering paths that begin with underscores or rewriting generated files.
: > "$REPO_DIR/.nojekyll"

echo ""
echo ":: apt repo ready at $REPO_DIR"
echo "   suite=$SUITE component=$COMPONENT arches=$arch_list"
