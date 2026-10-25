#!/usr/bin/env bash
set -euo pipefail

cd "${GITHUB_WORKSPACE:?GITHUB_WORKSPACE is required}"
: "${GITHUB_OUTPUT:?GITHUB_OUTPUT is required}"
target="${RELEASE_TARGET:?RELEASE_TARGET is required}"
agent=false base=false desktop=false images=false pages=false
case "$target" in
    agent) agent=true; pages=true ;;
    base) base=true; pages=true ;;
    desktop) desktop=true; pages=true ;;
    images) agent=true; base=true; images=true ;;
    website) pages=true ;;
    all) agent=true; base=true; desktop=true; images=true; pages=true ;;
    *) echo "error: unknown release target '$target'" >&2; exit 2 ;;
esac
if [ "${RELEASE_SECURITY_BOOTSTRAP:-0}" = "1" ] \
    && [[ "$target" = images || "$target" = website ]]; then
    echo "error: release-security bootstrap requires a package publication target" >&2
    exit 2
fi

version="$(bash scripts/lib/package-version.sh)"
mkdir -p build/release
if [ "$agent" = true ] || [ "$desktop" = true ]; then
    # Freeze the ref without an extra checkout/LFS download. Build jobs
    # verify and stage that exact commit through the existing source helper.
    python3 - <<'PY'
from scripts import app_sources

selection = app_sources.read_lock()
if selection["version"] == 2:
    selection = {
        **selection,
        "version": 1,
        "revision": app_sources.resolve_main(selection),
    }
app_sources.write_lock("build/release/app-sources.json", selection)
PY
fi
{
    printf 'version=%s\n' "$version"
    printf 'agent=%s\nbase=%s\ndesktop=%s\nimages=%s\npages=%s\n' \
        "$agent" "$base" "$desktop" "$images" "$pages"
} >> "$GITHUB_OUTPUT"
