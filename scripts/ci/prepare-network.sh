#!/usr/bin/env bash
set -euo pipefail

if [[ "${GITHUB_ACTIONS:-}" != true || "${RUNNER_ENVIRONMENT:-}" != github-hosted ]]; then
    echo "Network preparation is restricted to disposable GitHub-hosted runners" >&2
    exit 1
fi

if getent ahosts archive.ubuntu.com >/dev/null; then
    exit 0
fi

interface="$(ip -4 route show default | awk 'NR == 1 { print $5 }')"
if [[ -z "$interface" ]]; then
    echo "Hosted runner has no default IPv4 interface" >&2
    exit 1
fi

sudo -n resolvectl dns "$interface" 1.1.1.1 8.8.8.8
sudo -n resolvectl flush-caches
for host in azure.archive.ubuntu.com archive.ubuntu.com security.ubuntu.com; do
    getent ahosts "$host" >/dev/null
done
