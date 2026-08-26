#!/bin/bash

set -euo pipefail

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PROJECT_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/../../.." && pwd)
TEST_ROOT="$PROJECT_DIR/build/test-remove-home-overlay.$$"
INTEGRATION_FS="$TEST_ROOT/integration-fs"

cleanup() {
    if mountpoint -q "$INTEGRATION_FS/home" 2>/dev/null; then
        umount "$INTEGRATION_FS/home" || true
    fi
    while mountpoint -q "$INTEGRATION_FS/mounted-home" 2>/dev/null; do
        umount "$INTEGRATION_FS/mounted-home" || break
    done
    if mountpoint -q "$INTEGRATION_FS" 2>/dev/null; then
        umount "$INTEGRATION_FS" || true
    fi
    rm -rf -- "$TEST_ROOT"
}
trap cleanup EXIT

# Sourcing exposes the migration primitives without running the root-only main.
. "$PROJECT_DIR/rootfs/overlay/usr/lib/cos/init/remove-home-overlay.sh"

fail() {
    printf 'not ok - %s\n' "$*" >&2
    exit 1
}

assert_file_contains() {
    file=$1
    expected=$2
    [ "$(cat -- "$file")" = "$expected" ] ||
        fail "$file did not contain '$expected'"
}

assert_same_inode() {
    left=$1
    right=$2
    [ "$(stat -c '%d:%i' "$left")" = "$(stat -c '%d:%i' "$right")" ] ||
        fail "$left and $right are not hard-linked"
}

mkdir -p "$TEST_ROOT/state"
OVERLAY_DIR="$TEST_ROOT/state"
BASE="$OVERLAY_DIR/base"
PERSISTENT_UPPER="$OVERLAY_DIR/upper"
PERSISTENT_WORK="$OVERLAY_DIR/work"
RUNTIME_UPPER="$TEST_ROOT/run/upper"
RUNTIME_WORK="$TEST_ROOT/run/work"
TARGET_FILE="$OVERLAY_DIR/mount-target"
SNAPSHOT="$OVERLAY_DIR/removal-snapshot"
SNAPSHOT_COMPLETE_FILE="$OVERLAY_DIR/removal-snapshot.complete"
BACKUP="$OVERLAY_DIR/removal-underlay"

classify_overlay_options \
    "rw,relatime,lowerdir=$BASE,upperdir=$PERSISTENT_UPPER,workdir=$PERSISTENT_WORK" ||
    fail "persistent OverlayFS options were not recognized"
[ "$ACTIVE_UPPER" = "$PERSISTENT_UPPER" ] ||
    fail "persistent upper directory was not selected"

classify_overlay_options \
    "rw,lowerdir=$BASE,upperdir=$RUNTIME_UPPER,workdir=$RUNTIME_WORK" ||
    fail "runtime OverlayFS options were not recognized"
[ "$ACTIVE_WORK" = "$RUNTIME_WORK" ] ||
    fail "runtime work directory was not selected"

if classify_overlay_options \
    "rw,lowerdir=$BASE,upperdir=$PERSISTENT_UPPER,workdir=$TEST_ROOT/wrong"; then
    fail "mismatched OverlayFS state was accepted"
fi

visible="$TEST_ROOT/visible"
mkdir -p "$visible/.config" "$visible/opaque"
printf 'new value\n' > "$visible/.config/settings"
printf 'new child\n' > "$visible/opaque/new"
ln "$visible/.config/settings" "$visible/settings-hardlink"
ln -s .config/settings "$visible/settings-link"
chmod 0710 "$visible"
chmod 0640 "$visible/.config/settings"
touch -t 202001020304.05 "$visible/.config/settings"

snapshot="$TEST_ROOT/snapshot"
snapshot_visible_home "$visible" "$snapshot" ||
    fail "the visible tree could not be snapshotted"
assert_file_contains "$snapshot/.config/settings" "new value"
assert_same_inode "$snapshot/.config/settings" "$snapshot/settings-hardlink"
[ "$(readlink -- "$snapshot/settings-link")" = ".config/settings" ] ||
    fail "snapshot did not preserve a symbolic link"
[ "$(stat -c '%Y' "$snapshot/.config/settings")" = \
    "$(stat -c '%Y' "$visible/.config/settings")" ] ||
    fail "snapshot did not preserve timestamps"

underlay="$TEST_ROOT/underlay"
backup="$TEST_ROOT/underlay-backup"
mkdir -p "$underlay/opaque"
printf 'must disappear\n' > "$underlay/deleted-by-whiteout"
printf 'must disappear\n' > "$underlay/opaque/lower-child"

replace_tree_atomically "$underlay" "$snapshot" "$backup" ||
    fail "atomic migration failed"
