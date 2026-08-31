#!/bin/sh

COS_EXT_GROUP=cos-extension
COS_EXT_GID=60999
COS_EXT_UID_FIRST=61000
COS_EXT_UID_COUNT=64
COS_EXT_DYNAMIC_UID_FIRST=61184
COS_EXT_DYNAMIC_UID_LAST=65519
COS_EXT_HOME=/nonexistent
COS_EXT_SHELL=/usr/sbin/nologin

identity_etc_dir=${COS_IDENTITY_ETC_DIR:-/etc}
identity_state_dir=${COS_IDENTITY_STATE_DIR:-/var/lib/cos}
identity_reserved_manifest=$identity_state_dir/extension-identities.reserved
identity_owned_manifest=$identity_state_dir/extension-identities.owned
identity_pending_manifest=$identity_state_dir/extension-identities.pending
identity_quarantine_dir=$identity_state_dir/extension-quarantine

identity_name() {
    printf 'cos-ext-%02d' "$1"
}

identity_fail() {
    echo "claw-os-agent: $*" >&2
    return 1
}

identity_prepare_state_dir() {
    [ "$(id -u)" -eq 0 ] || {
        identity_fail "extension identity provisioning requires root"
        return 1
    }
    [ ! -L "$identity_state_dir" ] || {
        identity_fail "$identity_state_dir must not be a symlink"
        return 1
    }
    install -d -o 0 -g 0 -m 0700 "$identity_state_dir" || return 1
    metadata=$(stat -c '%u:%g:%a:%F' "$identity_state_dir") || return 1
    [ "$metadata" = "0:0:700:directory" ] || {
        identity_fail "$identity_state_dir is not a root-owned mode-0700 directory"
        return 1
    }
}

identity_group_line() {
    getent group "$COS_EXT_GROUP" 2>/dev/null || true
}

identity_group_validate() {
    line=$(identity_group_line)
    [ -n "$line" ] || {
        identity_fail "group $COS_EXT_GROUP is missing"
        return 1
    }
    IFS=: read -r record_name record_password record_gid record_members <<EOF
$line
EOF
    [ "$record_name" = "$COS_EXT_GROUP" ] || {
        identity_fail "group name mismatch for $COS_EXT_GROUP"
        return 1
    }
    [ "$record_password" = x ] || {
        identity_fail "group $COS_EXT_GROUP has an unexpected password field"
        return 1
    }
    [ "$record_gid" = "$COS_EXT_GID" ] || {
        identity_fail "group $COS_EXT_GROUP has gid $record_gid, expected $COS_EXT_GID"
        return 1
    }
    [ -z "$record_members" ] || {
        identity_fail "group $COS_EXT_GROUP has supplementary members"
        return 1
    }
    reverse=$(getent group "$COS_EXT_GID" 2>/dev/null || true)
    [ "$reverse" = "$line" ] || {
        identity_fail "gid $COS_EXT_GID does not resolve uniquely to $COS_EXT_GROUP"
        return 1
    }
    printf '%s\n' "$COS_EXT_GID"
}

identity_shadow_locked() {
    name=$1
    line=$(getent shadow "$name" 2>/dev/null || true)
    [ -n "$line" ] || {
        identity_fail "account $name has no shadow record"
        return 1
    }
    old_ifs=$IFS
    IFS=:
    set -- $line
    IFS=$old_ifs
    [ "$1" = "$name" ] || {
        identity_fail "shadow name mismatch for $name"
        return 1
    }
    password=$2
    shadow_field=$2
    first=${shadow_field%"${shadow_field#?}"}
    [ "$first" = "!" ] || [ "$first" = "*" ] || {
        identity_fail "account $name is not password-locked"
        return 1
    }
    [ -z "$(printf '%s' "$shadow_field" | tr -d '!*')" ] || {
        identity_fail "account $name retains password hash material"
        return 1
    }
}

