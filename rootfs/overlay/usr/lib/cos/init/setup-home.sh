#!/bin/bash
# /usr/lib/cos/init/setup-home.sh — Set up OverlayFS on the agent's
# writable home directory.
#
# Used by:
#   - cos-home-setup.service (systemd unit)   (Docker / WSL / ISO / VM targets)
#   - /usr/local/bin/cos-init                 (recovery/debug shell helper)
#
# Target path resolution (Linux-native, no hardcoded /home/<user>):
#   1. First positional argument, if given.
#   2. $COS_HOME, if set.
#   3. The first existing human user's passwd home.
#   4. If no human user exists yet, defer without creating a directory.
#
# Behaviour:
#   - Idempotent (no-op if the target is already a mount).
#   - On non-Linux or where mount(8) is missing, exits 0 without doing anything.
#   - When the backing filesystem is already an overlay (live ISOs ship a
#     squashfs+overlay rootfs), upper/work move to tmpfs under /run, since
#     overlay-on-overlay is not reliably supported and would otherwise fail.
#   - Always exits 0 — overlay-mount failure is non-fatal; cos still works
#     without checkpoint support, with a JSON warning emitted to stdout.

set -e

TARGET="${1:-${COS_HOME:-}}"

if [ -z "$TARGET" ]; then
    account=$(awk -F: \
        '$3 >= 1000 && $3 < 65534 && $7 !~ /(nologin|false)$/ { print; exit }' \
        /etc/passwd)
    if [ -z "$account" ]; then
        printf '{"overlay": "deferred", "reason": "no human user exists yet"}\n'
        exit 0
    fi
    IFS=: read -r _ _ target_uid target_gid _ TARGET _ <<EOF
$account
EOF
else
    target_uid=
    target_gid=
    case "$TARGET" in
        /home/*)
            account=$(awk -F: -v home="$TARGET" '$6 == home { print; exit }' /etc/passwd)
            if [ -z "$account" ]; then
                printf '{"overlay": "deferred", "path": "%s", "reason": "home owner does not exist yet"}\n' "$TARGET"
                exit 0
            fi
            IFS=: read -r _ _ target_uid target_gid _ _ _ <<EOF
$account
EOF
            ;;
    esac
fi

case "$TARGET" in
    /root|/home/*) ;;
    *)
        printf '{"overlay": "failed", "path": "%s", "error": "target must be /root or a local /home path"}\n' "$TARGET"
        exit 0
        ;;
esac
if [ -L "$TARGET" ]; then
    printf '{"overlay": "failed", "path": "%s", "error": "target must not be a symlink"}\n' "$TARGET"
    exit 0
fi

OVERLAY_DIR="/var/lib/cos/overlay"
BASE="$OVERLAY_DIR/base"
UPPER="$OVERLAY_DIR/upper"
WORK="$OVERLAY_DIR/work"

if mountpoint -q "$TARGET" 2>/dev/null; then
    printf '{"overlay": "already-mounted", "path": "%s"}\n' "$TARGET"
    exit 0
fi

if [ "$(uname)" != "Linux" ] || ! command -v mount >/dev/null 2>&1; then
    exit 0
fi

mkdir -p "$TARGET"
if [ "$(readlink -f "$TARGET")" != "$TARGET" ]; then
    printf '{"overlay": "failed", "path": "%s", "error": "target path is not canonical"}\n' "$TARGET"
    exit 0
fi
if [ -n "${target_uid:-}" ]; then
    chown "$target_uid:$target_gid" "$TARGET"
else
    target_uid=$(stat -c '%u' "$TARGET")
    target_gid=$(stat -c '%g' "$TARGET")
fi

# Detect overlay-backed rootfs (Debian live media). On those, upper/work
# must live on a non-overlay filesystem — use tmpfs at /run/cos-overlay.
mkdir -p "$OVERLAY_DIR"
backing_fs=$(findmnt -no FSTYPE -T "$OVERLAY_DIR" 2>/dev/null || echo unknown)
if [ "$backing_fs" = "overlay" ] || [ "$backing_fs" = "overlayfs" ]; then
    UPPER="/run/cos-overlay/upper"
    WORK="/run/cos-overlay/work"
fi

mkdir -p "$BASE" "$UPPER" "$WORK"

# upper becomes the visible owner of $TARGET after mount; base holds the
# seeded content and must be readable as the same user. work is overlay
# internal — kept root-owned, mode 0700 enforced by the kernel.
chown "$target_uid:$target_gid" "$BASE" "$UPPER"
chmod 0700 "$BASE" "$UPPER"

# First boot: seed the base layer from whatever the image shipped at the
# target path (so existing dotfiles / project skeletons survive).
if [ -z "$(ls -A "$BASE" 2>/dev/null)" ] && [ -d "$TARGET" ]; then
    cp -a "$TARGET/." "$BASE/" 2>/dev/null || true
fi

if mount_error=$(mount -t overlay overlay \
    -o "lowerdir=$BASE,upperdir=$UPPER,workdir=$WORK" \
    "$TARGET" 2>&1); then
    printf '{"overlay": "mounted", "path": "%s", "upper": "%s"}\n' "$TARGET" "$UPPER"
else
    printf '{"overlay": "failed", "path": "%s", "error": "%s", "warning": "checkpoints disabled — run with --privileged or --cap-add SYS_ADMIN"}\n' \
        "$TARGET" "$mount_error"
fi
