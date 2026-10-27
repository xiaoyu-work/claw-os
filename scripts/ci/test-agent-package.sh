#!/usr/bin/env bash
set -euo pipefail

cd "${GITHUB_WORKSPACE:?GITHUB_WORKSPACE is required}"
: "${RUNNER_TEMP:?RUNNER_TEMP is required}"
source scripts/lib/arch.sh

tui() {
    if [ -e /proc/sys/kernel/apparmor_restrict_unprivileged_userns ]; then
        sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0
    fi
    cos_fixture="$(mktemp /tmp/claw-agent-tui-cos.XXXXXX)"
    trap 'rm -f "$cos_fixture"' EXIT
    install -m 0755 "target/$RUST_TARGET/release/cos" "$cos_fixture"
    original_namespace="$(readlink /proc/self/ns/mnt)"
    for scenario in complete slow-progress cancel commands confirmations durable-queue task-center task-controls approval-center approval-choice attachments appearance agents side platform memory-center hooks-center mcp-center extensions-center usage-center debug-center account-center voice-settings file-mentions copy-export raw-scrollback vim backtrack notification-inbox activity-lifecycle activity-controls activity-evidence activity-review workspace multiline multiline-key reconnect startup-reconnect resume resume-running plain; do
        unshare --user --map-current-user --keep-caps --mount --net \
            python3 -B core/tests/agent_tui_pty.py \
            --cos "$cos_fixture" \
            --case "$scenario" \
            --original-mount-namespace "$original_namespace" \
            --transcript "/tmp/claw-tui-$scenario.log"
    done
}

installed() {
    : "${COS_PACKAGE_VERSION:?COS_PACKAGE_VERSION is required}"
    file_version="${COS_PACKAGE_VERSION//:/%3a}"
    agent_deb="build/debs/claw-os-agent_${file_version}_${ARCH}.deb"
    install_deb="$(mktemp /tmp/claw-os-agent.XXXXXX.deb)"
    install_debs=("$install_deb")
    trap 'rm -f -- "${install_debs[@]}"' EXIT
    install -m 0644 "$agent_deb" "$install_deb"
    if [ "${1:-}" = core ]; then
        base_deb="build/debs/claw-os-base_${file_version}_all.deb"
        base_install_deb="$(mktemp /tmp/claw-os-base.XXXXXX.deb)"
        install_debs+=("$base_install_deb")
        install -m 0644 "$base_deb" "$base_install_deb"
    fi
    # Keep the real postinst scan away from transient build caches.
    sudo unshare --mount --propagation private \
        /bin/bash -euo pipefail -c '
            workspace=$1
            cargo_home=$2
            rustup_home=$3
            shift 3
            cd /tmp
            for path in "$workspace" "$cargo_home" "$rustup_home"; do
                if [ -d "$path" ]; then
                    mount -t tmpfs -o nodev,nosuid,noexec,size=1m tmpfs "$path"
                fi
            done
            apt-get install -y --no-install-recommends "$@"
            apt-get install -y --reinstall --no-install-recommends "$@"
        ' bash "$GITHUB_WORKSPACE" "$CARGO_HOME" \
        "${RUSTUP_HOME:-$HOME/.rustup}" "${install_debs[@]}"
    for attempt in {1..30}; do
        if sudo systemctl is-active --quiet clawd.service \
            && sudo test -S /run/cos/clawd.sock; then
            break
        fi
        sleep 1
    done
    if ! sudo systemctl is-enabled --quiet clawd.service \
        || ! sudo systemctl is-active --quiet clawd.service \
        || ! sudo test -S /run/cos/clawd.sock; then
        sudo systemctl status clawd.service --no-pager || true
        sudo journalctl -u clawd.service --no-pager -n 100 || true
        exit 1
    fi
    test -x /usr/bin/pkexec
    test -x /usr/bin/pkttyagent
    test -x /usr/local/bin/claw-approval-helper
    sudo /usr/lib/cos/bin/claw-security-floor verify-installed
    original_mount="$(readlink /proc/self/ns/mnt)"
    original_pid="$(readlink /proc/self/ns/pid)"
    sudo unshare --mount --pid --fork --mount-proc --net \
        python3 -B core/tests/approval_polkit_process.py \
        --cos /usr/local/bin/cos \
        --clawd /usr/local/bin/clawd \
        --helper /usr/local/bin/claw-approval-helper \
        --policy /usr/share/polkit-1/actions/org.clawos.approval.policy \
        --original-mount-namespace "$original_mount" \
        --original-pid-namespace "$original_pid"
    /usr/local/bin/cos-browser --help >/dev/null
    COS_USER_DATA_DIR="$RUNNER_TEMP/cos-user-data" \
        /usr/local/bin/cos agent skills list \
        | python3 -c 'import json,sys; data=json.load(sys.stdin); assert "claw-os" in data["names"]'
    COS_USER_DATA_DIR="$RUNNER_TEMP/cos-user-data" \
        /usr/local/bin/cos agent skills info claw-os \
        | python3 -c 'import json,sys; data=json.load(sys.stdin); assert data["source"] == "builtin"'
    timeout 15s sudo /usr/local/bin/cos agent service status \
        | python3 -c 'import json,sys; data=json.load(sys.stdin); assert data["status"] == "running", data; assert data["daemon"] == "clawd", data'
    service_cgroup=/sys/fs/cgroup/system.slice/clawd.service
    main_pid="$(systemctl show clawd.service --property MainPID --value)"
    test -n "$main_pid"
    test -z "$(sudo cat "$service_cgroup/cgroup.procs")"
    sudo grep -qx "$main_pid" "$service_cgroup/cos-broker/cgroup.procs"
    for controller in cpu memory pids; do
        sudo grep -qw "$controller" "$service_cgroup/cgroup.subtree_control"
    done
}

case "${1:-}" in
    tui) tui ;;
    installed) installed ;;
    installed-core) installed core ;;
    *) echo "usage: $0 {tui|installed|installed-core}" >&2; exit 2 ;;
esac
