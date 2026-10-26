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

nearby() {
    cargo test --manifest-path crates/claw-nearby/Cargo.toml --locked
    cargo clippy --manifest-path crates/claw-nearby/Cargo.toml --all-targets --locked -- -D warnings
    cargo test --manifest-path crates/claw-nearby/vendor/owlyd/Cargo.toml --test service --locked
    cargo test -p cos --lib --locked --no-run --message-format=json \
        > "$RUNNER_TEMP/nearby-tests.jsonl"
    test_binary="$(cos_test_binary "$RUNNER_TEMP/nearby-tests.jsonl")"
    sudo timeout 60 unshare --mount --net --fork \
        "$test_binary" clawd::nearby:: --include-ignored --test-threads=1
    python3 -B packaging/deb/tests/test_nearby.py
}

app_bind() {
    cargo build -p cos --bin claw-app-runner --locked
    COS_CAPTURED_TEST_RUNNER="$CARGO_TARGET_DIR/debug/claw-app-runner" \
        cargo test -p cos --lib --locked \
        bridge::captured::tests::process::real_runner_waits_for_binding_and_preserves_app_input \
        -- --exact --ignored --nocapture --test-threads=1
}

calendar() {
    cargo build -p cos --bin cos --locked
    cargo build --manifest-path desktop/applets/claw-applet-services/Cargo.toml \
        --features provider --bins --locked --target-dir "$RUNNER_TEMP/calendar-target"
    cargo test -p cos --lib --locked --no-run --message-format=json \
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
    cargo build -p cos --bin claw-agentd --bin claw-extension-host
    cargo test -p cos \
        --test agentd_process_boundary \
        --test extension_host_boundary \
        --no-run
    cargo test -p cos --lib --locked --no-run --message-format=json \
        > "$RUNNER_TEMP/service-retirement-tests.jsonl"
    service_test_binary="$(cos_test_binary "$RUNNER_TEMP/service-retirement-tests.jsonl")"
    sudo timeout 120 unshare --mount --net --fork \
        env COS_SERVICE_RETIREMENT_HOST="$CARGO_TARGET_DIR/debug/claw-extension-host" \
        bash --noprofile --norc -c '
            set -euo pipefail
            mount --make-rprivate /
            exec "$1" --exact clawd::app_services::retirement::tests::process::service_retirement_keeps_custody_until_cleanup_succeeds \
                --ignored --nocapture --test-threads=1
        ' private-service-retirement "$service_test_binary"
    sudo timeout 30 "$service_test_binary" \
        --exact extension_host::identity::tests::checked_release_retries_authenticated_marker_cleanup \
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
        "$agentd_test" "$root/agentd_process_boundary"
    sudo install -o root -g root -m 0755 \
        "$extension_test" "$root/extension_host_boundary"
    sudo install -o root -g root -m 0755 \
        "$lib_test" "$root/cos_lib_tests"

    sudo env \
        HOME=/root \
        SUDO_UID="$(id -u)" \
        COS_PRIVILEGED_EXTENSION_HOST_BIN="$root/claw-extension-host" \
        "$root/agentd_process_boundary" --test-threads=1
    sudo env \
        HOME=/root \
        SUDO_UID="$(id -u)" \
        COS_PRIVILEGED_EXTENSION_HOST_BIN="$root/claw-extension-host" \
        "$root/extension_host_boundary" --test-threads=1
    sudo env \
        HOME=/root \
        SUDO_UID="$(id -u)" \
        COS_REQUIRE_PRIVILEGED_CHILD_TESTS=1 \
        "$root/cos_lib_tests" extension_host::child_isolation::tests \
        --test-threads=1
}

display() {
    cargo test -p claw-display-control -p claw-display-login --locked -- --test-threads=1
    cargo clippy -p claw-display-control -p claw-display-login --all-targets --locked -- -D warnings
    cargo build -p cos --locked --bin cos --bin claw-display-host \
        --bin claw-display-session --bin claw-gui-runner --bin claw-app-runner
    cargo build -p claw-display-login --locked
    cargo test -p cos --lib --locked --no-run --message-format=json \
        > "$CARGO_TARGET_DIR/gui-tests.jsonl"
    test_binary="$(cos_test_binary "$CARGO_TARGET_DIR/gui-tests.jsonl")"
    cargo clippy --manifest-path desktop/comp/test/display-control/Cargo.toml \
        --locked --all-targets -- -D warnings
    cargo build --manifest-path desktop/comp/test/display-control/Cargo.toml --locked
    cargo clippy --manifest-path desktop/session/Cargo.toml --locked -- -D warnings
    cargo build --manifest-path desktop/session/Cargo.toml --locked
    cc -std=c11 -Wall -Wextra -Werror \
        crates/claw-display-login/test/fixtures/pam_login.c \
        -lpam -ldl -o "$CARGO_TARGET_DIR/pam-login-fixture"
    private="$(mktemp -d "$RUNNER_TEMP/cos-gui.XXXXXX")"
    sudo timeout 180 unshare --mount --net --ipc --uts --fork \
        bash --noprofile --norc crates/claw-display-login/test/fixtures/bootstrap.sh \
        "$test_binary" "$CARGO_TARGET_DIR/debug/claw-display-host" \
        "$CARGO_TARGET_DIR/debug/claw-display-headless-fixture" \
        "$CARGO_TARGET_DIR/debug/libpam_claw_display.so" \
        "$CARGO_TARGET_DIR/pam-login-fixture" "$private" \
        "$CARGO_TARGET_DIR/debug/cos" "$CARGO_TARGET_DIR/debug/claw-gui-runner" \
        "$CARGO_TARGET_DIR/debug/claw-app-runner" "$CARGO_TARGET_DIR/debug/claw-gui-probe" \
        "$CARGO_TARGET_DIR/debug/claw-display-session" "$CARGO_TARGET_DIR/debug/cosmic-session"
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
        --no-run --locked --message-format=json > "$CARGO_TARGET_DIR/applet-kernel-tests.jsonl"
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
    app-bind) app_bind ;;
    nearby) nearby ;;
    calendar) calendar ;;
    extensions) extensions ;;
    display) display ;;
    applets) applets ;;
    *)
        echo "usage: $0 {app-bind|nearby|calendar|extensions|display|applets}" >&2
        exit 2
        ;;
esac
