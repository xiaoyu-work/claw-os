#!/bin/sh
# Flatten the managed OverlayFS home before claw-os-base is removed.
#
# The merged mount is the only authoritative view: copying the upper layer
# directly would mishandle whiteouts, opaque directories, and lower metadata.
# Snapshot the read-only merged view, detach it, then replace the underlay.

OVERLAY_DIR=/var/lib/cos/overlay
BASE="$OVERLAY_DIR/base"
PERSISTENT_UPPER="$OVERLAY_DIR/upper"
PERSISTENT_WORK="$OVERLAY_DIR/work"
RUNTIME_UPPER=/run/cos-overlay/upper
RUNTIME_WORK=/run/cos-overlay/work
TARGET_FILE="$OVERLAY_DIR/mount-target"
SNAPSHOT="$OVERLAY_DIR/removal-snapshot"
SNAPSHOT_COMPLETE_FILE="$OVERLAY_DIR/removal-snapshot.complete"
BACKUP="$OVERLAY_DIR/removal-underlay"

ACTIVE_UPPER=
ACTIVE_WORK=
MIGRATION_MODE=

message() {
    printf 'claw-os-base: %s\n' "$*" >&2
}

path_exists() {
    [ -e "$1" ] || [ -L "$1" ]
}

valid_home_target() {
    case "$1" in
        /root|/home/?*) ;;
        *) return 1 ;;
    esac

    # findmnt's raw output escapes whitespace and backslashes. Refuse unusual
    # paths rather than risk operating on a decoded approximation.
    case "$1" in
        *[!A-Za-z0-9_./-]*) return 1 ;;
    esac

    [ ! -L "$1" ] || return 1
    [ "$(readlink -f -- "$1" 2>/dev/null)" = "$1" ]
}

option_is_set() {
    options=$1
    expected=$2
    case ",$options," in
        *",$expected,"*) return 0 ;;
        *) return 1 ;;
    esac
}

classify_overlay_options() {
    options=$1
    ACTIVE_UPPER=
    ACTIVE_WORK=

    option_is_set "$options" "lowerdir=$BASE" || return 1
    if option_is_set "$options" "upperdir=$PERSISTENT_UPPER" &&
        option_is_set "$options" "workdir=$PERSISTENT_WORK"; then
        ACTIVE_UPPER=$PERSISTENT_UPPER
        ACTIVE_WORK=$PERSISTENT_WORK
        return 0
    fi
    if option_is_set "$options" "upperdir=$RUNTIME_UPPER" &&
        option_is_set "$options" "workdir=$RUNTIME_WORK"; then
        ACTIVE_UPPER=$RUNTIME_UPPER
        ACTIVE_WORK=$RUNTIME_WORK
        return 0
    fi
    return 1
}

managed_overlay_at() {
    target=$1
    fstype=$(findmnt -rn -M "$target" -o FSTYPE 2>/dev/null) || return 1
    case "$fstype" in
        overlay|overlayfs) ;;
        *) return 1 ;;
    esac
    options=$(findmnt -rn -M "$target" -o OPTIONS 2>/dev/null) || return 1
    classify_overlay_options "$options"
}

find_managed_targets() {
    findmnt -rn --raw -t overlay,overlayfs -o TARGET 2>/dev/null |
        while IFS= read -r candidate; do
            valid_home_target "$candidate" || continue
            if managed_overlay_at "$candidate"; then
                printf '%s\n' "$candidate"
            fi
        done
}

