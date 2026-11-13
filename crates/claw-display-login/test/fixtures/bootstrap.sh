#!/usr/bin/env bash
set -euo pipefail

selection=all
native_product_input=
native_product=
native_app=
native_calendar_reader=
if [[ $# == 17 && ( "${15}" == --native-calendar-only || "${15}" == --native-desktop-widgets-only ) ]]; then
    native_calendar_reader=$(realpath -- "${17}")
    [[ -f "$native_calendar_reader" && -x "$native_calendar_reader" && ! -L "$native_calendar_reader" ]] || exit 64
    set -- "${@:1:16}"
elif [[ $# == 16 && ( "${15}" == --native-calendar-only || "${15}" == --native-desktop-widgets-only ) ]]; then
    printf 'actual Calendar consumers require the existing compiled OS reader\n' >&2
    exit 64
fi
if [[ $# == 16 && ( "${15}" == --native-editor-only || "${15}" == --native-files-only || "${15}" == --native-terminal-only || "${15}" == --native-calendar-only || "${15}" == --native-store-only || "${15}" == --native-store-package-only || "${15}" == --native-desktop-widgets-only || "${15}" == --native-clipboard-only ) ]]; then
    if [[ "${15}" == --native-editor-only ]]; then
        native_product=editor
        native_app=cosmic-edit
    elif [[ "${15}" == --native-files-only ]]; then
        native_product=files
        native_app=cosmic-files
    elif [[ "${15}" == --native-terminal-only ]]; then
        native_product=terminal
        native_app=cosmic-term
    elif [[ "${15}" == --native-store-only || "${15}" == --native-store-package-only ]]; then
        native_product=store
        native_app=cosmic-store
    elif [[ "${15}" == --native-desktop-widgets-only ]]; then
        native_product=desktop-widgets
        native_app=widget-rail
    elif [[ "${15}" == --native-clipboard-only ]]; then
        native_product=clipboard
        native_app=panel-clipboard
    else
        native_product=calendar
        native_app=panel-calendar
    fi
    selection="native-$native_product"
    if [[ "${15}" == --native-store-package-only ]]; then
        selection=native-store-package
    fi
    native_product_input=$(realpath -- "${16}")
    [[ -d "$native_product_input/$native_app" && ! -L "$native_product_input/$native_app" \
        && -f "$native_product_input/native-$native_product.json" && ! -L "$native_product_input/native-$native_product.json" ]] || exit 64
    set -- "${@:1:14}"
elif [[ $# == 15 ]]; then
    case "${15}" in
        --session-startup-only) selection=startup ;;
        --file-manager-only) selection=file-manager ;;
        --native-fs-only) selection=native-fs ;;
        --native-package-only) selection=native-package ;;
        *) exit 64 ;;
    esac
    set -- "${@:1:14}"
fi
package_fixture=0
if [[ "$selection" == native-package || "$selection" == native-store-package ]]; then
    package_fixture=1
fi
[[ "$EUID" == 0 && ( $# == 6 || $# == 14 ) ]] || exit 64
if [[ "$package_fixture" == 0 ]]; then
    [[ "$(readlink /proc/self/ns/mnt)" != "$(readlink /proc/1/ns/mnt)" ]] || exit 64
fi
registry=$(realpath -- "$1")
host=$(realpath -- "$2")
compositor=$(realpath -- "$3")
pam_module=$(realpath -- "$4")
pam_fixture=$(realpath -- "$5")
private=$(realpath -- "$6")
gui=0
if [[ $# == 14 ]]; then
    gui=1
    if [[ "$package_fixture" == 0 ]]; then
        [[ "$(readlink /proc/self/ns/net)" != "$(readlink /proc/1/ns/net)" ]] || exit 64
    fi
    cos=$(realpath -- "$7")
    gui_runner=$(realpath -- "$8")
    app_runner=$(realpath -- "$9")
    probe=$(realpath -- "${10}")
    session=$(realpath -- "${11}")
    cosmic_session=$(realpath -- "${12}")
    file_manager=$(realpath -- "${13}")
    file_manager_autostart=$(realpath -- "${14}")
    [[ -x "$cos" && -x "$gui_runner" && -x "$app_runner" && -x "$probe" \
        && -x "$session" && -x "$cosmic_session" && -x "$file_manager" \
        && -f "$file_manager_autostart" ]] || exit 64
fi
[[ -x "$registry" && -x "$host" && -x "$compositor" && -f "$pam_module" ]] || exit 64
[[ -x "$pam_fixture" && -d "$private" && "$private" != / ]] || exit 64
mount --make-rprivate /
if [[ "$package_fixture" == 1 ]]; then
    package_setup=$(realpath -- core/test/unit/clawd/server/gui_fixture/package_setup.py)
    /usr/bin/python3 -I -B "$package_setup" isolate
else
    mount -t tmpfs -o mode=0755,nosuid,nodev tmpfs /run
    mount -t tmpfs -o mode=0755,nosuid,nodev tmpfs /var/lib
    mkdir "$private/usr-upper" "$private/usr-work"
    mount -t overlay overlay \
        -o "lowerdir=/usr,upperdir=$private/usr-upper,workdir=$private/usr-work" /usr
fi
if [[ "$gui" == 1 && "$package_fixture" == 0 ]]; then
    mkdir "$private/etc-upper" "$private/etc-work"
    mount -t overlay overlay \
        -o "lowerdir=/etc,upperdir=$private/etc-upper,workdir=$private/etc-work" /etc
    mount -t tmpfs -o mode=0700,nosuid,nodev tmpfs /root
    mount -t tmpfs -o mode=0755,nosuid,nodev tmpfs /var/log
    install -d -m0755 /etc/cos
    mount -t tmpfs -o mode=0755,nosuid,nodev tmpfs /etc/cos
    if [[ "$selection" == native-store ]]; then
        mount -t tmpfs -o mode=0755,nosuid,nodev tmpfs /var/cache
        install -d -m0755 /etc/apt
        mount -t tmpfs -o mode=0755,nosuid,nodev tmpfs /etc/apt
    fi
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
    if [[ "$package_fixture" == 0 ]]; then
        install -m0755 "$cosmic_session" /usr/bin/cosmic-session
        install -m0755 "$file_manager" /usr/lib/cos/bin/claw-file-manager
        install -Dm0644 "$file_manager_autostart" /etc/xdg/autostart/claw-file-manager.desktop
    else
        install -m0755 "${COS_APP_APT_TEST_HOOK:?matching compiled APT helper}" /usr/lib/cos/bin/claw-app-apt-hook
        install -m0755 "${COS_APP_APT_TEST_HOST:?matching compiled App Host}" /usr/local/bin/claw-extension-host
        install -m0644 tools/app_apt_metadata.py /usr/lib/cos/app-apt-metadata.py
    fi
    install -m0755 "$probe" /usr/lib/cos/bin/claw-gui-probe
    install -m0755 "$pam_fixture" /usr/lib/cos/bin/pam-login-fixture
    install -d -m0755 /usr/lib/cos/apps /usr/lib/cos/trust/publishers.d
    mount -t tmpfs -o mode=0755,nosuid,nodev tmpfs /usr/lib/cos/apps
    if [[ -n "$native_product" ]]; then
        /usr/bin/python3 -I -B packaging/deb/native_runtime_assets.py --root "$private/native-runtime"
        install -d -m0755 /usr/share/icons
        cp -a -- "$private/native-runtime/usr/share/icons/Cosmic" /usr/share/icons/
        cp -a -- "$native_product_input/$native_app" "/usr/lib/cos/apps/$native_app"
        chown -hR 0:0 "/usr/lib/cos/apps/$native_app"
    fi
    if [[ "$native_product" == calendar || "$native_product" == desktop-widgets ]]; then
        [[ -n "$native_calendar_reader" ]] || exit 64
        install -m0755 "$native_calendar_reader" /usr/lib/cos/bin/claw-calendar-reader
    fi
fi
printf '%s\n' \
    'root:x:0:0:root:/root:/bin/sh' \
    'claw-display-test:x:62050:62050:Private display fixture:/run/display-home:/bin/sh' \
    'claw-display-other:x:62051:62051:Private negative caller:/run/display-other-home:/bin/sh' \
    >"$private/passwd"
printf '%s\n' 'root:x:0:' 'claw-display-test:x:62050:' 'claw-display-other:x:62051:' >"$private/group"
hash=$(printf '%s\n' 'private-disposable-display-fixture-password' | openssl passwd -6 -stdin)
printf 'root:!:20000:0:99999:7:::\nclaw-display-test:%s:20000:0:99999:7:::\nclaw-display-other:!:20000:0:99999:7:::\n' "$hash" \
    >"$private/shadow"
chmod 0600 "$private/shadow"
if [[ "$package_fixture" == 1 ]]; then
    /usr/bin/python3 -I -B "$package_setup" accounts "$private"
fi
mount --bind "$private/passwd" /etc/passwd
mount --bind "$private/group" /etc/group
mount --bind "$private/shadow" /etc/shadow
install -d -m0755 /run/cos /run/user
install -d -m0700 -o62050 -g62050 /run/display-home /run/user/62050
install -d -m0700 -o62051 -g62051 /run/display-other-home
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
    if [[ -n "$native_product" ]]; then
        install -m0644 "$native_product_input/native-$native_product.json" "/run/gui-fixture/native-$native_product.json"
        install -d -m0700 -o62050 -g62050 /run/user/62050/native-product-observer
        printf 'private-native-product-v1' >/run/gui-public/native-product-enabled
        chmod 0644 /run/gui-public/native-product-enabled
    fi
    if [[ "$native_product" == calendar || "$native_product" == desktop-widgets || "$native_product" == clipboard ]]; then
        printf '%s' "$native_product" >/run/gui-fixture/native-panel-product
        chmod 0600 /run/gui-fixture/native-panel-product
    fi
    if [[ "$selection" == native-store-package ]]; then
        printf 'private-store-package-v1' >/run/gui-fixture/native-store-package
        chmod 0600 /run/gui-fixture/native-store-package
    fi
    printf 'private-v1' >/run/gui-public/isolated
    chmod 0644 /run/gui-public/isolated
    for kind in mnt net; do
        readlink "/proc/self/ns/$kind" >"/run/gui-public/$kind-private"
        readlink "/proc/1/ns/$kind" >"/run/gui-public/$kind-host"
        chmod 0644 "/run/gui-public/$kind-private" "/run/gui-public/$kind-host"
    done
    install -d -m0755 /run/gui-session-empty-path
    install -d -m0755 /run/gui-session-components
    for component in cosmic-settings-daemon cosmic-notifications cosmic-panel \
        cosmic-app-library cosmic-launcher cosmic-workspaces cosmic-osd cosmic-bg \
        cosmic-greeter cosmic-files-applet cosmic-idle; do
        ln -s /usr/lib/cos/bin/pam-login-fixture "/run/gui-session-components/$component"
    done
    install -d -m0711 -o62050 -g62050 /run/gui-public/file-manager-bus
    cat > /run/gui-public/file-manager-bus.conf <<'BUS'
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:path=/run/user/62050/bus</listen>
  <listen>unix:path=/run/gui-public/file-manager-bus/bus</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <deny user="*"/>
    <allow user="root"/>
    <allow user="claw-display-test"/>
    <allow user="claw-display-other"/>
    <deny own="*"/>
    <deny send_destination="*"/>
    <allow receive_sender="*"/>
  </policy>
  <policy user="root">
    <allow send_destination="org.freedesktop.DBus"/>
  </policy>
  <policy user="claw-display-test">
    <allow own="*"/>
    <allow send_destination="*"/>
  </policy>
  <policy user="claw-display-other">
    <allow send_destination="org.freedesktop.DBus"/>
    <allow send_destination="org.freedesktop.FileManager1"/>
  </policy>
</busconfig>
BUS
    chmod 0644 /run/gui-public/file-manager-bus.conf
    cp -a "$private/pam" /run/gui-fixture/pam
    printf 'private-v1' >/run/gui-fixture/isolated
    printf '%s\n' "$group" >/run/gui-fixture/cgroup
    export HOME=/root COS_DATA_DIR=/var/lib/cos COS_LOG_DIR=/var/log/cos
    export COS_CONFIG_DIR=/etc/cos COS_CACHE_DIR=/run/gui-fixture/cache
    export PATH=/usr/local/bin:/usr/bin:/bin
    result=0
    fixture_test=clawd::server::gui_fixture::private_authenticated_gui
    case "$selection" in
        startup) fixture_test=clawd::server::gui_fixture::private_session_startup_failure ;;
        file-manager) fixture_test=clawd::server::gui_fixture::private_file_manager_session ;;
        native-fs) fixture_test=clawd::server::gui_fixture::private_native_fs_kernel ;;
        native-editor) fixture_test=clawd::server::gui_fixture::native_products::private_native_editor ;;
        native-files) fixture_test=clawd::server::gui_fixture::native_products::private_native_files ;;
        native-terminal) fixture_test=clawd::server::gui_fixture::native_products::private_native_terminal ;;
        native-calendar|native-desktop-widgets|native-clipboard) fixture_test=clawd::server::gui_fixture::native_products::private_native_panel ;;
        native-store|native-store-package) fixture_test=clawd::server::gui_fixture::native_products::private_native_store ;;
        native-package) fixture_test=clawd::server::gui_fixture::package::private_native_package_self_handoff ;;
    esac
    listing=$("$registry" --list)
    grep -Fx "$fixture_test: test" <<< "$listing" >/dev/null
    if [[ "$package_fixture" == 1 ]]; then
        grep -Fx 'clawd::server::gui_fixture::package::private_native_package_owner: test' <<< "$listing" >/dev/null
    fi
    grep -Fx 'clawd::server::gui_fixture::file_manager::private_file_manager_client: test' \
        <<< "$listing" >/dev/null
    if [[ "$package_fixture" == 1 ]]; then
        # PID 1 supervises; authenticated Root providers require pid > 1.
        "$registry" --exact "$fixture_test" --ignored --nocapture --test-threads=1 &
        registry_pid=$!
        wait "$registry_pid" || result=$?
        registry_pid=
    else
        "$registry" --exact "$fixture_test" --ignored --nocapture --test-threads=1 || result=$?
    fi
    if [[ -n "$native_product" ]]; then
        cp "/run/gui-fixture/native-$native_product.json" "$private/native-$native_product.json"
        chmod 0644 "$private/native-$native_product.json"
        if [[ -f /run/user/62050/native-product-observer/window.json ]]; then
            cp /run/user/62050/native-product-observer/window.json "$private/native-$native_product-window.json"
            chmod 0644 "$private/native-$native_product-window.json"
        fi
        if [[ ( "$native_product" == calendar || "$native_product" == store || "$native_product" == desktop-widgets || "$native_product" == clipboard ) && -f "/run/gui-fixture/native-$native_product-consumer.json" ]]; then
            cp "/run/gui-fixture/native-$native_product-consumer.json" "$private/native-$native_product-consumer.json"
            chmod 0644 "$private/native-$native_product-consumer.json"
            /usr/bin/python3 -I -B - "$private" "$native_product" <<'PY'
