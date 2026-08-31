#!/bin/bash

set -euo pipefail

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PROJECT_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/../../.." && pwd)
HELPER="$PROJECT_DIR/packaging/deb/claw-os-agent/extension-identities.sh"
SCRATCH="$PROJECT_DIR/.test-extension-identities.$$"
trap 'rm -rf "$SCRATCH"' EXIT
mkdir -p "$SCRATCH/bin" "$SCRATCH/etc" "$SCRATCH/state"

export MOCK_ETC="$SCRATCH/etc"
export COS_IDENTITY_ETC_DIR="$SCRATCH/etc"
export COS_IDENTITY_STATE_DIR="$SCRATCH/state"
export PATH="$SCRATCH/bin:$PATH"

cat > "$SCRATCH/bin/id" <<'EOF'
#!/bin/sh
[ "${1-}" = -u ] && {
    echo 0
    exit 0
}
exec /usr/bin/id "$@"
EOF

cat > "$SCRATCH/bin/stat" <<'EOF'
#!/bin/sh
case "${2-}" in
    %u:%g:%a:%F)
        echo 0:0:700:directory
        ;;
    %u:%g:%a:%F:%h)
        echo 0:0:644:regular\ file:1
        ;;
    *)
        exec /usr/bin/stat "$@"
        ;;
esac
EOF

cat > "$SCRATCH/bin/install" <<'EOF'
#!/bin/sh
mode=
directory=0
while [ "$#" -gt 0 ]; do
    case "$1" in
        -d) directory=1; shift ;;
        -o|-g) shift 2 ;;
        -m) mode=$2; shift 2 ;;
        *) path=$1; shift ;;
    esac
done
[ "$directory" -eq 1 ] && [ -n "${path-}" ] || exit 2
/usr/bin/mkdir -p "$path"
[ -z "$mode" ] || /usr/bin/chmod "$mode" "$path"
EOF

cat > "$SCRATCH/bin/getent" <<'EOF'
#!/bin/sh
db=$1
key=${2-}
file=$MOCK_ETC/$db
[ -f "$file" ] || exit 2
if [ -z "$key" ]; then
    cat "$file"
    exit 0
fi
case "$db" in
    passwd) awk -F: -v key="$key" '$1 == key || $3 == key { print; found=1 } END { exit !found }' "$file" ;;
    group) awk -F: -v key="$key" '$1 == key || $3 == key { print; found=1 } END { exit !found }' "$file" ;;
    shadow) awk -F: -v key="$key" '$1 == key { print; found=1 } END { exit !found }' "$file" ;;
    *) exit 2 ;;
esac
EOF

cat > "$SCRATCH/bin/groupadd" <<'EOF'
#!/bin/sh
name=
gid=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --gid) gid=$2; shift 2 ;;
        --system) shift ;;
        *) name=$1; shift ;;
    esac
done
[ -n "$name" ] && [ -n "$gid" ] || exit 2
grep -q "^$name:" "$MOCK_ETC/group" 2>/dev/null && exit 9
grep -q "^[^:]*:[^:]*:$gid:" "$MOCK_ETC/group" 2>/dev/null && exit 4
echo "$name:x:$gid:" >> "$MOCK_ETC/group"
EOF

cat > "$SCRATCH/bin/useradd" <<'EOF'
#!/bin/sh
uid=
group=
home=
shell=
password=
comment=
name=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --uid) uid=$2; shift 2 ;;
        --gid) group=$2; shift 2 ;;
        --home-dir) home=$2; shift 2 ;;
        --shell) shell=$2; shift 2 ;;
        --password) password=$2; shift 2 ;;
        --comment) comment=$2; shift 2 ;;
        --system|--no-create-home) shift ;;
        *) name=$1; shift ;;
    esac
done
case "$group" in
    *[!0-9]*|'') gid=$(awk -F: -v name="$group" '$1 == name { print $3 }' "$MOCK_ETC/group") ;;
    *) gid=$group ;;
esac
[ -n "$uid" ] && [ -n "$gid" ] && [ -n "$name" ] || exit 2
create_account() {
    echo "$name:x:$uid:$gid:$comment:$home:$shell" >> "$MOCK_ETC/passwd"
    echo "$name:!:20000:0:99999:7:::" >> "$MOCK_ETC/shadow"
}
if [ "${MOCK_FAIL_USER-}" = "$name" ]; then
    [ "${MOCK_PARTIAL_USERADD-0}" = 1 ] && create_account
    exit 12