[ "$MIGRATION_MODE" = atomic ] || fail "atomic migration was not reported"
[ ! -e "$underlay/deleted-by-whiteout" ] ||
    fail "a whiteout-hidden underlay file survived atomic migration"
[ ! -e "$underlay/opaque/lower-child" ] ||
    fail "an opaque-directory lower child survived atomic migration"
assert_file_contains "$underlay/opaque/new" "new child"
assert_same_inode "$underlay/.config/settings" "$underlay/settings-hardlink"

snapshot="$TEST_ROOT/in-place-snapshot"
mkdir -p "$snapshot/opaque"
printf 'replacement\n' > "$snapshot/current"
printf 'new child\n' > "$snapshot/opaque/new"
ln "$snapshot/current" "$snapshot/current-hardlink"
chmod 0701 "$snapshot"

mounted_underlay="$TEST_ROOT/mounted-underlay"
mkdir -p "$mounted_underlay/opaque"
printf 'must disappear\n' > "$mounted_underlay/deleted-by-whiteout"
printf 'must disappear\n' > "$mounted_underlay/opaque/lower-child"

replace_tree_in_place "$mounted_underlay" "$snapshot" ||
    fail "in-place migration failed"
[ "$MIGRATION_MODE" = in-place ] || fail "in-place migration was not reported"
[ ! -e "$mounted_underlay/deleted-by-whiteout" ] ||
    fail "a whiteout-hidden underlay file survived in-place migration"
[ ! -e "$mounted_underlay/opaque/lower-child" ] ||
    fail "an opaque-directory lower child survived in-place migration"
assert_file_contains "$mounted_underlay/current" "replacement"
assert_same_inode "$mounted_underlay/current" "$mounted_underlay/current-hardlink"

# DrvFS can ignore chmod; assert root-directory metadata wherever chmod works.
if [ "$(stat -c '%a' "$snapshot")" = 701 ]; then
    [ "$(stat -c '%a' "$mounted_underlay")" = 701 ] ||
        fail "in-place migration did not preserve root-directory mode"
fi

mkdir -p "$BASE"
if main 2> "$TEST_ROOT/stale-state-error"; then
    fail "removal was allowed with unpresented OverlayFS state"
fi
grep -q "state exists without an active verified managed-home mount" \
    "$TEST_ROOT/stale-state-error" ||
    fail "stale OverlayFS state did not produce recovery guidance"
rmdir "$BASE"

printf 'ok - managed-home removal preserves the merged namespace and metadata\n'

