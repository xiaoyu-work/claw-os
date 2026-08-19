#!/usr/bin/env bash
# presets/_common.sh — shared helper for the preset build scripts.
#
# Presets are thin wrappers around the core build pipeline (./build.sh
# <target>, which delegates to targets/<target>/build.sh). They exist so
# you don't have to remember the long FEATURES / FORMATS / SIZE strings:
# each preset exports a known-good combination and hands off to the core.
#
# This file is meant to be sourced, never run directly.

# Resolve the repo root from the location of this file so presets work
# regardless of the caller's working directory.
PRESETS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$PRESETS_DIR/.." && pwd)"
source "$PROJECT_DIR/scripts/lib/image-profiles.sh"

# Desktop feature set: the full COSMIC desktop + VMware guest tools (screen
# auto-resize, clipboard) + Copilot CLI + the Claw OS apt repo. This is the
# one feature string that the core's default does NOT cover, which is why a
# preset is useful here. The wsl/docker targets already default to the
# right features, so those presets don't override FEATURES at all.
FEATURES_DESKTOP="$IMAGE_FEATURES_DESKTOP_VM"

# run_preset <target> — export whatever FEATURES/FORMATS/SIZE the caller
# set (any may be empty, in which case the target's own default applies)
# and hand off to the core dispatcher. exec replaces this process so the
# core build's exit status becomes ours.
run_preset() {
    local target="$1"
    echo ":: preset -> ./build.sh $target"
    [ -n "${FEATURES:-}" ] && echo "::   FEATURES = $FEATURES"
    [ -n "${FORMATS:-}" ]  && echo "::   FORMATS  = $FORMATS"
    [ -n "${SIZE:-}" ]     && echo "::   SIZE     = $SIZE"
    [ -n "${FEATURES:-}" ] && export FEATURES
    [ -n "${FORMATS:-}" ]  && export FORMATS
    [ -n "${SIZE:-}" ]     && export SIZE
    exec "$PROJECT_DIR/build.sh" "$target"
}
