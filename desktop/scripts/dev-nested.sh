#!/usr/bin/env bash
# desktop/scripts/dev-nested.sh
#
# One-shot launcher for a nested COSMIC desktop session — runs the full DE
# (compositor + panel + launcher + bg + osd + workspaces + applets + apps)
# inside a single Wayland window on the host, with no ISO/VM required.
#
# How it works:
#   * cosmic-comp's winit backend renders the compositor into a normal
#     window when WAYLAND_DISPLAY (or DISPLAY) is set on the host. See
#     desktop/comp/src/backend/mod.rs:25-45 — setting COSMIC_BACKEND=winit
#     forces this path even on KMS-capable hardware.
#   * cosmic-session (desktop/session/src/main.rs) orchestrates comp +
#     settings-daemon + bg + panel + launcher + applibrary + workspaces +
#     osd + notifications + idle. Its ProcessManager auto-restarts any
#     child with ExponentialBackoff (set_max_restarts = usize::MAX,
#     desktop/session/src/main.rs:130-133), so killing a single component
#     after a rebuild is enough to pick up the new binary.
#   * We build each component in-tree, then prepend a temp dir of symlinks
#     to PATH so cosmic-session resolves the dev binaries instead of any
#     system-installed /usr/bin/cosmic-* leftovers.
#
# Host requirements:
#   * Linux with a running Wayland session (WAYLAND_DISPLAY set).
#     Tested under WSL2+WSLg, GNOME-on-Wayland, KDE Plasma 6 Wayland and
#     a parent COSMIC session.
#   * rustup stable + just + dbus-run-session + standard Wayland/Mesa
#     dev libs (same set that rootfs/features/desktop/install.sh expects).
#
# Usage:
#   ./desktop/scripts/dev-nested.sh build [components...]    # cargo build (default: all)
#   ./desktop/scripts/dev-nested.sh start                    # launch nested session
#   ./desktop/scripts/dev-nested.sh stop                     # stop nested session
#   ./desktop/scripts/dev-nested.sh restart <component>      # rebuild + hot-swap one component
#   ./desktop/scripts/dev-nested.sh status                   # show pid + components
#   ./desktop/scripts/dev-nested.sh logs [-f]                # tail session log
#   ./desktop/scripts/dev-nested.sh components               # list known components
#
# Environment knobs:
#   DEV_NESTED_PROFILE  cargo profile (default: release)
#   DEV_NESTED_LOG      log file (default: ~/.cache/claw-os/dev-nested.log)
#   RUST_LOG            forwarded into the nested session

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DESKTOP_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

PROFILE="${DEV_NESTED_PROFILE:-release}"
STATE_DIR="${XDG_RUNTIME_DIR:-/tmp}/claw-dev-nested"
PID_FILE="$STATE_DIR/session.pid"
BIN_DIR="$STATE_DIR/bin"
LOG_FILE="${DEV_NESTED_LOG:-${HOME}/.cache/claw-os/dev-nested.log}"

# component_name : subdir_under_desktop
# Listed in roughly the order cosmic-session spawns them (see
# desktop/session/src/main.rs:228-350). cosmic-greeter is intentionally
# omitted — it needs greetd+PAM and won't usefully run nested.
COMPONENTS=(
    "cosmic-session:session"
    "cosmic-comp:comp"
    "cosmic-settings-daemon:settings-daemon"
    "cosmic-bg:bg"
    "cosmic-panel:panel"
    "cosmic-launcher:launcher"
    "cosmic-app-library:applibrary"
    "cosmic-workspaces:workspaces"
    "cosmic-osd:osd"
    "cosmic-notifications:notifications"
    "cosmic-idle:idle"
    "cosmic-randr:randr"
)

# -----------------------------------------------------------------------------
# Helpers.
# -----------------------------------------------------------------------------

die() { echo "error: $*" >&2; exit 1; }
info() { echo ":: $*"; }

component_dir() {
    local name="$1"
    for entry in "${COMPONENTS[@]}"; do
        [ "${entry%%:*}" = "$name" ] && { echo "$DESKTOP_DIR/${entry##*:}"; return 0; }
    done
    return 1
}

# Resolve the on-disk path of a component's release binary.
# Most crates use a plain `cargo build` and end up at <crate>/target/<profile>/<name>.
# Workspaces (panel uses cosmic-panel-bin in a workspace; comp/settings-daemon/
# workspaces use Makefiles) all still drop the binary at the standard
# target/<profile>/<binary_name> location, so we can use a uniform rule.
component_bin() {
    local name="$1"
    local dir
    dir="$(component_dir "$name")" || return 1
    echo "$dir/target/$PROFILE/$name"
}

