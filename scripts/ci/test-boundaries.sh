#!/usr/bin/env bash
set -euo pipefail

cd "${GITHUB_WORKSPACE:?GITHUB_WORKSPACE is required}"
: "${RUNNER_TEMP:?RUNNER_TEMP is required}"
export CARGO_TARGET_DIR="$GITHUB_WORKSPACE/target"

cos_test_binary() {
    python3 - "$1" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    records = map(json.loads, handle)
    print(next(row["executable"] for row in records
               if row.get("executable") and row.get("profile", {}).get("test")
               and row.get("target", {}).get("name") == "cos"))
PY
}

private_extension_case() {
    local seconds=$1
    shift
    sudo timeout "$seconds" unshare --mount --net --fork \
        bash --noprofile --norc -c '
            set -euo pipefail
            mount --make-rprivate /
            ip link set lo up
            mount -t tmpfs -o mode=0755,nosuid,nodev tmpfs /run
            mkdir -m0755 /run/lock
            mount -t tmpfs -o mode=0755,nosuid,nodev tmpfs /var/lib
            mount -t tmpfs -o mode=0700,nosuid,nodev tmpfs /root
            mount -t tmpfs -o mode=0755,nosuid,nodev tmpfs /usr/local/bin
            install -o root -g root -m0755 "$1" /usr/local/bin/claw-app-runner
            shift
            exec "$@"
        ' private-extension-case "$CARGO_TARGET_DIR/debug/claw-app-runner" "$@"
}

nearby() {
    cargo test --manifest-path crates/claw-nearby/Cargo.toml --locked
    cargo clippy --manifest-path crates/claw-nearby/Cargo.toml --all-targets --locked -- -D warnings
    cargo test --manifest-path crates/claw-nearby/vendor/owlyd/Cargo.toml --test service --locked
    cargo test -p cos --lib --locked --no-run --message-format=json-render-diagnostics \
        > "$RUNNER_TEMP/nearby-tests.jsonl"
    test_binary="$(cos_test_binary "$RUNNER_TEMP/nearby-tests.jsonl")"
    sudo timeout 60 unshare --mount --net --fork \
        "$test_binary" clawd::nearby:: --include-ignored --test-threads=1
    python3 -B packaging/deb/tests/test_nearby.py
}

app_apt() {
    local program
    for program in apt-get dpkg dpkg-deb dpkg-query apt-ftparchive gpg gpgconf python3 unshare bwrap setfacl node google-chrome; do
        command -v "$program" > /dev/null
    done
    local node_binary
    node_binary="$(readlink -f -- "$(command -v node)")"
    python3 -I -c 'import apt_pkg'
    cargo build -p cos --bin cos --bin claw-app-runner --bin claw-app-apt-hook --bin claw-extension-host --locked
    cargo test -p cos --lib --locked --no-run --message-format=json-render-diagnostics \
        > "$CARGO_TARGET_DIR/app-apt-tests.jsonl"
    local test_binary listing fixture
    test_binary="$(cos_test_binary "$CARGO_TARGET_DIR/app-apt-tests.jsonl")"
    listing="$("$test_binary" --list)"
    local fixtures=(
        private_apt_bundle_requires_both_reviews_and_owns_shared_dependency
        private_apt_refuses_substituted_bytes_plan_and_status_before_dpkg
        private_apt_hook_identity_and_lost_reply_never_replay
        private_apt_completed_dispatch_failure_recovers_read_only
        private_apt_ordinary_self_handoff_retains_receipt
        private_apt_owner_withdrawal_denies_live_calls_and_retires_only_owner
        private_apt_global_stop_is_separate_and_unreviewed_effect_is_not_applied
        private_apt_retained_payload_lifecycle_and_nonzero_completion
        private_installed_product_web_calls_real_kv_without_model
    )
    for fixture in "${fixtures[@]}" private_apt_foreign_uid_probe private_apt_retained_peer private_apt_ordinary_actor; do
        grep -Fx -- "clawd::app_store::apt::tests::$fixture: test" \
            <<< "$listing" > /dev/null
    done
    local parent_mnt parent_pid parent_net
    parent_mnt="$(readlink /proc/self/ns/mnt)"
    parent_pid="$(readlink /proc/self/ns/pid)"
    parent_net="$(readlink /proc/self/ns/net)"
    for fixture in "${fixtures[@]}"; do
        sudo timeout --kill-after=5 150 unshare --mount --pid --net --fork --mount-proc --kill-child=KILL \
            env COS_APP_APT_TEST_HOOK="$CARGO_TARGET_DIR/debug/claw-app-apt-hook" \
                COS_APP_APT_TEST_COS="$CARGO_TARGET_DIR/debug/cos" \
                COS_APP_APT_TEST_RUNNER="$CARGO_TARGET_DIR/debug/claw-app-runner" \
                COS_APP_APT_TEST_HOST="$CARGO_TARGET_DIR/debug/claw-extension-host" \
                COS_APP_APT_TEST_NODE="$node_binary" \
                COS_APP_APT_PARENT_MNT="$parent_mnt" \
                COS_APP_APT_PARENT_PID="$parent_pid" \
                COS_APP_APT_PARENT_NET="$parent_net" \
            bash --noprofile --norc -c '
                set -euo pipefail
                mount --make-rprivate /
                # PID 1 supervises; authenticated service controllers require pid > 1.
                status=0
                "$1" --exact "$2" --ignored --nocapture --test-threads=1 || status=$?
                exit "$status"
            ' private-app-apt "$test_binary" "clawd::app_store::apt::tests::$fixture"
    done
}