fi
create_account
EOF

cat > "$SCRATCH/bin/userdel" <<'EOF'
#!/bin/sh
name=$1
awk -F: -v name="$name" '$1 != name' "$MOCK_ETC/passwd" > "$MOCK_ETC/passwd.new"
mv "$MOCK_ETC/passwd.new" "$MOCK_ETC/passwd"
awk -F: -v name="$name" '$1 != name' "$MOCK_ETC/shadow" > "$MOCK_ETC/shadow.new"
mv "$MOCK_ETC/shadow.new" "$MOCK_ETC/shadow"
EOF

cat > "$SCRATCH/bin/groupdel" <<'EOF'
#!/bin/sh
name=$1
awk -F: -v name="$name" '$1 != name' "$MOCK_ETC/group" > "$MOCK_ETC/group.new"
mv "$MOCK_ETC/group.new" "$MOCK_ETC/group"
EOF

cat > "$SCRATCH/bin/homectl" <<'EOF'
#!/bin/sh
[ "${MOCK_HOMED_IDENTITY-}" = "${2-}" ] && exit 0
exit 1
EOF

chmod +x "$SCRATCH/bin/"*

sed -e 's/^COS_EXT_UID_COUNT=64$/COS_EXT_UID_COUNT=4/' \
    -e 's/\[ "$COS_EXT_UID_COUNT" -eq 64 \]/[ "$COS_EXT_UID_COUNT" -eq 4 ]/' \
    "$HELPER" > "$SCRATCH/extension-identities.sh"
# shellcheck source=/dev/null
. "$SCRATCH/extension-identities.sh"

fail() {
    echo "not ok - $*" >&2
    exit 1
}

reset_fixture() {
    rm -rf "$SCRATCH/etc" "$SCRATCH/state"
    mkdir -p "$SCRATCH/etc" "$SCRATCH/state"
    : > "$SCRATCH/etc/passwd"
    : > "$SCRATCH/etc/group"
    : > "$SCRATCH/etc/shadow"
    : > "$SCRATCH/etc/subuid"
    : > "$SCRATCH/etc/subgid"
    unset MOCK_FAIL_USER MOCK_PARTIAL_USERADD MOCK_HOMED_IDENTITY
}

populate_correct_accounts() {
    echo "cos-extension:x:60999:" > "$SCRATCH/etc/group"
    for index in $(seq 0 3); do
        name=$(printf 'cos-ext-%02d' "$index")
        uid=$((61000 + index))
        echo "$name:x:$uid:60999:Claw OS extension slot $index:/nonexistent:/usr/sbin/nologin" \
            >> "$SCRATCH/etc/passwd"
        echo "$name:!:20000:0:99999:7:::" >> "$SCRATCH/etc/shadow"
    done
}

reset_fixture
identity_provision || fail "fresh provisioning failed"
[ "$(wc -l < "$SCRATCH/etc/passwd")" -eq 4 ] || fail "fresh install did not create test users"
identity_finalize || fail "fresh finalize failed"
[ "$(wc -l < "$identity_reserved_manifest")" -eq 6 ] || fail "reservation manifest incomplete"
grep -q '^user:cos-ext-00:61000:60999$' "$identity_owned_manifest" ||
    fail "package ownership marker missing first user"
identity_provision || fail "safe upgrade of package-owned users failed"
identity_finalize || fail "safe upgrade finalize failed"
[ "$(wc -l < "$SCRATCH/etc/passwd")" -eq 4 ] || fail "upgrade duplicated users"

reset_fixture
populate_correct_accounts
identity_provision || fail "preexisting correct identities were rejected"
identity_finalize || fail "preexisting identities did not finalize"
[ ! -s "$identity_owned_manifest" ] || fail "preexisting accounts were claimed as package-owned"

reset_fixture
echo "cos-extension:x:60999:" > "$SCRATCH/etc/group"
echo "unrelated:x:61000:60999:Unrelated:/home/unrelated:/bin/sh" > "$SCRATCH/etc/passwd"
echo "unrelated:!:20000:0:99999:7:::" > "$SCRATCH/etc/shadow"
if identity_provision; then
    fail "uid collision was accepted"
fi
grep -q '^unrelated:' "$SCRATCH/etc/passwd" || fail "collision account was mutated"
[ "$(wc -l < "$SCRATCH/etc/passwd")" -eq 1 ] || fail "collision created partial accounts"