identity_account_validate() {
    name=$1
    uid=$2
    index=$((uid - COS_EXT_UID_FIRST))
    line=$(getent passwd "$name" 2>/dev/null || true)
    [ -n "$line" ] || {
        identity_fail "account $name is missing"
        return 1
    }
    IFS=: read -r account_name account_password account_uid account_gid \
        account_gecos account_home account_shell <<EOF
$line
EOF
    [ "$account_name" = "$name" ] || {
        identity_fail "account name mismatch for $name"
        return 1
    }
    [ "$account_password" = x ] || {
        identity_fail "account $name has an unexpected passwd field"
        return 1
    }
    [ "$account_uid" = "$uid" ] || {
        identity_fail "account $name has uid $account_uid, expected $uid"
        return 1
    }
    [ "$account_gid" = "$COS_EXT_GID" ] || {
        identity_fail "account $name has gid $account_gid, expected $COS_EXT_GID"
        return 1
    }
    [ "$account_gecos" = "Claw OS extension slot $index" ] || {
        identity_fail "account $name has an unexpected comment field"
        return 1
    }
    [ "$account_home" = "$COS_EXT_HOME" ] || {
        identity_fail "account $name has home $account_home, expected $COS_EXT_HOME"
        return 1
    }
    [ "$account_shell" = "$COS_EXT_SHELL" ] || {
        identity_fail "account $name has shell $account_shell, expected $COS_EXT_SHELL"
        return 1
    }
    reverse=$(getent passwd "$uid" 2>/dev/null || true)
    [ "$reverse" = "$line" ] || {
        identity_fail "uid $uid does not resolve uniquely to $name"
        return 1
    }
    identity_shadow_locked "$name" || return 1
    if command -v homectl >/dev/null 2>&1; then
        if homectl inspect "$name" >/dev/null 2>&1 ||
            homectl inspect "$uid" >/dev/null 2>&1; then
            identity_fail "account $name is managed by systemd-homed"
            return 1
        fi
    fi
}

identity_range_validate() {
    last=$((COS_EXT_UID_FIRST + COS_EXT_UID_COUNT - 1))
    [ "$COS_EXT_UID_COUNT" -eq 64 ] || {
        identity_fail "extension identity count must be 64"
        return 1
    }
    [ "$COS_EXT_UID_FIRST" -gt 60000 ] || {
        identity_fail "extension identity range overlaps normal login allocation"
        return 1
    }
    [ "$last" -lt "$COS_EXT_DYNAMIC_UID_FIRST" ] || {
        identity_fail "extension identity range overlaps systemd DynamicUser"
        return 1
    }
    [ "$COS_EXT_GID" -gt 60000 ] &&
        [ "$COS_EXT_GID" -lt "$COS_EXT_DYNAMIC_UID_FIRST" ] || {
        identity_fail "extension execution gid overlaps login or DynamicUser space"
        return 1
    }
}

identity_regular_root_file() {
    file=$1
    [ -e "$file" ] || return 0
    [ ! -L "$file" ] || {
        identity_fail "$file must not be a symlink"
        return 1
    }
    metadata=$(stat -c '%u:%g:%a:%F:%h' "$file") || return 1
    old_ifs=$IFS
    IFS=:
    set -- $metadata
    IFS=$old_ifs
    [ "$1" = 0 ] && [ "$2" = 0 ] && [ "$4" = "regular file" ] &&
        [ "$5" = 1 ] || {
        identity_fail "$file must be a single-link root-owned regular file"
        return 1
    }
    mode=$((0$3))
    [ $((mode & 022)) -eq 0 ] || {
        identity_fail "$file must not be group- or world-writable"
        return 1
    }
}

