#!/bin/bash
# Provision the container's human account, then start systemd or a login shell.

set -Eeuo pipefail

username="${CLAW_USER:-}"
uid="${CLAW_UID:-1000}"
gid="${CLAW_GID:-1000}"
created_user=""
created_group=""

fail() {
    echo "error: $*" >&2
    exit 1
}

rollback() {
    if [ -n "$created_user" ] && id "$created_user" >/dev/null 2>&1; then
        userdel --remove "$created_user" >/dev/null 2>&1 || true
    fi
    if [ -n "$created_group" ] && getent group "$created_group" >/dev/null; then
        groupdel "$created_group" >/dev/null 2>&1 || true
    fi
}
trap rollback ERR
trap 'rollback; exit 130' INT
trap 'rollback; exit 143' TERM

[ "$(id -u)" -eq 0 ] || fail "entrypoint must run as root"

[[ "$username" =~ ^[a-z][a-z0-9_-]{0,30}$ ]] \
    || fail "CLAW_USER must match ^[a-z][a-z0-9_-]{0,30}$"
[[ "$uid" =~ ^[0-9]+$ ]] && (( uid >= 100 && uid < 60000 )) \
    || fail "CLAW_UID must be an integer from 100 through 59999"
[[ "$gid" =~ ^[0-9]+$ ]] && (( gid >= 100 && gid < 60000 )) \
    || fail "CLAW_GID must be an integer from 100 through 59999"

existing_user="$(getent passwd "$username" || true)"
existing_uid="$(getent passwd "$uid" || true)"
existing_group="$(getent group "$username" || true)"
existing_gid="$(getent group "$gid" || true)"

if [ -n "$existing_user" ]; then
    IFS=: read -r _ _ actual_uid actual_gid _ home _ <<< "$existing_user"
    [ "$actual_uid" = "$uid" ] \
        || fail "user '$username' already exists with UID $actual_uid, expected $uid"
    [ "$actual_gid" = "$gid" ] \
        || fail "user '$username' already exists with GID $actual_gid, expected $gid"
    [ "$home" = "/home/$username" ] \
        || fail "user '$username' has unexpected home '$home'"
else
    [ -z "$existing_uid" ] || fail "UID $uid is already assigned"
    [ -z "$existing_group" ] || fail "group '$username' is already assigned"
    [ -z "$existing_gid" ] || fail "GID $gid is already assigned"

    groupadd --gid "$gid" "$username"
    created_group="$username"
    if ! useradd \
        --uid "$uid" \
        --gid "$gid" \
        --create-home \
        --shell /bin/bash \
        "$username"; then
        groupdel "$username" >/dev/null 2>&1 || true
        exit 1
    fi
    created_user="$username"
    passwd --lock "$username" >/dev/null
fi
id -nG "$username" | tr ' ' '\n' | grep -Fxq sudo \
    || usermod --append --groups sudo "$username"

mkdir -p /workspace
if ! mountpoint -q /workspace; then
    chmod 0755 /workspace
    chown "$uid:$gid" /workspace
fi

home="/home/$username"
if [ -e "$home/workspace" ] && [ ! -L "$home/workspace" ]; then
    fail "$home/workspace exists and is not the managed /workspace symlink"
fi
ln -sfn /workspace "$home/workspace"
chown -h "$uid:$gid" "$home/workspace"

printf 'COS_HOME=%s\n' "$home" > /etc/default/cos-home

created_user=""
created_group=""
trap - ERR INT TERM

if [ "${1:-}" = "shell" ]; then
    shift
    if [ "$#" -eq 0 ]; then
        set -- /bin/bash --login
    fi
    exec runuser --user "$username" -- \
        env HOME="$home" USER="$username" LOGNAME="$username" SHELL=/bin/bash "${@}"
fi

if [ "$#" -eq 0 ]; then
    set -- /sbin/init
fi
exec "$@"