app_bind() {
    cargo build -p cos --bin cos --bin claw-app-runner --locked
    sudo apt-get install -y --no-install-recommends acl
    cargo test -p cos --lib --locked --no-run --message-format=json-render-diagnostics \
        > "$CARGO_TARGET_DIR/ordinary-custody-tests.jsonl"
    local test_binary
    test_binary="$(cos_test_binary "$CARGO_TARGET_DIR/ordinary-custody-tests.jsonl")"
    local listing
    listing="$("$test_binary" --list)"
    local gate
    for gate in \
        real_runner_waits_for_binding_and_preserves_app_input \
        pre_sandbox_custody_gate_preserves_stdio_and_refuses_unbound_execution
    do
        grep -Fx -- "bridge::captured::tests::process::$gate: test" <<< "$listing" > /dev/null
        COS_CAPTURED_TEST_RUNNER="$CARGO_TARGET_DIR/debug/claw-app-runner" \
            "$test_binary" --exact "bridge::captured::tests::process::$gate" \
            --ignored --nocapture --test-threads=1
    done
    local fixture
    for fixture in \
        ordinary_root_standalone_retires_host_before_publication \
        ordinary_root_standalone_cancel_retry_publishes_only_after_cleanup \
        ordinary_root_task_host_retires_namespace_data_before_publication \
        ordinary_root_live_grandchild_blocks_publication_until_checked_retry \
        ordinary_root_foreign_and_lost_ack_cannot_authorize_publication \
        ordinary_root_indeterminate_provider_blocks_store_publication
    do
        grep -Fx -- "extension_host::spawn::task_data::tests::store_custody::$fixture: test" \
            <<< "$listing" > /dev/null
        sudo timeout 150 unshare --mount --net --fork \
            env COS_ORDINARY_TEST_COS="$CARGO_TARGET_DIR/debug/cos" \
                COS_ORDINARY_TEST_RUNNER="$CARGO_TARGET_DIR/debug/claw-app-runner" \
            bash --noprofile --norc -c '
                set -euo pipefail
                mount --make-rprivate /
                exec "$1" --exact "$2" --ignored --nocapture --test-threads=1
            ' private-ordinary-custody "$test_binary" \
            "extension_host::spawn::task_data::tests::store_custody::$fixture"
    done
}

