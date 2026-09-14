#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != Linux ]]; then
    printf '%s\n' 'Build the Codex TUI on Linux (Ubuntu under WSL on Windows).' >&2
    exit 1
fi

export HOME
HOME="$(getent passwd "$(id -u)" | cut -d: -f6)"
if [[ -z "$HOME" ]]; then
    printf '%s\n' 'Cannot resolve the current Linux account home.' >&2
    exit 1
fi
export PATH="$HOME/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

module_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
exec python3 -B "$module_dir/build.py" "$@"