import json, os, pathlib, stat, sys

destination = pathlib.Path(sys.argv[1])
owner = destination.stat()
source = pathlib.Path("/run/user/62050/native-product-observer")
product = sys.argv[2]
profiles = {
    "calendar": ("native-calendar-agenda.shm", b"CLAWCALSHM1\0", "claw.private-calendar"),
    "store": ("native-store-window.shm", b"CLAWSTRSHM1\0", "claw.private-store"),
    "desktop-widgets": ("native-widget-rail.shm", b"CLAWWIDSHM1\0", "claw.private-widget-rail"),
    "clipboard": ("native-clipboard-history.shm", b"CLAWCLPSHM1\0", "claw.private-clipboard"),
}
assert product in profiles
popup = product in ("calendar", "clipboard")
widgets = product == "desktop-widgets"
filename, magic, prefix = profiles[product]

def protected(path, uid, limit):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as stream:
        info = os.fstat(stream.fileno())
        assert stat.S_ISREG(info.st_mode) and info.st_uid == uid and info.st_nlink == 1
        assert info.st_mode & 0o077 == 0 and info.st_size <= limit
        data = stream.read(limit + 1)
        assert len(data) <= limit
        return data

def publish(name, data):
    fd = os.open(destination / name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "wb") as stream:
        os.fchown(stream.fileno(), owner.st_uid, owner.st_gid)
        stream.write(data)