calendar() {
    cargo build -p cos --bin cos --locked
    cargo build --manifest-path desktop/applets/claw-applet-services/Cargo.toml \
        --features provider --bins --locked --target-dir "$RUNNER_TEMP/calendar-target"
    cargo test -p cos --lib --locked --no-run --message-format=json-render-diagnostics \
        > "$RUNNER_TEMP/calendar-tests.jsonl"
    test_binary="$(cos_test_binary "$RUNNER_TEMP/calendar-tests.jsonl")"
    sudo timeout 120 unshare --mount --net --fork \
        env COS_CALENDAR_COS="$CARGO_TARGET_DIR/debug/cos" \
            COS_CALENDAR_READER="$RUNNER_TEMP/calendar-target/debug/claw-calendar-reader" \
            COS_CALENDAR_PROVIDER="$RUNNER_TEMP/calendar-target/debug/claw-os-applet-provider" \
        bash --noprofile --norc -c '
            set -euo pipefail
            mount --make-rprivate /
            exec "$1" --exact clawd::calendar::tests::process::calendar_mcp_to_broker_reader_and_public_sdk \
                --ignored --nocapture --test-threads=1
        ' private-calendar "$test_binary"
    sudo timeout 30 "$test_binary" \
        --exact worker::migrate::tests::root_calendar_migration_uses_the_authenticated_owner_not_root_euid \
        --ignored --nocapture --test-threads=1
}

extensions() {
    sudo apt-get install -y --no-install-recommends acl binutils
    cargo build -p cos --bin claw-agentd --bin claw-extension-host --bin claw-app-runner
    cargo test -p cos \
        --test agentd_process_boundary \
        --test extension_host_boundary \
        --no-run --message-format=json-render-diagnostics \
        > "$RUNNER_TEMP/extension-tests.jsonl"
    cargo test -p cos --lib --locked --no-run --message-format=json-render-diagnostics \
        > "$RUNNER_TEMP/service-retirement-tests.jsonl"
    service_test_binary="$(cos_test_binary "$RUNNER_TEMP/service-retirement-tests.jsonl")"
    private_extension_case 120 \
        env COS_SERVICE_RETIREMENT_HOST="$CARGO_TARGET_DIR/debug/claw-extension-host" \
        "$service_test_binary" --exact clawd::app_services::retirement::tests::process::service_retirement_keeps_custody_until_cleanup_succeeds \
            --ignored --nocapture --test-threads=1
    private_extension_case 30 "$service_test_binary" \
        --exact extension_host::identity::tests::checked_release_retries_authenticated_marker_cleanup \
        --ignored --nocapture --test-threads=1

    local restart_case=clawd::trust_withdrawal::tests::restart_process::root_controller_restart_keeps_unconfirmed_owner_scoped_outcome
    for fixture in "$restart_case" \
        clawd::trust_withdrawal::tests::restart_process::controller_process \
        clawd::trust_withdrawal::tests::restart_process::foreign_owner_process
    do
        "$service_test_binary" --list | grep -Fx "$fixture: test" > /dev/null
    done
    sudo timeout 120 "$service_test_binary" --exact "$restart_case" \
        --ignored --nocapture --test-threads=1

    readonly root=/opt/cos-boundary-tests
    sudo install -d -o root -g root -m 0755 "$root"
    cleanup() {
        sudo rm -rf -- "$root"
    }
    trap cleanup EXIT

    agentd_test=$(find target/debug/deps -maxdepth 1 -type f -perm -u+x \
        -name 'agentd_process_boundary-*' ! -name '*.d' -printf '%T@ %p\n' |
        sort -nr | head -n1 | cut -d' ' -f2-)
    extension_test=$(find target/debug/deps -maxdepth 1 -type f -perm -u+x \
        -name 'extension_host_boundary-*' ! -name '*.d' -printf '%T@ %p\n' |
        sort -nr | head -n1 | cut -d' ' -f2-)
    lib_test=
    while IFS= read -r candidate; do
        if "$candidate" --list 2>/dev/null |
            grep '^extension_host::child_isolation::tests::' >/dev/null; then
            lib_test=$candidate
            break
        fi
    done < <(find target/debug/deps -maxdepth 1 -type f -perm -u+x \
        -name 'cos-*' ! -name '*.d' ! -name '*.rlib' ! -name '*.rmeta' \
        -printf '%T@ %p\n' | sort -nr | cut -d' ' -f2-)
    test -n "$agentd_test"
    test -n "$extension_test"
    test -n "$lib_test"
    mapfile -t privileged_child_tests < <(
        "$lib_test" --list |
            sed -n 's/^\(extension_host::child_isolation::tests::[^:]*\): test$/\1/p'
    )
    readonly expected_privileged_child_tests=19
    if [ "${#privileged_child_tests[@]}" -ne "$expected_privileged_child_tests" ]; then
        printf 'expected %s privileged child tests, found %s\n' \
            "$expected_privileged_child_tests" "${#privileged_child_tests[@]}" >&2
        printf '%s\n' "${privileged_child_tests[@]}" >&2
        exit 1
    fi

    sudo install -o root -g root -m 0755 \
        target/debug/claw-agentd "$root/claw-agentd"
    sudo install -o root -g root -m 0755 \
        target/debug/claw-extension-host "$root/claw-extension-host"
    sudo install -o root -g root -m 0755 \
        target/debug/claw-app-runner "$root/claw-app-runner"
    sudo install -o root -g root -m 0755 \
        "$agentd_test" "$root/agentd_process_boundary"
    sudo install -o root -g root -m 0755 \
        "$extension_test" "$root/extension_host_boundary"
    sudo install -o root -g root -m 0755 \
        "$lib_test" "$root/cos_lib_tests"

    private_extension_case 120 env \
        HOME=/root \
        SUDO_UID="$(id -u)" \
        COS_PRIVILEGED_EXTENSION_HOST_BIN="$root/claw-extension-host" \
        "$root/agentd_process_boundary" --test-threads=1
    private_extension_case 120 env \
        HOME=/root \
        SUDO_UID="$(id -u)" \
        COS_PRIVILEGED_EXTENSION_HOST_BIN="$root/claw-extension-host" \
        "$root/extension_host_boundary" --test-threads=1
    private_extension_case 120 env \
        HOME=/root \
        SUDO_UID="$(id -u)" \
        COS_REQUIRE_PRIVILEGED_CHILD_TESTS=1 \
        "$root/cos_lib_tests" extension_host::child_isolation::tests \
        --test-threads=1
    private_extension_case 30 env \
        HOME=/root \
        SUDO_UID="$(id -u)" \
        "$root/cos_lib_tests" \
        clawd::memory::tests::root_legacy_reads_never_create_or_transfer_owner_state \
        --exact --ignored --test-threads=1
}