require_wayland() {
    if [ -z "${WAYLAND_DISPLAY:-}" ] && [ -z "${DISPLAY:-}" ]; then
        die "no WAYLAND_DISPLAY or DISPLAY in env — need a host Wayland session (WSLg / GNOME / KDE / COSMIC)"
    fi
}

ensure_state_dir() {
    mkdir -p "$STATE_DIR" "$BIN_DIR" "$(dirname "$LOG_FILE")"
}

# Build a single component.  Uses `just build-release` when the crate has
# a justfile recipe for it (the upstream COSMIC convention), otherwise
# falls back to plain `cargo build --release`.  `cosmic-comp`,
# `cosmic-settings-daemon` and `cosmic-workspaces` use Makefiles instead
# of justfiles — `make all` produces the same target/<profile>/<bin>.
build_one() {
    local name="$1"
    local dir
    dir="$(component_dir "$name")" || die "unknown component: $name"

    info "building $name ($(realpath --relative-to="$DESKTOP_DIR" "$dir"))"

    if [ -f "$dir/Makefile" ] && [ ! -f "$dir/justfile" ] && [ ! -f "$dir/Justfile" ]; then
        ( cd "$dir" && make all )
    elif [ -f "$dir/justfile" ] || [ -f "$dir/Justfile" ]; then
        # `just build-release` is the documented dev recipe (see e.g.
        # desktop/files/justfile:50). Fall back to cargo if absent.
        if just --justfile "$dir/justfile" --list 2>/dev/null | grep -q '^ *build-release\b'; then
            ( cd "$dir" && just build-release )
        elif just --justfile "$dir/Justfile" --list 2>/dev/null | grep -q '^ *build-release\b'; then
            ( cd "$dir" && just build-release )
        else
            ( cd "$dir" && cargo build --profile "$PROFILE" )
        fi
    else
        ( cd "$dir" && cargo build --profile "$PROFILE" )
    fi
}

# Populate $BIN_DIR with symlinks to the freshly-built binaries.  We
# prepend this dir to PATH for the nested session so cosmic-session
# resolves dev binaries (not any /usr/bin/cosmic-* the host may have).
link_bins() {
    rm -rf "$BIN_DIR"
    mkdir -p "$BIN_DIR"
    for entry in "${COMPONENTS[@]}"; do
        local name="${entry%%:*}"
        local bin
        bin="$(component_bin "$name")"
        if [ -x "$bin" ]; then
            ln -sf "$bin" "$BIN_DIR/$name"
        else
            echo "warning: $name not built (missing $bin) — run 'build $name'" >&2
        fi
    done
}