identity_subid_validate_file() {
    file=$1
    lo=$2
    [ -e "$file" ] || return 0
    identity_regular_root_file "$file" || return 1
    if awk -F: -v lo="$lo" \
        -v hi="$((COS_EXT_UID_FIRST + COS_EXT_UID_COUNT - 1))" '
        /^[[:space:]]*(#|$)/ { next }
        NF != 3 || $1 == "" || $2 !~ /^[0-9]+$/ ||
            $3 !~ /^[0-9]+$/ || $3 == 0 ||
            $2 > 4294967295 || $3 > 4294967295 ||
            $3 > 4294967295 - $2 + 1 {
            exit 2
        }
        {
            last = $2 + $3 - 1
            if (last < $2) exit 2
            if ($2 <= hi && last >= lo) exit 1
        }
    ' "$file"; then
        status=0
    else
        status=$?
    fi
    case "$status" in
        0) return 0 ;;
        1) identity_fail "$file overlaps package-reserved extension identities" ;;
        *) identity_fail "$file contains an invalid subordinate-id record" ;;
    esac
}

identity_subids_validate() {
    identity_subid_validate_file "$identity_etc_dir/subuid" "$COS_EXT_UID_FIRST" &&
        identity_subid_validate_file "$identity_etc_dir/subgid" "$COS_EXT_GID"
}

identity_preflight() {
    identity_range_validate || return 1
    identity_subids_validate || return 1
    group_by_name=$(identity_group_line)
    group_by_gid=$(getent group "$COS_EXT_GID" 2>/dev/null || true)
    if [ -n "$group_by_name" ] || [ -n "$group_by_gid" ]; then
        [ -n "$group_by_name" ] && [ "$group_by_name" = "$group_by_gid" ] || {
            identity_fail "group name/gid collision for $COS_EXT_GROUP/$COS_EXT_GID"
            return 1
        }
        identity_group_validate >/dev/null || return 1
    fi
    index=0
    while [ "$index" -lt "$COS_EXT_UID_COUNT" ]; do
        name=$(identity_name "$index")
        uid=$((COS_EXT_UID_FIRST + index))
        by_name=$(getent passwd "$name" 2>/dev/null || true)
        by_uid=$(getent passwd "$uid" 2>/dev/null || true)
        if [ -n "$by_name" ] || [ -n "$by_uid" ]; then
            [ -n "$by_name" ] && [ "$by_name" = "$by_uid" ] || {
                identity_fail "account name/uid collision for $name/$uid"
                return 1
            }
            [ -n "$group_by_name" ] || {
                identity_fail "account collision exists before group provisioning"
                return 1
            }
            identity_account_validate "$name" "$uid" "$index" || return 1
        elif command -v homectl >/dev/null 2>&1 &&
            { homectl inspect "$name" >/dev/null 2>&1 ||
              homectl inspect "$uid" >/dev/null 2>&1; }; then
            identity_fail "systemd-homed owns reserved identity $name/$uid" || return 1
        fi
        index=$((index + 1))
    done
}

