#!/usr/bin/env bash
# packaging/apt-repo/build-repo.sh — assemble an apt repository at
# build/apt-repo/ from the .debs in build/debs/.
#
# Layout produced (Debian "flat-and-pool" style):
#
#   build/apt-repo/
#   ├── dists/trixie/
#   │   ├── InRelease           (signed Release, omitted if no GPG key)
#   │   ├── Release             (always)
#   │   ├── Release.gpg         (detached signature, omitted if no GPG key)
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
# The repo is unsigned by default. Set GPG_KEY_ID to enable signing.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

DEBS_DIR="$PROJECT_DIR/build/debs"
REPO_DIR="$PROJECT_DIR/build/apt-repo"
SUITE="${SUITE:-trixie}"
COMPONENT="main"
GPG_KEY_ID="${GPG_KEY_ID:-}"

if [ ! -d "$DEBS_DIR" ] || [ -z "$(ls "$DEBS_DIR"/*.deb 2>/dev/null)" ]; then
    echo "error: no .debs in $DEBS_DIR — run packaging/deb/build-debs.sh first" >&2
    exit 1
fi

if ! command -v apt-ftparchive >/dev/null 2>&1; then
    echo "error: apt-ftparchive not found. Install it with: apt-get install apt-utils" >&2
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

# Sign the repo if a key is configured.
if [ -n "$GPG_KEY_ID" ]; then
    echo ":: signing with GPG key $GPG_KEY_ID"
    gpg --batch --yes --default-key "$GPG_KEY_ID" --detach-sign \
        --armor -o "dists/$SUITE/Release.gpg" "dists/$SUITE/Release"
    gpg --batch --yes --default-key "$GPG_KEY_ID" --clearsign \
        -o "dists/$SUITE/InRelease" "dists/$SUITE/Release"
    # Export the public key so users can fetch + trust it.
    gpg --armor --export "$GPG_KEY_ID" > "$REPO_DIR/claw-os.gpg"
    echo "  :: signed; public key at $REPO_DIR/claw-os.gpg"
else
    echo "  :: GPG_KEY_ID not set — repo is unsigned (use [trusted=yes])"
fi

# Index page for GitHub Pages. We list every binary-<arch>/Packages file
# so visitors can confirm at a glance which arches the repo covers.
{
    cat <<EOF
<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>Claw OS apt repo</title></head>
<body>
<h1>Claw OS apt repository</h1>
<p>To add this repo to a Debian or Ubuntu system:</p>
<pre>
echo "deb [trusted=yes] https://xiaoyu-work.github.io/claw-os $SUITE $COMPONENT" \\
  | sudo tee /etc/apt/sources.list.d/claw-os.list
sudo apt update
sudo apt install claw-os-base
</pre>
<p>Available indexes:</p>
<ul>
EOF
    for a in "${binary_arches[@]}" all; do
        echo "  <li><a href=\"dists/$SUITE/$COMPONENT/binary-$a/Packages\">binary-$a / Packages</a></li>"
    done
    cat <<EOF
</ul>
<p>Source: <a href="https://github.com/xiaoyu-work/claw-os">github.com/xiaoyu-work/claw-os</a></p>
</body></html>
EOF
} > "$REPO_DIR/index.html"

echo ""
echo ":: apt repo ready at $REPO_DIR"
echo "   suite=$SUITE component=$COMPONENT arches=$arch_list"
