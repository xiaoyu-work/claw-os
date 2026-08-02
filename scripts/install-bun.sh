#!/usr/bin/env bash
set -euo pipefail
umask 077

# Pinned release assets and digests from the Bun GitHub release metadata.
BUN_VERSION=1.3.14

case "$(uname -m)" in
    x86_64|amd64)
        ASSET="bun-linux-x64.zip"
        SHA256="951ee2aee855f08595aeec6225226a298d3fea83a3dcd6465c09cbccdf7e848f"
        ;;
    aarch64|arm64)
        ASSET="bun-linux-aarch64.zip"
        SHA256="a27ffb63a8310375836e0d6f668ae17fa8d8d18b88c37c821c65331973a19a3b"
        ;;
    *)
        echo "error: unsupported architecture: $(uname -m)" >&2
        exit 1
        ;;
esac

: "${HOME:?HOME must be set}"
INSTALL_DIR="${BUN_INSTALL_DIR:-$HOME/.local/bin}"
case "$INSTALL_DIR" in
    /*) ;;
    *)
        echo "error: BUN_INSTALL_DIR must be an absolute path" >&2
        exit 1
        ;;
esac
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/claw-bun.XXXXXXXX")"
trap 'rm -rf "$WORK_DIR"' EXIT

ARCHIVE="$WORK_DIR/$ASSET"
URL="https://github.com/oven-sh/bun/releases/download/bun-v${BUN_VERSION}/${ASSET}"

echo ":: downloading Bun ${BUN_VERSION} (${ASSET})"
curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error \
    --output "$ARCHIVE" "$URL"
printf '%s  %s\n' "$SHA256" "$ARCHIVE" | sha256sum --check --status

# Extract only the expected executable member. Avoid extractall(), which
# would trust archive paths and permit traversal if an upstream ZIP changed.
python3 - "$ARCHIVE" "${ASSET%.zip}/bun" "$WORK_DIR/bun" <<'PY'
import shutil
import sys
import zipfile

archive, member, output = sys.argv[1:]
with zipfile.ZipFile(archive) as bundle:
    info = bundle.getinfo(member)
    if info.is_dir() or info.file_size <= 0:
        raise SystemExit(f"invalid Bun archive member: {member}")
    with bundle.open(info, "r") as source, open(output, "xb") as target:
        shutil.copyfileobj(source, target)
PY

mkdir -p "$INSTALL_DIR"
TEMP_DEST="$INSTALL_DIR/.bun.$$.tmp"
trap 'rm -rf "$WORK_DIR"; rm -f "$TEMP_DEST"' EXIT
install -m 0755 "$WORK_DIR/bun" "$TEMP_DEST"
mv -f "$TEMP_DEST" "$INSTALL_DIR/bun"

ACTUAL_VERSION="$("$INSTALL_DIR/bun" --version)"
if [ "$ACTUAL_VERSION" != "$BUN_VERSION" ]; then
    echo "error: installed Bun reports $ACTUAL_VERSION, expected $BUN_VERSION" >&2
    exit 1
fi

echo ":: installed Bun ${ACTUAL_VERSION} at $INSTALL_DIR/bun"
