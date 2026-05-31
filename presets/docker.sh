#!/usr/bin/env bash
# presets/docker.sh — Claw OS as a Docker image.
#
# Builds the headless Claw OS Docker image: the full non-desktop OS
# runtime (cos / clawd / browser stack) packaged as a container. No
# desktop UI, no boot/VM-only features — containers share the host kernel.
#
# Uses the docker target's own default feature set (base, cos-core,
# browser, systemd, apt-source, qwen3-embedding), so no FEATURES override
# is needed.
#
# Usage:
#   ./presets/docker.sh           # docker build does not require root
#
# Output: a local Docker image (see targets/docker/build.sh for the tag).

set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"

run_preset docker
