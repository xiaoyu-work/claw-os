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
#   │   └── main/binary-amd64/
#   │       ├── Packages
#   │       └── Packages.gz
#   └── pool/main/c/claw-os-base/claw-os-base_<v>_amd64.deb
#       pool/main/c/claw-os-browser/claw-os-browser_<v>_amd64.deb
#       pool/main/c/claw-os-systemd/claw-os-systemd_<v>_all.deb
#
# The repo is unsigned by default. Set GPG_KEY_ID to enable signing.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

DEBS_DIR="$PROJECT_DIR/build/debs"
REPO_DIR="$PROJECT_DIR/build/apt-repo"
SUITE="${SUITE:-trixie}"
COMPONENT="main"
ARCH="amd64"
GPG_KEY_ID="${GPG_KEY_ID:-}"

if [ ! -d "$DEBS_DIR" ] || [ -z "$(ls "$DEBS_DIR"/*.deb 2>/dev/null)" ]; then
    echo "error: no .debs in $DEBS_DIR — run packaging/deb/build-debs.sh first" >&2
    exit 1
fi

if ! command -v apt-ftparchive >/dev/null 2>&1; then
    echo "error: apt-ftparchive not found. Install it with: apt-get install apt-utils" >&2
    exit 1
fi

echo ":: building apt repo at $REPO_DIR"
rm -rf "$REPO_DIR"
mkdir -p "$REPO_DIR/dists/$SUITE/$COMPONENT/binary-$ARCH"
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

# Generate Packages file for binary-amd64 (covers Architecture: amd64 and all).
cd "$REPO_DIR"
echo ":: generating Packages.gz"
apt-ftparchive packages "pool/$COMPONENT" \
    > "dists/$SUITE/$COMPONENT/binary-$ARCH/Packages"
gzip -fk9 "dists/$SUITE/$COMPONENT/binary-$ARCH/Packages"

# Architecture: all packages also need to appear under binary-all, but apt
# resolves them via binary-amd64 too as long as Packages includes them.
# Per Debian policy we mirror the file so older apt clients can find them.
cp "dists/$SUITE/$COMPONENT/binary-$ARCH/Packages" \
   "dists/$SUITE/$COMPONENT/binary-all/Packages"
gzip -fk9 "dists/$SUITE/$COMPONENT/binary-all/Packages"

# Generate the Release file.
echo ":: generating Release"
cat > "$REPO_DIR/apt-ftparchive-release.conf" <<EOF
APT::FTPArchive::Release::Origin "Claw OS";
APT::FTPArchive::Release::Label "Claw OS";
APT::FTPArchive::Release::Suite "$SUITE";
APT::FTPArchive::Release::Codename "$SUITE";
APT::FTPArchive::Release::Architectures "$ARCH all";
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

# Index page for GitHub Pages.
cat > "$REPO_DIR/index.html" <<EOF
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
<p>Available packages: <a href="dists/$SUITE/$COMPONENT/binary-$ARCH/Packages">Packages</a></p>
<p>Source: <a href="https://github.com/xiaoyu-work/claw-os">github.com/xiaoyu-work/claw-os</a></p>
</body></html>
EOF

echo ""
echo ":: apt repo ready at $REPO_DIR"
echo "   suite=$SUITE component=$COMPONENT arch=$ARCH"