session_running() {
    [ -f "$PID_FILE" ] || return 1
    local pid
    pid="$(cat "$PID_FILE" 2>/dev/null || true)"
    [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null
}

# -----------------------------------------------------------------------------
# Sub-commands.
# -----------------------------------------------------------------------------

cmd_components() {
    printf '%-26s %s\n' COMPONENT DIRECTORY
    for entry in "${COMPONENTS[@]}"; do
        printf '%-26s %s\n' "${entry%%:*}" "desktop/${entry##*:}"
    done
}

cmd_build() {
    if [ "$#" -eq 0 ]; then
        for entry in "${COMPONENTS[@]}"; do
            build_one "${entry%%:*}"
        done
    else
        for name in "$@"; do
            build_one "$name"
        done
    fi
    link_bins
    info "binaries linked under $BIN_DIR"
}

cmd_start() {
    require_wayland
    ensure_state_dir

    if session_running; then
        die "nested session already running (pid $(cat "$PID_FILE")). Run 'stop' first."
    fi

    # Make sure every component is built at least once — fast no-op if up to date.
    link_bins
    local missing=0
    for entry in "${COMPONENTS[@]}"; do
        local name="${entry%%:*}"
        [ -L "$BIN_DIR/$name" ] || { echo "missing build for $name"; missing=1; }
    done
    if [ "$missing" = "1" ]; then
        info "running first-time build (this is the slow 30–60min compile)"
        cmd_build
    fi

    command -v dbus-run-session >/dev/null || die "dbus-run-session not on PATH (apt install dbus-user-session)"

    info "starting nested cosmic-session (winit backend)"
    info "logs: $LOG_FILE"

    # Environment for the nested session. cosmic-session imports a subset
    # of these into the user dbus + systemd-user env; the rest are read by
    # cosmic-comp and its children directly.
    (
        export PATH="$BIN_DIR:$PATH"
        export COSMIC_BACKEND=winit
        export XDG_SESSION_TYPE=wayland
        export XDG_CURRENT_DESKTOP="${XDG_CURRENT_DESKTOP:-COSMIC}"
        export XDG_SESSION_DESKTOP="${XDG_SESSION_DESKTOP:-COSMIC}"
        export _JAVA_AWT_WM_NONREPARENTING=1
        export GDK_BACKEND=wayland,x11
        export MOZ_ENABLE_WAYLAND=1
        export QT_QPA_PLATFORM="wayland;xcb"
        export RUST_BACKTRACE="${RUST_BACKTRACE:-1}"
        export RUST_LOG="${RUST_LOG:-cosmic_session=info,cosmic_comp=info}"

        # nohup + setsid so the session survives the launching terminal,
        # and so a SIGINT in this script doesn't tear the whole DE down.
        nohup setsid dbus-run-session -- cosmic-session \
            >"$LOG_FILE" 2>&1 &
        echo $! > "$PID_FILE"
    )

    sleep 1
    if session_running; then
        info "started (pid $(cat "$PID_FILE")). Tail logs with: $0 logs -f"
    else
        die "session exited immediately — check $LOG_FILE"
    fi
}

cmd_stop() {
    if ! session_running; then
        info "no nested session running"
        rm -f "$PID_FILE"
        return 0
    fi
    local pid
    pid="$(cat "$PID_FILE")"
    info "stopping nested session (pid $pid)"
    # SIGTERM the whole process group so children (comp, panel, ...) go too.
    kill -TERM -"$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || true
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        session_running || break
        sleep 0.5
    done
    if session_running; then
        info "force-killing pid $pid"
        kill -KILL -"$pid" 2>/dev/null || kill -KILL "$pid" 2>/dev/null || true
    fi
    rm -f "$PID_FILE"
}

cmd_restart() {
    [ "$#" -eq 1 ] || die "usage: restart <component>"
    local name="$1"
    local bin
    bin="$(component_bin "$name")" || die "unknown component: $name"

    if ! session_running; then
        die "no nested session running — start it first"
    fi

    build_one "$name"
    # Refresh symlink in case the binary path changed (it shouldn't, but
    # be defensive).
    ln -sf "$bin" "$BIN_DIR/$name"

    # cosmic-session's ProcessManager uses ExponentialBackoff with
    # max_restarts = usize::MAX, so killing the child triggers an
    # automatic re-spawn with the new binary on the next exec().
    if [ "$name" = "cosmic-session" ]; then
        info "restarting cosmic-session means restarting the whole DE"
        cmd_stop
        cmd_start
        return
    fi
    if [ "$name" = "cosmic-comp" ]; then
        # comp is the root of the Wayland tree — killing it nukes all
        # children. Restart the whole session instead.
        info "cosmic-comp is the root — restarting the whole session"
        cmd_stop
        cmd_start
        return
    fi

    info "killing $name — cosmic-session will respawn it from $bin"
    # Match the exact binary path to avoid killing host-installed copies.
    pkill -TERM -f "^$bin( |$)" || pkill -TERM -x "$name" || true
}

cmd_status() {
    if session_running; then
        local pid
        pid="$(cat "$PID_FILE")"
        info "nested session running (pid $pid)"
        echo
        printf '%-26s %-7s %s\n' COMPONENT PID BIN
        for entry in "${COMPONENTS[@]}"; do
            local name="${entry%%:*}"
            local bin
            bin="$(component_bin "$name")"
            local cpid
            cpid="$(pgrep -f "^$bin( |$)" | head -1 || true)"
            printf '%-26s %-7s %s\n' "$name" "${cpid:--}" "$bin"
        done
    else
        info "no nested session running"
    fi
}

cmd_logs() {
    [ -f "$LOG_FILE" ] || die "no log at $LOG_FILE"
    if [ "${1:-}" = "-f" ]; then
        tail -F "$LOG_FILE"
    else
        tail -n 200 "$LOG_FILE"
    fi
}

usage() {
    sed -n '2,/^set -euo/p' "$0" | sed -n '/^# /s/^# \{0,1\}//p' | sed '$d'
}

# -----------------------------------------------------------------------------
# Dispatch.
# -----------------------------------------------------------------------------

[ "$#" -ge 1 ] || { usage; exit 1; }
cmd="$1"; shift
case "$cmd" in
    build)      cmd_build "$@" ;;
    start)      cmd_start "$@" ;;
    stop)       cmd_stop "$@" ;;
    restart)    cmd_restart "$@" ;;
    status)     cmd_status "$@" ;;
    logs)       cmd_logs "$@" ;;
    components) cmd_components ;;
    -h|--help)  usage ;;
    *)          echo "unknown command: $cmd" >&2; usage >&2; exit 1 ;;
esac
