#!/usr/bin/env bash
# presets/wsl.sh — Claw OS as an installable WSL2 distribution.
#
# Builds a modern WSL distribution containing the headless Claw OS
# runtime (cos / clawd / browser stack, no COSMIC desktop — WSL has no
# graphical session). Install it on Windows with:
#
#     wsl --install --from-file build\claw-os-wsl-<arch>.wsl \
#         --name ClawOS --location C:\ClawOS --version 2
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
# Output: build/claw-os-wsl-<arch>.wsl

set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"

run_preset wsl
