#!/usr/bin/env bash
# presets/desktop.sh — full COSMIC desktop VM (VMware .vmdk).
#
# Builds a 50 GB VMware disk with the complete Claw OS desktop, VMware
# guest tools (auto screen resize + clipboard), and the Copilot CLI.
# Import the resulting .vmdk into VMware Workstation / Fusion / ESXi.
#
# This is the long (30–60 min) build because it compiles the whole COSMIC
# desktop. The desktop/target/ cargo cache on the host makes re-runs
# incremental, so only changed crates recompile.
#
# Usage:
#   sudo ./presets/desktop.sh
#
# Output: build/claw-os-vm-<arch>.vmdk

set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"

FEATURES="$FEATURES_DESKTOP"
FORMATS="${FORMATS:-vmdk}"
# The fully-installed desktop fills ~16 GB; a 16 GB disk boots to 100% full
# and cosmic-comp panics with ENOSPC writing its config. 50 GB leaves the
# user real working room. The raw image is sparse, so the file stays small
# until actually used.
SIZE="${SIZE:-50G}"

run_preset vm
