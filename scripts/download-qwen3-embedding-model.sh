#!/usr/bin/env bash
# Download the validated Qwen3 embedding ONNX GenAI bundle into Claw OS.
#
# Default target layout:
#   /var/lib/cos/models/qwen3-embedding-0.6b/v1/
#
# For image builds, pass --rootfs <path> so the model lands inside the rootfs:
#   scripts/download-qwen3-embedding-model.sh --rootfs build/claw-os-rootfs

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

HF_REPO="${COS_QWEN3_HF_REPO:-johnucm/Qwen-Qwen3-Embedding-0.6B-onnx}"
HF_REVISION="${COS_QWEN3_HF_REVISION:-6051d5d707ee165ff73b92905917099fd3298af1}"
MODEL_NAME="${COS_QWEN3_MODEL_NAME:-qwen3-embedding-0.6b}"
MODEL_VERSION="${COS_QWEN3_MODEL_VERSION:-v1}"

DEFAULT_ORT_GENAI_VERSION="$(
    sed -n 's/^pub const ORT_GENAI_KNOWN_GOOD_VERSION: &str = "\(.*\)";/\1/p' \
        "$PROJECT_DIR/core/src/engine_pkg/mod.rs" 2>/dev/null | head -1
)"
ORT_GENAI_VERSION="${COS_QWEN3_ORT_GENAI_VERSION:-${DEFAULT_ORT_GENAI_VERSION:-0.12.2}}"

DEFAULT_FILES=(
    added_tokens.json
    chat_template.jinja
    config.json
    genai_config.json
    generation_config.json
    merges.txt
    model.onnx
    model.onnx.data
    special_tokens_map.json
    tokenizer.json
    tokenizer_config.json
    vocab.json
)

ROOTFS=""
DEST_DIR=""
FORCE=0
DRY_RUN=0

usage() {
    cat <<EOF
Usage: $0 [--rootfs <path> | --dest <path>] [--revision <rev>] [--force] [--dry-run]

Downloads Hugging Face model $HF_REPO into the Claw OS model registry layout.

Options:
  --rootfs <path>     Install into <path>/var/lib/cos/models/$MODEL_NAME/$MODEL_VERSION
  --dest <path>       Install directly into an explicit model version directory
  --revision <rev>    Hugging Face revision/commit (default: $HF_REVISION)
  --force             Replace an existing incomplete or complete destination
  --dry-run           Print planned downloads without writing files
  -h, --help          Show this help

Environment overrides:
  COS_QWEN3_HF_REPO, COS_QWEN3_HF_REVISION, COS_QWEN3_MODEL_NAME,
  COS_QWEN3_MODEL_VERSION, COS_QWEN3_FILES, COS_QWEN3_ORT_GENAI_VERSION
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --rootfs)
            ROOTFS="${2:?--rootfs requires a path}"
            shift 2
            ;;
        --rootfs=*)
            ROOTFS="${1#--rootfs=}"
            shift
            ;;
        --dest)
            DEST_DIR="${2:?--dest requires a path}"
            shift 2
            ;;
        --dest=*)
            DEST_DIR="${1#--dest=}"
            shift
            ;;
        --revision)
            HF_REVISION="${2:?--revision requires a value}"
            shift 2
            ;;
        --revision=*)
            HF_REVISION="${1#--revision=}"
            shift
            ;;
        --force)
            FORCE=1
            shift
            ;;
        --dry-run)
            DRY_RUN=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "error: unknown argument: $1" >&2
            usage >&2
            exit 1
            ;;
    esac
done

if [ -n "$ROOTFS" ] && [ -n "$DEST_DIR" ]; then
    echo "error: pass only one of --rootfs or --dest" >&2
    exit 1
fi

if [ -n "$ROOTFS" ]; then
    DEST_DIR="$ROOTFS/var/lib/cos/models/$MODEL_NAME/$MODEL_VERSION"
