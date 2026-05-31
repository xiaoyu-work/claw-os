#!/usr/bin/env bash
# rootfs/build.sh — Build a Debian rootfs by composing features.
#
# 1. Bootstraps a minimal Debian rootfs into build/claw-os-rootfs (always).
# 2. Copies rootfs/overlay/* on top (always).
# 3. Applies each feature in order: apt-installs its packages.txt, then runs
#    its install.sh.
#
# See rootfs/features/README.md for the feature contract.
#
# Usage:
#   sudo ./rootfs/build.sh [--features f1,f2,f3]
#
# Default features: base,cos-core,browser  (matches the legacy behaviour).

set -euo pipefail

# ---------------------------------------------------------------------------
# Detach from the controlling terminal (root-cause fix for "build randomly
# stops / hangs" on WSL2).
#
# Symptom: the build wedges with every process in state `T+` (stopped, still
# the terminal's *foreground* group) at an apt/dpkg step — "no key was ever
# pressed". A foreground group is stopped only by SIGTSTP/SIGSTOP, and WSL2's
# tty/pty layer can spuriously deliver SIGTSTP to the build's process group
# while apt/dpkg drives its progress pty (tcsetpgrp). Disabling apt's pty
# (Dpkg::Use-Pty 0, below) helps but is not sufficient on its own.
#
# The only airtight fix is to give the build NO controlling terminal at all:
# with no tty there is nothing that can generate a job-control stop signal.
# Re-exec ourselves under setsid with stdin from /dev/null. stdout/stderr are
# inherited unchanged, so `... | tee build.log` and the caller's
# ${PIPESTATUS[0]} keep working. Skipped when stdin is not a tty (e.g. CI),
# where there is no controlling terminal to detach from.
if [ -z "${COS_BUILD_DETACHED:-}" ] && [ -t 0 ] && command -v setsid >/dev/null 2>&1; then
    export COS_BUILD_DETACHED=1
    exec setsid -w "$0" "$@" < /dev/null
fi

# Never let apt/dpkg or any feature install.sh prompt on (or grab) a terminal.
# Combined with the detach above and the Dpkg::Use-Pty drop-in written into the
# chroot below, this keeps every apt invocation fully non-interactive so it can
# neither block on a prompt nor be suspended by terminal job control.
# Exported here so it propagates to every child feature install.sh too.
export DEBIAN_FRONTEND=noninteractive

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
ROOTFS="$PROJECT_DIR/build/claw-os-rootfs"
SUITE="trixie"

# Architecture mapping ($ARCH, $DEB_ARCH, $KERNEL_PKG, …). Defaults to host
# arch when $ARCH is unset.
source "$PROJECT_DIR/scripts/lib/arch.sh"

DEFAULT_FEATURES="base,cos-core,browser"
FEATURES="$DEFAULT_FEATURES"