path_has_mounts() {
    checked=$1
    mount_targets=$(findmnt -rn --raw -o TARGET 2>/dev/null) || return 0
    while IFS= read -r mounted; do
        case "$mounted" in
            "$checked"|"$checked"/*) return 0 ;;
        esac
    done <<EOF
$mount_targets
EOF
    return 1
}

target_has_descendant_mounts() {
    checked=$1
    mount_targets=$(findmnt -rn --raw -o TARGET 2>/dev/null) || return 0
    while IFS= read -r mounted; do
        case "$mounted" in
            "$checked"/*) return 0 ;;
        esac
    done <<EOF
$mount_targets
EOF
    return 1
}

overlay_state_exists() {
    for path in \
        "$TARGET_FILE" "$BASE" "$PERSISTENT_UPPER" "$PERSISTENT_WORK" \
        "$RUNTIME_UPPER" "$RUNTIME_WORK" "$SNAPSHOT" \
        "$SNAPSHOT_COMPLETE_FILE" "$BACKUP"; do
        if path_exists "$path"; then
            return 0
        fi
    done
    return 1
}

snapshot_visible_home() {
    source_home=$1
    destination=$2

    umask 077
    cp -a --one-file-system -- "$source_home" "$destination"
}

remove_tree_safely() {
    tree=$1
    path_exists "$tree" || return 0
    if path_has_mounts "$tree"; then
        message "refusing to remove mounted recovery/state path $tree"
        return 1
    fi
    if [ -L "$tree" ] || [ ! -d "$tree" ]; then
        rm -f -- "$tree"
        return
    fi
    find "$tree" -xdev -mindepth 1 -delete && rmdir -- "$tree"
}

replace_tree_atomically() {
    target=$1
    snapshot=$2
    backup=$3

    mv -- "$target" "$backup" || return 1
    if mv -- "$snapshot" "$target"; then
        MIGRATION_MODE=atomic
        return 0
    fi

    message "could not install the home snapshot; restoring the underlay"
    mv -- "$backup" "$target" || true
    return 1
}

replace_tree_in_place() {
    target=$1
    snapshot=$2

    # Clearing first materializes OverlayFS deletions: names hidden by
    # whiteouts in the merged view must not survive in the old underlay.
    find "$target" -xdev -mindepth 1 -delete || return 1
    cp -a --one-file-system -- "$snapshot/." "$target/" || return 1
    MIGRATION_MODE=in-place
}

remount_managed_overlay() {
    target=$1
    mkdir -p -- "$target" || return 1
    mount -t overlay overlay \
        -o "lowerdir=$BASE,upperdir=$ACTIVE_UPPER,workdir=$ACTIVE_WORK" \
        "$target"
}

restore_writable_mount() {
    target=$1
    if mountpoint -q "$target" 2>/dev/null; then
        mount -o remount,rw "$target"
    else
        remount_managed_overlay "$target"
    fi
}

print_recovery() {
    target=$1
    message "package removal has been stopped; no OverlayFS state was discarded."
    if path_exists "$SNAPSHOT"; then
        if [ -f "$SNAPSHOT_COMPLETE_FILE" ]; then
            message "the complete merged-home snapshot is retained at $SNAPSHOT"
        else
            message "an incomplete staging tree may remain at $SNAPSHOT; do not use it as the only recovery source"
        fi
    fi
    if [ -n "$target" ]; then
        message "restore the managed view with:"
        printf '  sudo /usr/lib/cos/init/setup-home.sh %s\n' "$target" >&2
    fi
    message "close processes and unmount filesystems below the home, then retry:"
    printf '  sudo apt remove claw-os-base\n' >&2
    message "do not delete $OVERLAY_DIR or $SNAPSHOT while recovering."
}

fail_with_recovery() {
    reason=$1
    target=${2:-}
    message "refusing removal: $reason"
    print_recovery "$target"
    return 1
}

cleanup_overlay_state() {
    for tree in \
        "$BASE" "$PERSISTENT_UPPER" "$PERSISTENT_WORK" \
        "$RUNTIME_UPPER" "$RUNTIME_WORK"; do
        remove_tree_safely "$tree" || return 1
    done

    remove_tree_safely "$SNAPSHOT" || return 1
    remove_tree_safely "$BACKUP" || return 1
    rm -f -- "$SNAPSHOT_COMPLETE_FILE" || return 1
    rm -f -- "$TARGET_FILE" || return 1
    rmdir -- /run/cos-overlay 2>/dev/null || true
    rmdir -- "$OVERLAY_DIR" 2>/dev/null || true
}

flatten_managed_overlay() {
    target=$1

    # A verified live merged mount is authoritative. A prior snapshot can be
    # discarded and rebuilt, but retain an old underlay until this attempt
    # finishes; its presence selects the in-place path below.
    if path_exists "$SNAPSHOT"; then
        message "discarding an earlier removal snapshot after verifying the managed mount"
        remove_tree_safely "$SNAPSHOT" || {
            fail_with_recovery \
                "a previous removal snapshot could not be cleared" "$target"
            return 1
        }
    fi
    rm -f -- "$SNAPSHOT_COMPLETE_FILE" || {
        fail_with_recovery \
            "a previous snapshot marker could not be cleared" "$target"
        return 1
    }
    if target_has_descendant_mounts "$target"; then
        fail_with_recovery \
            "$target has mounted descendants" "$target"
        return 1
    fi

    # A read-only remount makes the merged namespace stable while it is
    # copied. If open writers prevent this, keep the package and visible view.
    if ! mount -o remount,ro "$target"; then
        fail_with_recovery \
            "could not make $target read-only for a consistent snapshot" \
            "$target"
        return 1
    fi

    if ! snapshot_visible_home "$target" "$SNAPSHOT"; then
        restore_writable_mount "$target" || true
        remove_tree_safely "$SNAPSHOT" || true
        fail_with_recovery "could not snapshot the merged home" "$target"
        return 1
    fi
    if ! sync -f "$SNAPSHOT"; then
        restore_writable_mount "$target" || true
        fail_with_recovery "could not flush the merged-home snapshot" "$target"
        return 1
    fi
    if ! printf 'complete\n' > "$SNAPSHOT_COMPLETE_FILE" ||
        ! sync -f "$SNAPSHOT_COMPLETE_FILE"; then
        restore_writable_mount "$target" || true
        fail_with_recovery \
            "could not record a durable merged-home snapshot" "$target"
        return 1
    fi

    if ! umount "$target"; then
        if restore_writable_mount "$target"; then
            remove_tree_safely "$SNAPSHOT" || true
        fi
        fail_with_recovery "could not unmount $target cleanly" "$target"
        return 1
    fi

    if target_has_descendant_mounts "$target"; then
        remount_managed_overlay "$target" || true
        fail_with_recovery \
            "the underlying home has mounted descendants" "$target"
        return 1
    fi

    target_parent=$(dirname -- "$target")
    if ! path_exists "$BACKUP" &&
        ! mountpoint -q "$target" 2>/dev/null &&
        [ "$(stat -c '%d' "$SNAPSHOT")" = "$(stat -c '%d' "$target_parent")" ]; then
        if ! replace_tree_atomically "$target" "$SNAPSHOT" "$BACKUP"; then
            # Btrfs subvolumes can report one device while rejecting rename
            # across subvolume boundaries. If rollback restored the target,
            # use the guarded in-place path instead.
            if path_exists "$target" && ! path_exists "$BACKUP"; then
                replace_tree_in_place "$target" "$SNAPSHOT" || {
                    remount_managed_overlay "$target" || true
                    fail_with_recovery \
                        "could not install the merged-home snapshot" "$target"
                    return 1
                }
            else
                remount_managed_overlay "$target" || true
                fail_with_recovery \
                    "could not atomically install the merged-home snapshot" \
                    "$target"
                return 1
            fi
        fi
    else
        if ! replace_tree_in_place "$target" "$SNAPSHOT"; then
            remount_managed_overlay "$target" || true
            fail_with_recovery \
                "could not materialize the merged home in its backing filesystem" \
                "$target"
            return 1
        fi
    fi

    if ! sync -f "$target"; then
        remount_managed_overlay "$target" || true
        fail_with_recovery "could not flush the flattened home" "$target"
        return 1
    fi

    if ! cleanup_overlay_state; then
        message "refusing removal: the home is visible and flattened at $target,"
        message "but stale OverlayFS state could not be removed."
        message "the package remains installed; preserve $OVERLAY_DIR and retry after repairing its permissions or filesystem."
        return 1
    fi

    message "preserved the merged home at $target and unmounted its OverlayFS"
}

main() {
    for required_command in \
        cp dirname find findmnt mkdir mount mountpoint mv readlink rm rmdir \
        stat sync umount; do
        if ! command -v "$required_command" >/dev/null 2>&1; then
            fail_with_recovery \
                "required command '$required_command' is unavailable" ""
            return 1
        fi
    done

    managed_targets=$(find_managed_targets)
    case "$managed_targets" in
        "")
            if overlay_state_exists; then
                recovery_target=
                if [ -r "$TARGET_FILE" ]; then
                    IFS= read -r recovery_target < "$TARGET_FILE" || true
                    valid_home_target "$recovery_target" ||
                        recovery_target=
                fi
                fail_with_recovery \
                    "OverlayFS state exists without an active verified managed-home mount" \
                    "$recovery_target"
                return 1
            fi
            message "no managed home OverlayFS is active"
            return 0
            ;;
        *"
"*)
            fail_with_recovery \
                "more than one managed-home OverlayFS mount is active" ""
            return 1
            ;;
    esac

    target=$managed_targets
    if ! managed_overlay_at "$target"; then
        fail_with_recovery \
            "the managed mount changed while removal was starting" "$target"
        return 1
    fi
    flatten_managed_overlay "$target"
}

if [ "${0##*/}" = remove-home-overlay.sh ]; then
    set -u
    main "$@"
fi