display() {
    local selection="${1:-display}"
    if [[ "$selection" == native-editor || "$selection" == native-files || "$selection" == native-terminal ]]; then
        cargo test -p cos --lib --locked native_editor_readiness -- --test-threads=1
        if [[ "$selection" == native-terminal ]]; then
            cargo test -p cos --lib --locked native_terminal_session_selection -- --test-threads=1
        fi
        cargo test --manifest-path desktop/comp/test/display-control/Cargo.toml \
            --locked keyboard_plan_refuses -- --test-threads=1
        python3 -B scripts/ci/native_editor_payload.py \
            --product "${selection#native-}" \
            --output "$GITHUB_WORKSPACE/build/$selection-input" \
            --target-dir "$CARGO_TARGET_DIR/$selection"
    fi
    cargo test -p claw-display-control -p claw-display-login --locked -- --test-threads=1
    cargo clippy -p claw-display-control -p claw-display-login --all-targets --locked -- -D warnings
    cargo build -p cos --locked --bin cos --bin claw-display-host \
        --bin claw-display-session --bin claw-gui-runner --bin claw-app-runner \
        --bin claw-file-manager
    if [[ "$selection" == native-package ]]; then
        local program
        for program in apt-get dpkg dpkg-deb dpkg-query apt-ftparchive gpg gpgconf python3 unshare bwrap setfacl; do
            command -v "$program" >/dev/null
        done
        python3 -I -c 'import apt_pkg'
        cargo build -p cos --locked --bin claw-app-apt-hook --bin claw-extension-host
        cargo test -p cos --lib --locked -- native_package_window \
            operation_bound_grant_sets_preserve_digest_bounds_and_atomicity \
            issuance_never_extends_a_fixed_operation_deadline \
            --test-threads=1
    fi
    cargo test -p cos --bin claw-file-manager --locked -- --test-threads=1
    cargo build -p claw-display-login --locked
    cargo test -p cos --lib --locked --no-run --message-format=json-render-diagnostics \
        > "$CARGO_TARGET_DIR/gui-tests.jsonl"
    test_binary="$(cos_test_binary "$CARGO_TARGET_DIR/gui-tests.jsonl")"
    cargo clippy --manifest-path desktop/comp/test/display-control/Cargo.toml \
        --locked --all-targets -- -D warnings
    cargo build --manifest-path desktop/comp/test/display-control/Cargo.toml --locked
    cargo clippy --manifest-path desktop/session/Cargo.toml --locked -- -D warnings
    cargo build --manifest-path desktop/session/Cargo.toml --locked
    cargo test --manifest-path desktop/session/Cargo.toml --locked file_manager::tests \
        -- --test-threads=1
    cc -std=c11 -Wall -Wextra -Werror \
        crates/claw-display-login/test/fixtures/pam_login.c \
        -lpam -ldl -o "$CARGO_TARGET_DIR/pam-login-fixture"
    private="$GITHUB_WORKSPACE/build/$selection-authenticated.$BASHPID"
    mkdir -p "$GITHUB_WORKSPACE/build"
    mkdir -m0700 "$private"
    local selectors=()
    if [[ "$selection" == display || "$selection" == native-fs || "$selection" == native-editor || "$selection" == native-files ]]; then
        test -c /dev/fuse
    fi
    if [[ "$selection" == native-fs ]]; then
        selectors=(--native-fs-only)
    elif [[ "$selection" == native-editor || "$selection" == native-files || "$selection" == native-terminal ]]; then
        selectors=("--$selection-only" "$GITHUB_WORKSPACE/build/$selection-input")
    elif [[ "$selection" == file-manager ]]; then
        selectors=(--file-manager-only)
    elif [[ "$selection" == native-package ]]; then
        selectors=(--native-package-only)
    fi
    local fixture_timeout=240
    if [[ "$selection" == display ]]; then
        # The full lane includes separate protected metadata and content cases.
        fixture_timeout=360
    fi
    local isolated=(sudo timeout "$fixture_timeout" unshare --mount --net --ipc --uts --fork)
    if [[ "$selection" == native-package ]]; then
        isolated=(sudo timeout --kill-after=5 "$fixture_timeout" unshare --mount --pid --net --ipc --uts --fork --mount-proc --kill-child=KILL
            env "COS_APP_APT_PARENT_MNT=$(readlink /proc/self/ns/mnt)"
                "COS_APP_APT_PARENT_PID=$(readlink /proc/self/ns/pid)"
                "COS_APP_APT_PARENT_NET=$(readlink /proc/self/ns/net)"
                "COS_APP_APT_TEST_HOOK=$CARGO_TARGET_DIR/debug/claw-app-apt-hook"
                "COS_APP_APT_TEST_HOST=$CARGO_TARGET_DIR/debug/claw-extension-host")
    fi
    "${isolated[@]}" bash --noprofile --norc crates/claw-display-login/test/fixtures/bootstrap.sh \
        "$test_binary" "$CARGO_TARGET_DIR/debug/claw-display-host" \
        "$CARGO_TARGET_DIR/debug/claw-display-headless-fixture" \
        "$CARGO_TARGET_DIR/debug/libpam_claw_display.so" \
        "$CARGO_TARGET_DIR/pam-login-fixture" "$private" \
        "$CARGO_TARGET_DIR/debug/cos" "$CARGO_TARGET_DIR/debug/claw-gui-runner" \
        "$CARGO_TARGET_DIR/debug/claw-app-runner" "$CARGO_TARGET_DIR/debug/claw-gui-probe" \
        "$CARGO_TARGET_DIR/debug/claw-display-session" "$CARGO_TARGET_DIR/debug/cosmic-session" \
        "$CARGO_TARGET_DIR/debug/claw-file-manager" desktop/session/data/claw-file-manager.desktop \
        "${selectors[@]}"
    if [[ "$selection" != display ]]; then
        return
    fi
    sudo timeout 30 unshare --mount --net --ipc --uts --fork \
        "$test_binary" \
        --exact display_session::registry::tests::private_deferred_retirement_reclaims_the_exact_slot_and_workload \
        --ignored --nocapture --test-threads=1 \
        | tee "$RUNNER_TEMP/gui-retirement.log"
    retired_group="$(sed -n 's/.*private retirement cgroup: //p' "$RUNNER_TEMP/gui-retirement.log")"
    [[ "$retired_group" =~ ^/sys/fs/cgroup/claw-display-retirement-[0-9]+-[0-9a-f]+$ ]]
    sudo test ! -e "$retired_group"
    sudo timeout 15 unshare --mount --net --fork \
        bash --noprofile --norc -c '
            set -euo pipefail
            mount --make-rprivate /
            ip link set lo up
            ip address add 93.184.216.34/32 dev lo
            exec "$1" --exact worker::net_broker::tests::retiring_endpoint_cancels_a_pending_tcp_connect \
                --ignored --nocapture --test-threads=1
        ' private-egress "$test_binary"
    for dns_case in \
        retiring_endpoint_cancels_a_pending_dns_lookup \
        resolver_preserves_complete_system_nss_answers_and_public_address_policy \
        a_pending_dns_lookup_keeps_its_deadline_and_reaps_the_resolver; do
        sudo timeout 15 unshare --mount --net --fork \
            bash --noprofile --norc -c '
                set -euo pipefail
                mount --make-rprivate /
                ip link set lo up
                exec "$1" --exact "$2" --ignored --nocapture --test-threads=1
            ' private-egress-dns "$test_binary" "worker::net_broker::tests::dns::$dns_case"
    done
    for data_case in \
        persistent_service_data_survives_uid_and_host_replacement \
        service_data_refuses_aliases_foreign_owners_submounts_and_replaced_bindings; do
        sudo timeout 90 unshare --mount --net --fork \
            env COS_APP_DATA_TEST_COS="$CARGO_TARGET_DIR/debug/cos" \
            bash --noprofile --norc -c '
                set -euo pipefail
                mount --make-rprivate /
                exec "$1" --exact "$2" --ignored --nocapture --test-threads=1
            ' private-app-data "$test_binary" "extension_host::spawn::app_data::tests::$data_case"
    done
    sudo timeout 100 unshare --mount --net --fork \
        env COS_APP_DATA_TEST_COS="$CARGO_TARGET_DIR/debug/cos" \
            COS_APP_DATA_TEST_RUNNER="$CARGO_TARGET_DIR/debug/claw-app-runner" \
        bash --noprofile --norc -c '
            set -euo pipefail
            mount --make-rprivate /
            exec "$1" --exact extension_host::spawn::task_data::tests::task_app_registration_binds_persistent_data_after_authorization \
                --ignored --nocapture --test-threads=1
        ' private-task-app-data "$test_binary"
}

