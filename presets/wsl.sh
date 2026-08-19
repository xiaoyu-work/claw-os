#!/usr/bin/env bash
# presets/wsl.sh — Claw OS as an importable WSL2 distribution.
#
# Builds a WSL root filesystem tarball containing the headless Claw OS
# runtime (cos / clawd / browser stack, no COSMIC desktop — WSL has no
# graphical session). Import it on Windows with:
#
#     New-Item -ItemType Directory -Force -Path C:\ClawOS | Out-Null
#     wsl --import ClawOS C:\ClawOS build\claw-os-wsl-<arch>.tar
#     wsl -d ClawOS
#
# Note: this produces the Claw OS *distro* you run inside WSL. It is a
# build artifact, distinct from the Ubuntu WSL you build it in.
#
# Uses the wsl target's own default feature set (base, cos-core, browser,
# systemd, apt-source, qwen3-embedding), so no FEATURES override is needed.
#
# Usage:
#   sudo ./presets/wsl.sh
#
# Output: build/claw-os-wsl-<arch>.tar

set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"

run_preset wsl
