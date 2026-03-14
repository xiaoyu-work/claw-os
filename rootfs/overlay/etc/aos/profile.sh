#!/bin/bash
# Agent OS shell profile — sourced on agent login.
export AOS_VERSION="0.2.0"
export WORKSPACE="/workspace"
export PATH="/usr/local/bin:$PATH"
cd "$WORKSPACE" 2>/dev/null || true
