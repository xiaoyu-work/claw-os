#!/usr/bin/env bash
# rootfs/features/apt-source/install.sh — configure the Claw OS apt repo on
# the installed system. After this runs, `apt update` will see claw-os-base,
# claw-os-browser and claw-os-systemd as upgradeable packages.
#
# Inherited from environment: ROOTFS.
#
# Overridable env vars:
#   COS_APT_REPO_URL    — repo base URL (default: official GH Pages)
#   COS_APT_REPO_SUITE  — suite name    (default: trixie)

set -euo pipefail

COS_APT_REPO_URL="${COS_APT_REPO_URL:-https://xiaoyu-work.github.io/claw-os}"
COS_APT_REPO_SUITE="${COS_APT_REPO_SUITE:-trixie}"

echo "  :: writing /etc/apt/sources.list.d/claw-os.list"
mkdir -p "$ROOTFS/etc/apt/sources.list.d"

# v1: the repo is unsigned. trusted=yes is required for apt to accept it.
# When CI gains a signing key, swap to `[signed-by=/etc/apt/keyrings/claw-os.gpg]`
# and ship the public key alongside this script.
cat > "$ROOTFS/etc/apt/sources.list.d/claw-os.list" <<EOF
# Claw OS — official package repository.
# Source: https://github.com/xiaoyu-work/claw-os
deb [trusted=yes] $COS_APT_REPO_URL $COS_APT_REPO_SUITE main
EOF

echo "  :: apt source ready ($COS_APT_REPO_URL $COS_APT_REPO_SUITE main)"