usage() {
    cat <<EOF
Usage: $0 [--features <list>]

Build a Debian rootfs at $ROOTFS by composing features.

Options:
  --features <list>   Comma-separated feature names (default: $DEFAULT_FEATURES)
  -h, --help          Show this help

Available features:
EOF
    if [ -d "$SCRIPT_DIR/features" ]; then
        for d in "$SCRIPT_DIR"/features/*/; do
            [ -d "$d" ] || continue
            echo "  $(basename "$d")"
        done
    fi
}

while [ $# -gt 0 ]; do
    case "$1" in
        --features)
            FEATURES="$2"
            shift 2
            ;;
        --features=*)
            FEATURES="${1#--features=}"
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "error: unknown argument '$1'" >&2
            usage >&2
            exit 1
            ;;
    esac
done

# Parse features and validate before doing anything else (works without root).
IFS=',' read -ra FEATURE_LIST <<< "$FEATURES"
for f in "${FEATURE_LIST[@]}"; do
    if [ -z "$f" ]; then
        echo "error: empty feature name in '$FEATURES'" >&2
        exit 1
    fi
    if [ ! -d "$SCRIPT_DIR/features/$f" ]; then
        echo "error: unknown feature '$f' (no $SCRIPT_DIR/features/$f directory)" >&2
        exit 1
    fi
done

if [ "$(id -u)" -ne 0 ]; then
    echo "error: must run as root" >&2
    exit 1
fi

# Read version from Cargo.toml (single source of truth).
COS_VERSION=$(grep '^version' "$PROJECT_DIR/core/Cargo.toml" | head -1 | sed 's/.*"\(.*\)".*/\1/')

export ROOTFS PROJECT_DIR SCRIPT_DIR SUITE COS_VERSION ARCH DEB_ARCH KERNEL_PKG

echo ":: features: ${FEATURE_LIST[*]}"
echo ":: arch:     $ARCH (deb=$DEB_ARCH, kernel=$KERNEL_PKG)"

# 1. Bootstrap minimal Debian rootfs.
echo ":: debootstrap --arch=$DEB_ARCH $SUITE -> $ROOTFS"
mkdir -p "$ROOTFS"
debootstrap --extractor=ar --arch="$DEB_ARCH" "$SUITE" "$ROOTFS"

# 2. Apply global overlay (config files, cos-init, etc.).
echo ":: applying global overlay"
cp -a "$SCRIPT_DIR/overlay/." "$ROOTFS/"

# 2b. Bind-mount kernel pseudofs and propagate resolv.conf into the chroot.
# Needed by chroot operations more involved than a plain `apt-get install`:
# - systemctl enable (wants /proc/1/comm to detect systemd)
# - plymouth-set-default-theme (reads /proc/cmdline)
# - rustup/cargo (read /proc/self/exe, /proc/cpuinfo; spawn child procs)
# - any package's postinst that runs `update-initramfs`, `ldconfig`, etc.
# debootstrap leaves /etc/resolv.conf empty; copy the host's so apt + curl
# can resolve names inside the chroot.
echo ":: setting up chroot bind mounts"
mkdir -p "$ROOTFS/proc" "$ROOTFS/sys" "$ROOTFS/dev" "$ROOTFS/dev/pts" "$ROOTFS/run"
mount --bind /proc "$ROOTFS/proc"
mount --bind /sys "$ROOTFS/sys"
mount --bind /dev "$ROOTFS/dev"
mount --bind /dev/pts "$ROOTFS/dev/pts"
if [ -e /etc/resolv.conf ]; then
    cp -L /etc/resolv.conf "$ROOTFS/etc/resolv.conf"
fi
install -d "$ROOTFS/etc/apt/apt.conf.d"
cat > "$ROOTFS/etc/apt/apt.conf.d/80cos-retries" <<'EOF'
Acquire::Retries "5";
Acquire::http::Timeout "30";
Acquire::https::Timeout "30";
DPkg::Lock::Timeout "60";
EOF
# Disable apt's pseudo-terminal for dpkg/maintainer scripts. The pty performs
# terminal job-control (tcsetpgrp) that, under WSL2, sends SIGTTOU/SIGTTIN to
# the build's process group and stops it (state `T`). Setting this in the
# chroot's apt config covers EVERY apt-get call — chroot_apt_get below AND the
# direct `chroot apt-get` calls in feature install.sh scripts.
cat > "$ROOTFS/etc/apt/apt.conf.d/81cos-no-pty" <<'EOF'
Dpkg::Use-Pty "0";
EOF

# debootstrap writes a single-component sources.list (`main` only).
# Claw OS needs `contrib` (e.g. some codec headers) and especially
# `non-free-firmware` (Intel/Realtek/Broadcom Wi-Fi blobs, CPU
# microcode). `non-free` covers anything still parked there that
# trixie hasn't migrated. `*-backports` provides packages dropped from
# trixie main (e.g. ydotool, only in trixie-backports). Overwrite both
# the legacy file and the deb822 file (whichever debootstrap chose to
# use), so apt sees the extra components on the very first
# `apt-get update`.
echo ":: enabling contrib / non-free-firmware / non-free components"
cat > "$ROOTFS/etc/apt/sources.list" <<EOF
deb http://deb.debian.org/debian $SUITE main contrib non-free-firmware non-free
deb http://deb.debian.org/debian $SUITE-updates main contrib non-free-firmware non-free
deb http://deb.debian.org/debian $SUITE-backports main contrib non-free-firmware non-free
deb http://security.debian.org/debian-security $SUITE-security main contrib non-free-firmware non-free
EOF
# Newer debootstrap variants emit /etc/apt/sources.list.d/debian.sources
# in deb822 format; if present, override it so our components win.
if [ -f "$ROOTFS/etc/apt/sources.list.d/debian.sources" ]; then
    cat > "$ROOTFS/etc/apt/sources.list.d/debian.sources" <<EOF
Types: deb
URIs: http://deb.debian.org/debian
Suites: $SUITE $SUITE-updates $SUITE-backports
Components: main contrib non-free-firmware non-free
Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg

Types: deb
URIs: http://security.debian.org/debian-security
Suites: $SUITE-security
Components: main contrib non-free-firmware non-free
Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg
EOF
fi

cleanup_chroot_mounts() {
    # Unmount in reverse order, lazy fallback for stray references.
    for mp in "$ROOTFS/dev/pts" "$ROOTFS/dev" "$ROOTFS/sys" "$ROOTFS/proc"; do
        if mountpoint -q "$mp"; then
            umount "$mp" 2>/dev/null || umount -l "$mp" 2>/dev/null || true
        fi
    done
}
trap cleanup_chroot_mounts EXIT

chroot_apt_get() {
    local attempt=1
    local max_attempts=3
    local delay=5
    local rc=0
    # Run apt fully non-interactively and DETACHED from the controlling
    # terminal:
    #   * DEBIAN_FRONTEND=noninteractive — never prompt via debconf.
    #   * -o Dpkg::Use-Pty=0 — do NOT allocate a pty for maintainer scripts.
    #     apt's default pty does terminal job-control (tcsetpgrp), which under
    #     WSL2 delivers SIGTTOU/SIGTTIN to our foreground process group and
    #     STOPS the whole build (processes wedge in state `T`, looking "hung"
    #     at "Processing triggers …" with no key ever pressed).
    #   * < /dev/null — give children no terminal to read from, so nothing can
    #     block on / grab the tty.
    while true; do
        if DEBIAN_FRONTEND=noninteractive \
            chroot "$ROOTFS" apt-get -o Dpkg::Use-Pty=0 "$@" < /dev/null; then
            return 0
        fi
        rc=$?
        if [ "$attempt" -ge "$max_attempts" ]; then
            return "$rc"
        fi
        echo "  :: apt-get $* failed (attempt $attempt/$max_attempts); retrying in ${delay}s" >&2
        sleep "$delay"
        attempt=$((attempt + 1))
        delay=$((delay * 2))
    done
}

# 3. Apply each feature in order.
for f in "${FEATURE_LIST[@]}"; do
    feature_dir="$SCRIPT_DIR/features/$f"
    echo "===> feature: $f"

    # 3a. Install packages.txt entries via apt inside chroot.
    if [ -f "$feature_dir/packages.txt" ]; then
        # Strip comments and blank lines. Guard with `|| true` so a
        # packages.txt that is entirely comments (e.g. apt-source) does
        # not return exit 1 from grep and trip `set -o pipefail`.
        # Expand ${VAR} references (KERNEL_PKG, DEB_ARCH, …) against the
        # exported env so a single packages.txt can name an arch-specific
        # kernel package. packages.txt is a repo file (trusted), and
        # we wrap the line in double quotes so eval only expands ${…}
        # / $… and does not run command substitution / globbing on
        # plain package names.
        pkgs=""
        while IFS= read -r _line; do
            case "$_line" in ''|\#*) continue ;; esac
            eval "_expanded=\"$_line\""
            # An empty expansion (e.g. ${GRUB_BIOS_PKG} on arm64) means
            # "no package needed on this arch" — skip silently.
            # shellcheck disable=SC2154  # _expanded is assigned by eval above
            [ -z "$_expanded" ] && continue
            pkgs="$pkgs $_expanded"
        done < <(grep -vE '^\s*(#|$)' "$feature_dir/packages.txt" || true)
        if [ -n "${pkgs// /}" ]; then
            echo "  :: apt install$pkgs"
            # NB: chroot_apt_get is a function whose body uses if/while, so a
            # non-zero return does NOT trip the caller's `set -e` (a known bash
            # errexit gotcha). Check explicitly and abort, otherwise a feature
            # whose packages fail to install would silently continue into its
            # install.sh and fail later with a confusing error.
            chroot_apt_get update -qq \
                || { echo "error: feature '$f': apt-get update failed" >&2; exit 1; }
            chroot_apt_get install -y --no-install-recommends $pkgs \
                || { echo "error: feature '$f': failed to install packages.txt packages" >&2; exit 1; }
            chroot_apt_get clean || true
            rm -rf "$ROOTFS/var/lib/apt/lists"/*
        fi
    fi

    # 3b. Run install.sh on the host.
    if [ -f "$feature_dir/install.sh" ]; then
        echo "  :: running install.sh"
        if [ -x "$feature_dir/install.sh" ]; then
            "$feature_dir/install.sh"
        else
            bash "$feature_dir/install.sh"
        fi
    fi
done

echo ":: done — rootfs at $ROOTFS"