if [ "${1:-}" = "--privileged-integration" ]; then
    [ "$(id -u)" -eq 0 ] ||
        fail "--privileged-integration must run as root"

    mkdir -p "$INTEGRATION_FS"
    mount -t tmpfs -o mode=0755,size=32m tmpfs "$INTEGRATION_FS"
    mkdir -p \
        "$INTEGRATION_FS/state/base/opaque" \
        "$INTEGRATION_FS/state/upper" \
        "$INTEGRATION_FS/state/work" \
        "$INTEGRATION_FS/home"
    printf 'original\n' > "$INTEGRATION_FS/state/base/changed"
    printf 'removed\n' > "$INTEGRATION_FS/state/base/deleted"
    printf 'lower child\n' > \
        "$INTEGRATION_FS/state/base/opaque/lower-child"
    cp -a "$INTEGRATION_FS/state/base/." "$INTEGRATION_FS/home/"

    mount -t overlay overlay \
        -o "lowerdir=$INTEGRATION_FS/state/base,upperdir=$INTEGRATION_FS/state/upper,workdir=$INTEGRATION_FS/state/work" \
        "$INTEGRATION_FS/home"
    printf 'visible change\n' > "$INTEGRATION_FS/home/changed"
    rm "$INTEGRATION_FS/home/deleted"
    rm -rf "$INTEGRATION_FS/home/opaque"
    mkdir "$INTEGRATION_FS/home/opaque"
    printf 'upper child\n' > "$INTEGRATION_FS/home/opaque/new"
    chmod 0710 "$INTEGRATION_FS/home"

    OVERLAY_DIR="$INTEGRATION_FS/state"
    BASE="$OVERLAY_DIR/base"
    PERSISTENT_UPPER="$OVERLAY_DIR/upper"
    PERSISTENT_WORK="$OVERLAY_DIR/work"
    RUNTIME_UPPER="$INTEGRATION_FS/run/upper"
    RUNTIME_WORK="$INTEGRATION_FS/run/work"
    TARGET_FILE="$OVERLAY_DIR/mount-target"
    SNAPSHOT="$OVERLAY_DIR/removal-snapshot"
    SNAPSHOT_COMPLETE_FILE="$OVERLAY_DIR/removal-snapshot.complete"
    BACKUP="$OVERLAY_DIR/removal-underlay"
    ACTIVE_UPPER="$PERSISTENT_UPPER"
    ACTIVE_WORK="$PERSISTENT_WORK"

    mkdir "$SNAPSHOT"
    printf 'stale snapshot\n' > "$SNAPSHOT/old"
    printf 'complete\n' > "$SNAPSHOT_COMPLETE_FILE"
    flatten_managed_overlay "$INTEGRATION_FS/home" ||
        fail "privileged OverlayFS flatten failed"
    ! mountpoint -q "$INTEGRATION_FS/home" ||
        fail "OverlayFS remained mounted after flattening"
    assert_file_contains "$INTEGRATION_FS/home/changed" "visible change"
    [ ! -e "$INTEGRATION_FS/home/deleted" ] ||
        fail "a real OverlayFS whiteout was not materialized"
    [ ! -e "$INTEGRATION_FS/home/opaque/lower-child" ] ||
        fail "a real opaque directory was not materialized"
    assert_file_contains "$INTEGRATION_FS/home/opaque/new" "upper child"
    [ "$(stat -c '%a' "$INTEGRATION_FS/home")" = 710 ] ||
        fail "OverlayFS root metadata was not preserved"
    [ ! -e "$OVERLAY_DIR" ] ||
        fail "successful migration left stale OverlayFS state"

    mkdir -p \
        "$INTEGRATION_FS/state/base/opaque" \
        "$INTEGRATION_FS/state/upper" \
        "$INTEGRATION_FS/state/work" \
        "$INTEGRATION_FS/mounted-home"
    mount -t tmpfs -o mode=0700,size=8m tmpfs \
        "$INTEGRATION_FS/mounted-home"
    printf 'original\n' > "$INTEGRATION_FS/state/base/changed"
    printf 'removed\n' > "$INTEGRATION_FS/state/base/deleted"
    printf 'lower child\n' > \
        "$INTEGRATION_FS/state/base/opaque/lower-child"
    cp -a "$INTEGRATION_FS/state/base/." \
        "$INTEGRATION_FS/mounted-home/"
    mount -t overlay overlay \
        -o "lowerdir=$INTEGRATION_FS/state/base,upperdir=$INTEGRATION_FS/state/upper,workdir=$INTEGRATION_FS/state/work" \
        "$INTEGRATION_FS/mounted-home"
    printf 'visible change\n' > "$INTEGRATION_FS/mounted-home/changed"
    rm "$INTEGRATION_FS/mounted-home/deleted"
    rm -rf "$INTEGRATION_FS/mounted-home/opaque"
    mkdir "$INTEGRATION_FS/mounted-home/opaque"
    printf 'upper child\n' > "$INTEGRATION_FS/mounted-home/opaque/new"
    chmod 0711 "$INTEGRATION_FS/mounted-home"

    OVERLAY_DIR="$INTEGRATION_FS/state"
    BASE="$OVERLAY_DIR/base"
    PERSISTENT_UPPER="$OVERLAY_DIR/upper"
    PERSISTENT_WORK="$OVERLAY_DIR/work"
    TARGET_FILE="$OVERLAY_DIR/mount-target"
    SNAPSHOT="$OVERLAY_DIR/removal-snapshot"
    SNAPSHOT_COMPLETE_FILE="$OVERLAY_DIR/removal-snapshot.complete"
    BACKUP="$OVERLAY_DIR/removal-underlay"
    ACTIVE_UPPER="$PERSISTENT_UPPER"
    ACTIVE_WORK="$PERSISTENT_WORK"

    flatten_managed_overlay "$INTEGRATION_FS/mounted-home" ||
        fail "mounted-underlay OverlayFS flatten failed"
    mountpoint -q "$INTEGRATION_FS/mounted-home" ||
        fail "the underlying home filesystem was unmounted"
    [ "$(findmnt -rn -M "$INTEGRATION_FS/mounted-home" -o FSTYPE)" = tmpfs ] ||
        fail "OverlayFS remained above the underlying home filesystem"
    assert_file_contains \
        "$INTEGRATION_FS/mounted-home/changed" "visible change"
    [ ! -e "$INTEGRATION_FS/mounted-home/deleted" ] ||
        fail "in-place flatten did not materialize a real whiteout"
    [ ! -e "$INTEGRATION_FS/mounted-home/opaque/lower-child" ] ||
        fail "in-place flatten did not materialize an opaque directory"
    assert_file_contains \
        "$INTEGRATION_FS/mounted-home/opaque/new" "upper child"
    [ "$(stat -c '%a' "$INTEGRATION_FS/mounted-home")" = 711 ] ||
        fail "in-place flatten did not preserve OverlayFS root metadata"

    printf 'ok - privileged OverlayFS flatten integrations\n'
fi