elif [ -z "$DEST_DIR" ]; then
    DEST_DIR="/var/lib/cos/models/$MODEL_NAME/$MODEL_VERSION"
fi

if [ -n "${COS_QWEN3_FILES:-}" ]; then
    # shellcheck disable=SC2206 # intentional whitespace splitting for test overrides
    FILES=(${COS_QWEN3_FILES})
else
    FILES=("${DEFAULT_FILES[@]}")
fi

for f in "${FILES[@]}"; do
    case "$f" in
        ""|/*|../*|*/../*|*/..|.|./*|*/./*)
            echo "error: invalid model file path: $f" >&2
            exit 1
            ;;
    esac
done

base_url="https://huggingface.co/$HF_REPO/resolve/$HF_REVISION"

is_complete() {
    local dir="$1"
    [ -f "$dir/manifest.json" ] || return 1
    local f
    for f in "${FILES[@]}"; do
        [ -s "$dir/$f" ] || return 1
    done
}

if [ "$DRY_RUN" = "1" ]; then
    echo "repo=$HF_REPO"
    echo "revision=$HF_REVISION"
    echo "dest=$DEST_DIR"
    printf '%s\n' "${FILES[@]}" | sed "s#^#$base_url/#"
    exit 0
fi

if is_complete "$DEST_DIR" && [ "$FORCE" != "1" ]; then
    echo ":: Qwen3 embedding model already installed at $DEST_DIR"
    exit 0
fi

if [ -e "$DEST_DIR" ]; then
    if [ "$FORCE" = "1" ]; then
        rm -rf "$DEST_DIR"
    else
        echo "error: destination exists but is incomplete: $DEST_DIR" >&2
        echo "       re-run with --force to replace it" >&2
        exit 1
    fi
fi

parent="$(dirname "$DEST_DIR")"
mkdir -p "$parent"
staging="$parent/.${MODEL_VERSION}.download.$$"
rm -rf "$staging"
mkdir -p "$staging"

cleanup() {
    rm -rf "$staging"
}
trap cleanup EXIT

echo ":: downloading $HF_REPO@$HF_REVISION"
echo ":: destination: $DEST_DIR"

for f in "${FILES[@]}"; do
    url="$base_url/$f"
    out="$staging/$f"
    mkdir -p "$(dirname "$out")"
    echo "  :: $f"
    curl -fL --retry 5 --retry-delay 2 --connect-timeout 30 \
        --continue-at - \
        --output "$out" \
        "$url"
done

python3 - "$staging" "$MODEL_NAME" "$MODEL_VERSION" "$ORT_GENAI_VERSION" "${FILES[@]}" <<'PY'
import hashlib
import json
import os
import sys

root, name, version, ort_genai_version, *files = sys.argv[1:]
tree = hashlib.sha256()
total = 0
for rel in sorted(files):
    path = os.path.join(root, rel)
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1024 * 1024), b""):
            h.update(chunk)
    digest = h.hexdigest()
    total += os.path.getsize(path)
    tree.update(rel.encode("utf-8"))
    tree.update(b"\0")
    tree.update(digest.encode("ascii"))
    tree.update(b"\n")

manifest = {
    "name": name,
    "version": version,
    "task": "embed",
    "engine": "ort-genai",
    "format": "onnx-genai",
    "sha256": tree.hexdigest(),
    "size": total,
    "files": sorted(files),
    "default_device": None,
    "params": {},
    "requires_engine": {
        "name": "ort-genai",
        "version": f"={ort_genai_version}",
    },
    "gguf_version": None,
    "arch": None,
}

with open(os.path.join(root, "manifest.json"), "w", encoding="utf-8") as fh:
    json.dump(manifest, fh, indent=2)
    fh.write("\n")

print(f":: manifest sha256={manifest['sha256']} size={total}")
PY

mv "$staging" "$DEST_DIR"
trap - EXIT
echo ":: installed Qwen3 embedding model at $DEST_DIR"