window = json.loads(protected(source / "window.json", 62050, 8192))
consumer = json.loads(protected(pathlib.Path(f"/run/gui-fixture/native-{product}-consumer.json"), 0, 16384))
assert window["instance"] == consumer["instance"]
if popup:
    assert window["popup"]["closed"]
    index = window["popup"].get("snapshot") or {}
    assert index == (consumer.get("passive_agenda_snapshot" if product == "calendar" else "passive_history_snapshot") or {})
    latest_commit = window["popup"]["buffer_commits"]
else:
    assert window["closed"] and window["toplevels"] == 0
    if widgets:
        assert window["role"] == "zwlr_layer_surface_v1" and window["layer_surfaces"] == 0
    index = window.get("snapshot") or {}
    observed = consumer.get("passive_panel_snapshot" if widgets else "passive_window_snapshot") or {}
    if index.get("status") == "saved":
        assert observed["instance"] == index["instance"] == window["instance"]
        assert observed["snapshot_commit"] <= index["snapshot_commit"]
    latest_commit = window["buffer_commits"]
copied, reason = False, index.get("status", "no-captured-frame")
if index.get("status") == "saved" and index.get("closed") and not index.get("mapped"):
    if index.get("matches_current_configure") and index["snapshot_commit"] == index["latest_buffer_commit"]:
        assert index["latest_buffer_commit"] == latest_commit
        assert 1 <= index["publications"] <= 32 and index["total_bytes"] <= 32 * 1024 * 1024
        data = protected(source / filename, 62050, 4 * 1024 * 1024 + 4112)
        assert len(data) == index["file_bytes"] and data[:12] == magic
        size = int.from_bytes(data[12:16], "little")
        assert size <= 4096 and 16 + size <= len(data)
        frame = json.loads(data[16:16 + size])
        pixels = data[16 + size:]
        assert frame["schema"] == prefix + "-shm-frame/v1"
        assert frame["synthetic_fixture"] and frame["captured_live"]
        assert frame["owner_uid"] == 62050 and frame["byte_order"] == "little-endian"
        assert frame["instance"] == window["instance"] == index["instance"]
        if popup:
            assert frame["parent_instance"] == window["instance"]
            assert frame["role"] == "xdg_popup" and frame["parent_role"] == "zwlr_layer_surface_v1"
            assert frame["geometry"] == window["popup"]["geometry"]
        else:
            assert frame["role"] == ("zwlr_layer_surface_v1" if widgets else "xdg_toplevel")
            assert frame["parent_role"] is None and frame["parent_instance"] is None
            assert frame["geometry"] == ([0, 0, *window["configure_size"]] if widgets else [0, 0, 800, 600])
        assert frame["buffer_commit"] == index["snapshot_commit"]
        assert frame["configure_serial"] == index["configure_serial"]
        width, height = frame["geometry"][2:]
        assert 100 <= width <= 4096 and 100 <= height <= 4096
        if widgets:
            assert width <= 512 and height <= 800
        assert (frame["raster"]["width"], frame["raster"]["height"]) == (width, height)
        assert frame["source_format"] in ("argb8888", "xrgb8888")
        assert frame["stored_stride"] == width * 4 and frame["stored_offset"] == 0
        assert len(pixels) == frame["stored_bytes"] == width * height * 4
        checksum = 0xcbf29ce484222325
        for byte in pixels:
            checksum = ((checksum ^ byte) * 0x100000001b3) & 0xffffffffffffffff
        assert f"{checksum:016x}" == frame["pixel_checksum"] == index["pixel_checksum"] == frame["raster"]["pixel_checksum"]
        publish(filename, data)
        copied, reason = True, "latest-live-frame-retained-through-original-close"
    else:
        reason = "latest-frame-or-configure-not-captured"
publish(filename.removesuffix(".shm") + "-export.json", (json.dumps({
    "schema": prefix + "-shm-export/v1", "instance": window["instance"],
    "copied": copied, "reason": reason, "snapshot": index,
    "reply_decode_asserted": False, "event_text_asserted": False,
}) + "\n").encode())
PY
        fi
    fi
    if [[ "$native_product" == clipboard && -f /run/gui-fixture/copyq-provider.log ]]; then
        cp /run/gui-fixture/copyq-provider.log "$private/copyq-provider.log"
        chmod 0644 "$private/copyq-provider.log"
    fi
    for case in password non-root locale host-parent compositor-mode duplicate inherited subscribers {0..24} session-startup file-manager native-product; do
        if [[ -f "/run/gui-fixture/pam-$case.log" ]]; then
            cp "/run/gui-fixture/pam-$case.log" "$private/pam-$case.log"
            chmod 0644 "$private/pam-$case.log"
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
