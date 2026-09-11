#!/usr/bin/env bash
set -euo pipefail

session_only=0
if [[ $# == 13 && "${13}" == --session-startup-only ]]; then
    session_only=1
    set -- "${@:1:12}"
fi
[[ "$EUID" == 0 && ( $# == 6 || $# == 12 ) ]] || exit 64
[[ "$(readlink /proc/self/ns/mnt)" != "$(readlink /proc/1/ns/mnt)" ]] || exit 64
registry=$(realpath -- "$1")
host=$(realpath -- "$2")
compositor=$(realpath -- "$3")
pam_module=$(realpath -- "$4")
pam_fixture=$(realpath -- "$5")
private=$(realpath -- "$6")
gui=0
if [[ $# == 12 ]]; then
    gui=1
    [[ "$(readlink /proc/self/ns/net)" != "$(readlink /proc/1/ns/net)" ]] || exit 64
    cos=$(realpath -- "$7")
    gui_runner=$(realpath -- "$8")
    app_runner=$(realpath -- "$9")
    probe=$(realpath -- "${10}")
    session=$(realpath -- "${11}")
    cosmic_session=$(realpath -- "${12}")
    [[ -x "$cos" && -x "$gui_runner" && -x "$app_runner" && -x "$probe" \
        && -x "$session" && -x "$cosmic_session" ]] || exit 64
fi
[[ -x "$registry" && -x "$host" && -x "$compositor" && -f "$pam_module" ]] || exit 64
[[ -x "$pam_fixture" && -d "$private" && "$private" != / ]] || exit 64
mount --make-rprivate /
mount -t tmpfs -o mode=0755,nosuid,nodev tmpfs /run
mount -t tmpfs -o mode=0755,nosuid,nodev tmpfs /var/lib
mkdir "$private/usr-upper" "$private/usr-work"
mount -t overlay overlay \
    -o "lowerdir=/usr,upperdir=$private/usr-upper,workdir=$private/usr-work" /usr
if [[ "$gui" == 1 ]]; then
    mkdir "$private/etc-upper" "$private/etc-work"
    mount -t overlay overlay \
        -o "lowerdir=/etc,upperdir=$private/etc-upper,workdir=$private/etc-work" /etc
    mount -t tmpfs -o mode=0700,nosuid,nodev tmpfs /root
    mount -t tmpfs -o mode=0755,nosuid,nodev tmpfs /var/log
    install -d -m0755 /etc/cos
    mount -t tmpfs -o mode=0755,nosuid,nodev tmpfs /etc/cos
fi
install -d -m0755 /usr/lib/cos/bin
install -m0755 "$host" /usr/lib/cos/bin/claw-display-host
install -m0755 "$compositor" /usr/bin/cosmic-comp
install -m0644 "$pam_module" /usr/lib/x86_64-linux-gnu/security/pam_claw_display.so
if [[ "$gui" == 1 ]]; then
    install -d -m0755 /usr/local/bin
    install -m0755 "$registry" /usr/local/bin/claw-gui-broker-fixture
    registry=/usr/local/bin/claw-gui-broker-fixture
    install -m0755 "$cos" /usr/local/bin/cos
    install -m0755 "$gui_runner" /usr/local/bin/claw-gui-runner
    install -m0755 "$app_runner" /usr/local/bin/claw-app-runner
    install -m0755 "$session" /usr/local/bin/claw-display-session
    install -m0755 "$cosmic_session" /usr/bin/cosmic-session
    install -m0755 "$probe" /usr/lib/cos/bin/claw-gui-probe
    install -m0755 "$pam_fixture" /usr/lib/cos/bin/pam-login-fixture
    install -d -m0755 /usr/lib/cos/apps /usr/lib/cos/trust/publishers.d
    mount -t tmpfs -o mode=0755,nosuid,nodev tmpfs /usr/lib/cos/apps
fi
printf '%s\n' \
    'root:x:0:0:root:/root:/bin/sh' \
    'claw-display-test:x:62050:62050:Private display fixture:/run/display-home:/bin/sh' \
    >"$private/passwd"
printf '%s\n' 'root:x:0:' 'claw-display-test:x:62050:' >"$private/group"
hash=$(printf '%s\n' 'private-disposable-display-fixture-password' | openssl passwd -6 -stdin)
printf 'root:!:20000:0:99999:7:::\nclaw-display-test:%s:20000:0:99999:7:::\n' "$hash" \
    >"$private/shadow"
chmod 0600 "$private/shadow"
mount --bind "$private/passwd" /etc/passwd
mount --bind "$private/group" /etc/group
mount --bind "$private/shadow" /etc/shadow
install -d -m0755 /run/cos /run/user
install -d -m0700 -o62050 -g62050 /run/display-home /run/user/62050
mkdir -m0700 "$private/pam"
printf '%s\n' \
    'auth required pam_unix.so' \
    'account required pam_unix.so' \
    'session required pam_loginuid.so' \
    'session required pam_claw_display.so close-before' \
    'session required pam_claw_display.so open-after' >"$private/pam/cosmic-greeter"
group=/sys/fs/cgroup/claw-display-fixture.$BASHPID
mkdir -m0755 -- "$group"
printf '%s\n' "$group" >"$private/cgroup"
registry_pid=
pam_pid=
cleanup() {
    local result=$?
    trap - EXIT
    if [[ -n "$pam_pid" ]] && kill -0 "$pam_pid" 2>/dev/null; then
        printf '1' >"$group/cgroup.kill"
        wait "$pam_pid" || true
    fi
    if [[ -n "$registry_pid" ]] && kill -0 "$registry_pid" 2>/dev/null; then
        kill "$registry_pid"
        wait "$registry_pid" || true
    fi
    if [[ "$(realpath -e -- "$group")" != "$group" \
        || ! "$group" =~ ^/sys/fs/cgroup/claw-display-fixture\.[[:alnum:]]+$ ]]; then
        printf 'refusing an unexpected private cgroup cleanup target\n' >&2
        exit 1
    fi
    if ! grep -qx 'populated 0' "$group/cgroup.events"; then
        printf '1' >"$group/cgroup.kill"
        for _ in $(seq 1 500); do
            if grep -qx 'populated 0' "$group/cgroup.events"; then break; fi
            sleep 0.01
        done
    fi
    if ! grep -qx 'populated 0' "$group/cgroup.events"; then
        printf 'private cgroup remains populated: %s\n' "$group" >&2
        exit 1
    fi
    while IFS= read -r -d '' child; do
        if ! rmdir -- "$child"; then
            printf 'private cgroup cleanup requires inspection: %s\n' "$child" >&2
            result=1
        fi
    done < <(find "$group" -depth -type d -print0)
    exit "$result"
}
trap cleanup EXIT
if [[ "$gui" == 1 ]]; then
    install -d -m0700 /run/gui-fixture
    install -d -m0755 /run/gui-public
    install -d -m0755 /run/gui-session-empty-path
    cp -a "$private/pam" /run/gui-fixture/pam
    printf 'private-v1' >/run/gui-fixture/isolated
    printf '%s\n' "$group" >/run/gui-fixture/cgroup
    export HOME=/root COS_DATA_DIR=/var/lib/cos COS_LOG_DIR=/var/log/cos
    export COS_CONFIG_DIR=/etc/cos COS_CACHE_DIR=/run/gui-fixture/cache
    export PATH=/usr/local/bin:/usr/bin:/bin
    result=0
    fixture_test=clawd::server::gui_fixture::private_authenticated_gui
    if [[ "$session_only" == 1 ]]; then
        fixture_test=clawd::server::gui_fixture::private_session_startup_failure
    fi
    "$registry" --exact "$fixture_test" \
        --ignored --nocapture --test-threads=1 || result=$?
    for case in password non-root locale host-parent compositor-mode duplicate inherited subscribers {0..11} session-startup; do
        if [[ -f "/run/gui-fixture/pam-$case.log" ]]; then
            cp "/run/gui-fixture/pam-$case.log" "$private/pam-$case.log"
            if [[ "$result" != 0 ]]; then cat "$private/pam-$case.log" >&2; fi
        fi
    done
    if [[ "$result" != 0 ]]; then
        printf '1' >"$group/cgroup.kill"
    fi
    exit "$result"
fi
mkfifo -m0600 "$private/registry-input" "$private/pam-input"
exec {registry_input}<>"$private/registry-input"
exec {pam_input}<>"$private/pam-input"
"$registry" --exact display_session::registry::tests::private_registry_process \
    --ignored --nocapture --test-threads=1 \
    <"$private/registry-input" >"$private/registry-output" 2>"$private/registry-error" &
registry_pid=$!
for _ in $(seq 1 100); do
    if grep -q 'private display registry ready' "$private/registry-output"; then break; fi
    if ! kill -0 "$registry_pid" 2>/dev/null; then cat "$private/registry-error" >&2; exit 1; fi
    sleep 0.05
done
grep -q 'private display registry ready' "$private/registry-output"
bash --noprofile --norc -c '
    printf 0 >"$1/cgroup.procs"
    exec "$2" "$3" activate
' private-pam "$group" "$pam_fixture" "$private/pam" \
    <"$private/pam-input" >"$private/pam-output" 2>"$private/pam-error" &
pam_pid=$!
for _ in $(seq 1 500); do
    if grep -q '^authenticated 62050 ' "$private/pam-output"; then break; fi
    if ! kill -0 "$pam_pid" 2>/dev/null; then cat "$private/pam-error" >&2; exit 1; fi
    sleep 0.05
done
grep -q '^authenticated 62050 ' "$private/pam-output"
cat "$private/pam-output"
printf 'close\n' >&"$pam_input"
if ! wait "$pam_pid"; then cat "$private/pam-error" >&2; exit 1; fi
pam_pid=
printf 'stop\n' >&"$registry_input"
wait "$registry_pid"
registry_pid=
grep -qx 'populated 0' "$group/cgroup.events"
[[ -z "$(cat "$group/cgroup.procs")" ]]
printf 'Root PAM -> supervised headless compositor -> checked logout: passed\n'
