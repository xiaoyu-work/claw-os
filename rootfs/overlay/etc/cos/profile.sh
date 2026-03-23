#!/bin/bash
# Claw OS shell profile — sourced on agent login.
export COS_VERSION="0.3.0"
export WORKSPACE="/workspace"
export PATH="/usr/local/bin:$PATH"

# Agent-native: suppress all interactive prompts.
# No command should ever block waiting for human input.
export DEBIAN_FRONTEND=noninteractive
export GIT_TERMINAL_PROMPT=0
export CI=true
export PAGER=cat
export GIT_PAGER=cat
export PIP_NO_INPUT=1
export NPM_CONFIG_YES=true
export PYTHONDONTWRITEBYTECODE=1
export NEEDRESTART_MODE=a
export APT_LISTCHANGES_FRONTEND=none

cd "$WORKSPACE" 2>/dev/null || true
