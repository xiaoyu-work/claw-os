#!/usr/bin/env bash
# presets/desktop.sh — Claw OS desktop VM (VMware .vmdk).
#
# Builds a 50 GB VMware disk with the complete Claw OS desktop, VMware
# guest tools (auto screen resize + clipboard), and the Copilot CLI.
# Import the resulting .vmdk into VMware Workstation / Fusion / ESXi.
#
# Requires authenticated Desktop Shell package inputs and builds the native compositor.
#
# Usage:
#   sudo ./presets/desktop.sh
#
# Output: build/claw-os-vm-<arch>.vmdk

set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"

FEATURES="$FEATURES_DESKTOP"
FORMATS="${FORMATS:-vmdk}"
# Leave working space for the desktop and installed Apps.
SIZE="${SIZE:-50G}"

run_preset vm
