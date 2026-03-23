#!/bin/bash
# Claw OS shell profile — sourced on agent login.
export COS_VERSION="0.2.0"
export WORKSPACE="/workspace"
export PATH="/usr/local/bin:$PATH"
cd "$WORKSPACE" 2>/dev/null || true