applets() {
    manifest=desktop/applets/claw-applet-services/Cargo.toml
    cargo test -p claw-os-sdk --lib --locked -- --test-threads=1
    cargo clippy -p claw-os-sdk --all-targets --locked -- -D warnings
    cargo test --manifest-path "$manifest" --lib --locked -- --test-threads=1
    cargo test --manifest-path "$manifest" --features provider --locked -- --test-threads=1
    cargo clippy --manifest-path "$manifest" --features provider --all-targets --locked -- -D warnings
    cargo build -p cos --bin cos --locked
    cargo test --manifest-path "$manifest" --features provider --test kernel_policy \
        --no-run --locked --message-format=json-render-diagnostics > "$CARGO_TARGET_DIR/applet-kernel-tests.jsonl"
    test_binary="$(python3 - "$CARGO_TARGET_DIR/applet-kernel-tests.jsonl" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    records = map(json.loads, handle)
    print(next(row["executable"] for row in records
               if row.get("executable") and row["target"]["name"] == "kernel_policy"))
PY
    )"
    sudo python3 desktop/applets/claw-applet-services/test/support/run_kernel_fixture.py \
        --kernel "$CARGO_TARGET_DIR/debug/cos" \
        --provider "$CARGO_TARGET_DIR/debug/claw-os-applet-provider" \
        --test-binary "$test_binary" \
        --work-dir "$CARGO_TARGET_DIR/applet-kernel-fixtures"
}

case "${1:-}" in
    app-apt) app_apt ;;
    app-bind) app_bind ;;
    nearby) nearby ;;
    calendar) calendar ;;
    extensions) extensions ;;
    display) display ;;
    native-fs) display native-fs ;;
    file-manager) display file-manager ;;
    native-package) display native-package ;;
    native-editor) display native-editor ;;
    native-files) display native-files ;;
    native-terminal) display native-terminal ;;
    applets) applets ;;
    *)
        echo "usage: $0 {app-apt|app-bind|nearby|calendar|extensions|display|native-fs|file-manager|native-package|native-editor|native-files|native-terminal|applets}" >&2
        exit 2
        ;;
esac