reset_fixture
echo "alice:60990:20" > "$SCRATCH/etc/subuid"
identity_provision && fail "overlapping subuid range was accepted"
[ ! -s "$SCRATCH/etc/group" ] || fail "subuid failure provisioned a group"

reset_fixture
echo "alice:61003:1" > "$SCRATCH/etc/subgid"
identity_provision && fail "overlapping subgid boundary was accepted"

reset_fixture
echo "alice:4294967295:2" > "$SCRATCH/etc/subuid"
identity_provision && fail "overflowing subuid range was accepted"

reset_fixture
echo "alice:61064:1" > "$SCRATCH/etc/subuid"
identity_provision || fail "adjacent upper subuid boundary was rejected"
identity_rollback_pending

reset_fixture
echo "alice:60999:1" > "$SCRATCH/etc/subgid"
identity_provision && fail "extension group gid in subgid was accepted"

reset_fixture
echo "alice:60998:1" > "$SCRATCH/etc/subgid"
identity_provision || fail "adjacent lower subgid boundary was rejected"
identity_rollback_pending

reset_fixture
echo "alice:61000:0" > "$SCRATCH/etc/subuid"
identity_provision && fail "zero-length subuid range was accepted"

reset_fixture
echo "alice:not-a-number:1" > "$SCRATCH/etc/subgid"
identity_provision && fail "malformed subgid range was accepted"

reset_fixture
echo "cos-extension:x:61010:" > "$SCRATCH/etc/group"
identity_provision && fail "wrong fixed extension gid was accepted"

reset_fixture
export MOCK_HOMED_IDENTITY=cos-ext-00
identity_provision && fail "systemd-homed identity collision was accepted"

reset_fixture
export MOCK_FAIL_USER=cos-ext-03
export MOCK_PARTIAL_USERADD=1
identity_provision && fail "partial useradd failure was accepted"
[ "$(wc -l < "$SCRATCH/etc/passwd")" -eq 1 ] ||
    fail "partial provisioning did not roll back proven package-owned users"
grep -q '^cos-ext-03:x:61003:60999:' "$SCRATCH/etc/passwd" ||
    fail "ambiguous partial useradd record was deleted"
grep -q '^cos-extension:x:60999:$' "$SCRATCH/etc/group" ||
    fail "group referenced by an ambiguous partial account was deleted"
unset MOCK_FAIL_USER MOCK_PARTIAL_USERADD
identity_provision || fail "retry after partial useradd did not converge"
identity_finalize || fail "retry after partial useradd did not finalize"

reset_fixture
identity_provision || fail "rollback fixture provisioning failed"
identity_rollback_pending
[ ! -s "$SCRATCH/etc/passwd" ] || fail "abort rollback retained package-created users"
[ ! -s "$SCRATCH/etc/group" ] || fail "abort rollback retained package-created group"

reset_fixture
identity_provision || fail "purge fixture provisioning failed"
identity_finalize || fail "purge fixture finalize failed"
sed -i 's#cos-ext-00:x:61000:60999:[^:]*:/nonexistent:/usr/sbin/nologin#cos-ext-00:x:61000:60999:changed:/nonexistent:/bin/sh#' \
    "$SCRATCH/etc/passwd"
identity_purge_owned
grep -q '^cos-ext-00:' "$SCRATCH/etc/passwd" ||
    fail "purge deleted an identity whose record no longer matched"
! grep -q '^cos-ext-01:' "$SCRATCH/etc/passwd" ||
    fail "purge retained a provably package-owned identity"
grep -q '^cos-extension:' "$SCRATCH/etc/group" ||
    fail "purge removed a group still referenced by a retained account"

reset_fixture
identity_provision || fail "quarantine purge fixture provisioning failed"
identity_finalize || fail "quarantine purge fixture finalize failed"
mkdir -p "$identity_quarantine_dir"
: > "$identity_quarantine_dir/61000.state"
identity_purge_owned
grep -q '^cos-ext-00:' "$SCRATCH/etc/passwd" ||
    fail "purge deleted an identity with an active cleanup quarantine"

[ $((COS_EXT_UID_FIRST + COS_EXT_UID_COUNT - 1)) -lt "$COS_EXT_DYNAMIC_UID_FIRST" ] ||
    fail "identity range overlaps systemd DynamicUser"
[ "$COS_EXT_GID" -lt "$COS_EXT_DYNAMIC_UID_FIRST" ] ||
    fail "extension gid overlaps systemd DynamicUser"

echo "ok - extension identity provisioning"