identity_rollback_file() {
    file=$1
    [ -f "$file" ] || return 0
    remaining=$file.remaining.$$
    : > "$remaining" || return 1
    failed=0
    while IFS=: read -r kind name value gid; do
        [ "$kind" = user ] || continue
        index=${name#cos-ext-}
        if identity_account_validate "$name" "$value" "$index" >/dev/null 2>&1; then
            if ! userdel "$name" >/dev/null 2>&1 ||
                getent passwd "$name" >/dev/null 2>&1 ||
                getent passwd "$value" >/dev/null 2>&1; then
                printf '%s:%s:%s:%s\n' "$kind" "$name" "$value" "$gid" >> "$remaining"
                failed=1
            fi
        else
            printf '%s:%s:%s:%s\n' "$kind" "$name" "$value" "$gid" >> "$remaining"
            failed=1
        fi
    done < "$file"
    while IFS=: read -r kind name value; do
        [ "$kind" = group ] || continue
        line=$(getent group "$name" 2>/dev/null || true)
        users=$(getent passwd 2>/dev/null |
            awk -F: -v gid="$value" '$4 == gid { print $1 }')
        if [ "$line" = "$name:x:$value:" ] && [ -z "$users" ]; then
            if ! groupdel "$name" >/dev/null 2>&1 ||
                getent group "$name" >/dev/null 2>&1 ||
                getent group "$value" >/dev/null 2>&1; then
                printf '%s:%s:%s\n' "$kind" "$name" "$value" >> "$remaining"
                failed=1
            fi
        elif [ -n "$line" ]; then
            printf '%s:%s:%s\n' "$kind" "$name" "$value" >> "$remaining"
            failed=1
        fi
    done < "$file"
    if [ -s "$remaining" ]; then
        chmod 0600 "$remaining"
        mv -f "$remaining" "$file"
    else
        rm -f "$remaining" "$file"
    fi
    [ "$failed" -eq 0 ]
}

identity_recover_partial() {
    for file in "$identity_state_dir"/.extension-identities.provision.*; do
        [ -e "$file" ] || continue
        if ! identity_rollback_file "$file"; then
            identity_merge_pending "$file" || return 1
            rm -f "$file"
        fi
    done
}

identity_merge_pending() {
    source_file=$1
    pending_new=$identity_pending_manifest.new.$$
    {
        [ ! -f "$identity_pending_manifest" ] || cat "$identity_pending_manifest"
        cat "$source_file"
    } | sort -u > "$pending_new" || {
        rm -f "$pending_new"
        return 1
    }
    chmod 0600 "$pending_new" || {
        rm -f "$pending_new"
        return 1
    }
    mv -f "$pending_new" "$identity_pending_manifest"
}

identity_provision() {
    if [ -d "$identity_state_dir" ]; then
        identity_prepare_state_dir || return 1
        identity_recover_partial || return 1
    fi
    identity_preflight || return 1
    identity_prepare_state_dir || return 1
    current=$identity_state_dir/.extension-identities.provision.$$
    : > "$current" || return 1
    chmod 0600 "$current" || return 1

    group_line=$(identity_group_line)
    if [ -z "$group_line" ]; then
        if ! groupadd --system --gid "$COS_EXT_GID" "$COS_EXT_GROUP"; then
            identity_rollback_file "$current"
            return 1
        fi
        printf 'group:%s:%s\n' "$COS_EXT_GROUP" "$COS_EXT_GID" >> "$current"
        identity_group_validate >/dev/null || {
            identity_rollback_file "$current"
            return 1
        }
    else
        identity_group_validate >/dev/null || {
            identity_rollback_file "$current"
            return 1
        }
    fi

    index=0
    while [ "$index" -lt "$COS_EXT_UID_COUNT" ]; do
        name=$(identity_name "$index")
        uid=$((COS_EXT_UID_FIRST + index))
        if ! getent passwd "$name" >/dev/null 2>&1; then
            if ! useradd --system --uid "$uid" --gid "$COS_EXT_GID" \
                --home-dir "$COS_EXT_HOME" --no-create-home \
                --password '!' \
                --shell "$COS_EXT_SHELL" --comment "Claw OS extension slot $index" \
                "$name"; then
                identity_rollback_file "$current"
                return 1
            fi
            printf 'user:%s:%s:%s\n' "$name" "$uid" "$COS_EXT_GID" >> "$current"
        fi
        if ! identity_account_validate "$name" "$uid" "$index"; then
            identity_rollback_file "$current"
            return 1
        fi
        index=$((index + 1))
    done
    identity_subids_validate || {
        identity_rollback_file "$current"
        return 1
    }

    identity_merge_pending "$current" || {
        identity_rollback_file "$current"
        return 1
    }
    rm -f "$current"
}

identity_validate_all() {
    identity_range_validate || return 1
    identity_subids_validate || return 1
    identity_group_validate >/dev/null || return 1
    index=0
    while [ "$index" -lt "$COS_EXT_UID_COUNT" ]; do
        identity_account_validate "$(identity_name "$index")" \
            "$((COS_EXT_UID_FIRST + index))" "$index" || return 1
        index=$((index + 1))
    done
}

identity_finalize() {
    identity_prepare_state_dir || return 1
    identity_validate_all || return 1
    identity_group_validate >/dev/null || return 1
    manifest_new=$identity_reserved_manifest.new.$$
    {
        echo "version=1"
        echo "group=$COS_EXT_GROUP:$COS_EXT_GID"
        index=0
        while [ "$index" -lt "$COS_EXT_UID_COUNT" ]; do
            name=$(identity_name "$index")
            uid=$((COS_EXT_UID_FIRST + index))
            echo "identity=$name:$uid:$COS_EXT_GID:$COS_EXT_HOME:$COS_EXT_SHELL"
            index=$((index + 1))
        done
    } > "$manifest_new" || return 1
    chmod 0600 "$manifest_new"
    mv -f "$manifest_new" "$identity_reserved_manifest"

    if [ -f "$identity_pending_manifest" ]; then
        owned_new=$identity_owned_manifest.new.$$
        {
            [ ! -f "$identity_owned_manifest" ] || cat "$identity_owned_manifest"
            cat "$identity_pending_manifest"
        } | sort -u > "$owned_new"
        chmod 0600 "$owned_new"
        mv -f "$owned_new" "$identity_owned_manifest"
        rm -f "$identity_pending_manifest"
    fi
}

identity_rollback_pending() {
    [ -e "$identity_state_dir" ] || return 0
    identity_prepare_state_dir || return 1
    identity_rollback_file "$identity_pending_manifest"
}

identity_purge_owned() {
    [ -e "$identity_state_dir" ] || return 0
    identity_prepare_state_dir || return 1
    rm -f "$identity_reserved_manifest"
    [ -f "$identity_owned_manifest" ] || return 0
    remaining=$identity_owned_manifest.remaining.$$
    : > "$remaining"
    while IFS=: read -r kind name value gid; do
        [ "$kind" = user ] || continue
        index=${name#cos-ext-}
        if [ -e "$identity_quarantine_dir/$value.state" ] ||
            [ -e "/run/user/$value" ]; then
            printf '%s:%s:%s:%s\n' "$kind" "$name" "$value" "$gid" >> "$remaining"
        elif identity_group_validate >/dev/null 2>&1 &&
            identity_account_validate "$name" "$value" "$index" >/dev/null 2>&1; then
            if ! userdel "$name" >/dev/null 2>&1 ||
                getent passwd "$name" >/dev/null 2>&1 ||
                getent passwd "$value" >/dev/null 2>&1; then
                printf '%s:%s:%s:%s\n' "$kind" "$name" "$value" "$gid" >> "$remaining"
            fi
        else
            printf '%s:%s:%s:%s\n' "$kind" "$name" "$value" "$gid" >> "$remaining"
        fi
    done < "$identity_owned_manifest"
    while IFS=: read -r kind name value; do
        case "$kind" in
            group)
                users=$(getent passwd | awk -F: -v gid="$value" '$4 == gid { print $1 }')
                line=$(getent group "$name" 2>/dev/null || true)
                if [ "$line" = "$name:x:$value:" ] && [ -z "$users" ]; then
                    if ! groupdel "$name" >/dev/null 2>&1 ||
                        getent group "$name" >/dev/null 2>&1 ||
                        getent group "$value" >/dev/null 2>&1; then
                        printf '%s:%s:%s\n' "$kind" "$name" "$value" >> "$remaining"
                    fi
                else
                    printf '%s:%s:%s\n' "$kind" "$name" "$value" >> "$remaining"
                fi
                ;;
        esac
    done < "$identity_owned_manifest"
    if [ -s "$remaining" ]; then
        chmod 0600 "$remaining"
        mv -f "$remaining" "$identity_owned_manifest"
        echo "claw-os-agent: retained identities whose package ownership could not be proven" >&2
    else
        rm -f "$remaining" "$identity_owned_manifest"
    fi
}
