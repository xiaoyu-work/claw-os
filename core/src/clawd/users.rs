use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::caps::{Cap, CapSet, Scope, Verb};
use crate::proc::SessionInfo;

use super::client_identity::ClientIdentity;

const TOOL_TIMEOUT: Duration = Duration::from_secs(120);
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const STREAM_CAP_BYTES: usize = 1024 * 1024;
const MIN_MANAGED_ID: u32 = 1000;
const MAX_MANAGED_ID: u32 = 60000;
static USER_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct UserRecord {
    name: String,
    uid: u32,
    gid: u32,
    gecos: String,
    home: String,
    shell: String,
    password: String,
    last_change: i64,
    minimum: i64,
    maximum: i64,
    warning: i64,
    inactive: i64,
    expire: i64,
    supplementary_groups: Vec<String>,
    primary_group: Option<GroupRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct GroupRecord {
    name: String,
    gid: u32,
    locked: bool,
    members: Vec<String>,
    admins: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
enum IdentitySnapshot {
    User(Option<UserRecord>),
    Group(Option<GroupRecord>),
}

#[derive(Clone, Serialize, Deserialize)]
struct IdentityBackup {
    token: String,
    owner_uid: u32,
    created_at: String,
    action: String,
    target_kind: String,
    target_name: String,
    before: IdentitySnapshot,
    applied: IdentitySnapshot,
    applied_fingerprint: String,
    status: String,
}

pub async fn control(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client);
        return Err("User Manager requires Linux shadow utilities".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err("User Manager requires root clawd".to_string());
        }
        let uid = client.require_uid()?;
        let home = client.require_home_dir()?;
        let peer_pid = client
            .pid
            .ok_or_else(|| "clawd peer pid is unavailable".to_string())?;
        let session_id = required_string(&params, "session")?;
        let action = required_string(&params, "action")?;
        let user = optional_string(&params, "user")?;
        let group = optional_string(&params, "group")?;
        let full_name = optional_string(&params, "full_name")?;
        let shell = optional_string(&params, "shell")?;
        let groups = optional_string(&params, "groups")?;
        let credential = optional_string(&params, "credential")?;
        let token = optional_string(&params, "token")?;
        let confirm = params
            .get("confirm")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        validate_action(
            &action,
            user.as_deref(),
            group.as_deref(),
            full_name.as_deref(),
            shell.as_deref(),
            groups.as_deref(),
            credential.as_deref(),
            token.as_deref(),
            confirm,
        )?;
        if let Some(user) = user.as_deref() {
            validate_account_name("user", user)?;
        }
        if let Some(group) = group.as_deref() {
            validate_account_name("group", group)?;
        }
        if let Some(groups) = groups.as_deref() {
            parse_groups(groups)?;
        }
        if let Some(shell) = shell.as_deref() {
            validate_shell(shell)?;
        }
        if let Some(full_name) = full_name.as_deref() {
            validate_full_name(full_name)?;
        }
        let credential_ref = credential
            .as_deref()
            .map(parse_credential_ref)
            .transpose()?;
        let requested = requested_caps(credential_ref.as_ref());
        let session = crate::paths::with_user_override(uid, home.clone(), async {
            authorize_session(&session_id, peer_pid, &requested)
        })
        .await?;

        if action == "status" {
            return identity_status();
        }
        let password = match credential_ref.as_ref() {
            Some((namespace, name)) => Some(
                crate::paths::with_user_override(uid, home, async {
                    crate::credential::load_for_broker(
                        name,
                        namespace,
                        session.tier.unwrap_or(u8::MAX),
                    )
                })
                .await?,
            ),
            None => None,
        };
        if let Some(password) = password.as_deref() {
            validate_password(password)?;
        }

        let _guard = tokio::time::timeout(
            LOCK_TIMEOUT,
            USER_LOCK.get_or_init(|| tokio::sync::Mutex::new(())).lock(),
        )
        .await
        .map_err(|_| "User Manager is busy with another identity mutation".to_string())?;
        mutate(
            &action,
            user.as_deref(),
            group.as_deref(),
            full_name.as_deref(),
            shell.as_deref(),
            groups.as_deref(),
            password.as_deref(),
            token.as_deref(),
            uid,
        )
        .await
    }
}

fn requested_caps(credential: Option<&(String, String)>) -> Vec<Cap> {
    let mut caps = vec![Cap::new(Verb::SYS_IDENTITY, Scope::name("manage"))];
    if let Some((namespace, name)) = credential {
        caps.push(Cap::new(
            Verb::SECRET_READ,
            Scope::name(format!("{namespace}/{name}")),
        ));
    }
    caps
}

fn authorize_session(
    session_id: &str,
    peer_pid: u32,
    requested: &[Cap],
) -> Result<SessionInfo, String> {
    let session = crate::proc::session_info_by_id(session_id)
        .ok_or_else(|| format!("user-manager session not found: {session_id}"))?;
    if session.app_id.as_deref() != Some("user-manager") {
        return Err("identity changes are restricted to the user-manager App".to_string());
    }
    if session.pending_bind || session.pid == 0 {
        return Err("user-manager session is not bound to a process".to_string());
    }
    let expected_start = session
        .start_time_ticks
        .ok_or_else(|| "user-manager session has no process identity".to_string())?;
    if crate::proc::read_start_time_ticks_pub(session.pid) != Some(expected_start) {
        return Err("user-manager session process identity is stale".to_string());
    }
    if !crate::proc::process_descends_from(peer_pid, session.pid) {
        return Err("identity request did not originate from the authorized session".to_string());
    }
    let mut caps = session.caps.clone().unwrap_or_else(CapSet::new);
    if let Some(transient) = &session.transient_caps {
        caps.extend(transient.iter().cloned());
    }
    for cap in requested {
        if !caps.covers(cap) {
            return Err(format!(
                "user-manager session lacks {}:{}",
                cap.verb.as_str(),
                cap.scope
            ));
        }
    }
    Ok(session)
}

fn identity_status() -> Result<Value, String> {
    let passwd = passwd_records()?;
    let groups = group_records()?;
    let users = passwd
        .into_iter()
        .map(|user| {
            let locked = user_record(&user.name)
                .ok()
                .flatten()
                .is_some_and(|record| password_locked(&record.password));
            json!({
                "name": user.name,
                "uid": user.uid,
                "gid": user.gid,
                "gecos": user.gecos,
                "home": user.home,
                "shell": user.shell,
                "locked": locked,
                "managed": managed_id(user.uid),
            })
        })
        .collect::<Vec<_>>();
    let user_count = users.len();
    let group_count = groups.len();
    Ok(json!({
        "users": users,
        "user_count": user_count,
        "groups": groups,
        "group_count": group_count,
    }))
}

async fn mutate(
    action: &str,
    user: Option<&str>,
    group: Option<&str>,
    full_name: Option<&str>,
    shell: Option<&str>,
    groups: Option<&str>,
    password: Option<&str>,
    token: Option<&str>,
    owner_uid: u32,
) -> Result<Value, String> {
    match action {
        "create-user" => {
            let user = user.unwrap();
            if user_record(user)?.is_some() {
                return Err(format!("user already exists: {user}"));
            }
            let before = IdentitySnapshot::User(None);
            let mut args = vec![
                "--create-home",
                "--user-group",
                "--shell",
                shell.unwrap_or("/bin/bash"),
            ];
            let full_name_value = full_name.unwrap_or("");
            if !full_name_value.is_empty() {
                args.extend(["--comment", full_name_value]);
            }
            if let Some(groups) = groups {
                ensure_groups_exist(&parse_groups(groups)?)?;
                args.extend(["--groups", groups]);
            }
            args.push(user);
            let backup = prepare_mutation(owner_uid, action, "user", user, before)?;
            run_checked(useradd_path()?, &args, None, TOOL_TIMEOUT).await?;
            let applied = IdentitySnapshot::User(user_record(user)?);
            finish_mutation(backup, applied).await
        }
        "delete-user" => {
            let user = user.unwrap();
            let record = require_managed_user(user)?;
            refuse_active_processes(&record)?;
            let before = IdentitySnapshot::User(Some(record));
            let backup = prepare_mutation(owner_uid, action, "user", user, before)?;
            run_checked(userdel_path()?, &[user], None, TOOL_TIMEOUT).await?;
            let applied = IdentitySnapshot::User(user_record(user)?);
            finish_mutation(backup, applied).await
        }
        "lock-user" | "unlock-user" => {
            let user = user.unwrap();
            require_managed_user(user)?;
            let before = IdentitySnapshot::User(user_record(user)?);
            let backup = prepare_mutation(owner_uid, action, "user", user, before)?;
            run_checked(
                usermod_path()?,
                &[
                    if action == "lock-user" {
                        "--lock"
                    } else {
                        "--unlock"
                    },
                    user,
                ],
                None,
                TOOL_TIMEOUT,
            )
            .await?;
            let applied = IdentitySnapshot::User(user_record(user)?);
            finish_mutation(backup, applied).await
        }
        "set-shell" => {
            let user = user.unwrap();
            require_managed_user(user)?;
            let before = IdentitySnapshot::User(user_record(user)?);
            let backup = prepare_mutation(owner_uid, action, "user", user, before)?;
            run_checked(
                usermod_path()?,
                &["--shell", shell.unwrap(), user],
                None,
                TOOL_TIMEOUT,
            )
            .await?;
            let applied = IdentitySnapshot::User(user_record(user)?);
            finish_mutation(backup, applied).await
        }
        "set-password" => {
            let user = user.unwrap();
            require_managed_user(user)?;
            let before = IdentitySnapshot::User(user_record(user)?);
            let backup = prepare_mutation(owner_uid, action, "user", user, before)?;
            let input = format!("{user}:{}\n", password.unwrap());
            run_checked(chpasswd_path()?, &[], Some(input.as_bytes()), TOOL_TIMEOUT).await?;
            let applied = IdentitySnapshot::User(user_record(user)?);
            finish_mutation(backup, applied).await
        }
        "create-group" => {
            let group = group.unwrap();
            if group_record(group)?.is_some() {
                return Err(format!("group already exists: {group}"));
            }
            let before = IdentitySnapshot::Group(None);
            let backup = prepare_mutation(owner_uid, action, "group", group, before)?;
            run_checked(groupadd_path()?, &[group], None, TOOL_TIMEOUT).await?;
            let applied = IdentitySnapshot::Group(group_record(group)?);
            finish_mutation(backup, applied).await
        }
        "delete-group" => {
            let group = group.unwrap();
            let record = require_managed_group(group)?;
            if !record.admins.is_empty() || !record.locked {
                return Err(
                    "groups with admins or a non-locked password cannot be deleted".to_string(),
                );
            }
            let before = IdentitySnapshot::Group(Some(record));
            let backup = prepare_mutation(owner_uid, action, "group", group, before)?;
            run_checked(groupdel_path()?, &[group], None, TOOL_TIMEOUT).await?;
            let applied = IdentitySnapshot::Group(group_record(group)?);
            finish_mutation(backup, applied).await
        }
        "add-to-group" | "remove-from-group" => {
            let user = user.unwrap();
            let group = group.unwrap();
            require_managed_user(user)?;
            group_record(group)?.ok_or_else(|| format!("group not found: {group}"))?;
            let before = IdentitySnapshot::User(user_record(user)?);
            let backup = prepare_mutation(owner_uid, action, "user", user, before)?;
            run_checked(
                gpasswd_path()?,
                &[
                    if action == "add-to-group" {
                        "--add"
                    } else {
                        "--delete"
                    },
                    user,
                    group,
                ],
                None,
                TOOL_TIMEOUT,
            )
            .await?;
            let applied = IdentitySnapshot::User(user_record(user)?);
            finish_mutation(backup, applied).await
        }
        "restore" => restore_identity(owner_uid, token.unwrap()).await,
        _ => unreachable!("validated identity action"),
    }
}

fn prepare_mutation(
    owner_uid: u32,
    action: &str,
    target_kind: &str,
    target_name: &str,
    before: IdentitySnapshot,
) -> Result<IdentityBackup, String> {
    let backup = IdentityBackup {
        token: uuid::Uuid::new_v4().simple().to_string(),
        owner_uid,
        created_at: chrono::Utc::now().to_rfc3339(),
        action: action.to_string(),
        target_kind: target_kind.to_string(),
        target_name: target_name.to_string(),
        before: before.clone(),
        applied: before.clone(),
        applied_fingerprint: fingerprint(&before)?,
        status: "prepared".to_string(),
    };
    save_backup(&backup)?;
    Ok(backup)
}

async fn finish_mutation(
    mut backup: IdentityBackup,
    applied: IdentitySnapshot,
) -> Result<Value, String> {
    if backup.before == applied {
        backup.applied = applied.clone();
        backup.applied_fingerprint = fingerprint(&applied)?;
        backup.status = "no-change".to_string();
        save_backup(&backup)?;
        return Ok(json!({
            "action": backup.action,
            "changed": false,
            "state": public_snapshot(&applied),
        }));
    }
    backup.applied_fingerprint = fingerprint(&applied)?;
    backup.applied = applied.clone();
    backup.status = "applied".to_string();
    save_backup(&backup)?;
    Ok(json!({
        "action": backup.action,
        "changed": true,
        "backup_token": backup.token,
        "state": public_snapshot(&applied),
    }))
}

async fn restore_identity(owner_uid: u32, token: &str) -> Result<Value, String> {
    validate_token(token)?;
    let mut backup = load_backup(token)?;
    if backup.owner_uid != owner_uid {
        return Err("identity backup belongs to another user".to_string());
    }
    if backup.status != "applied" {
        return Err(format!(
            "identity backup is not in an applied state: {}",
            backup.status
        ));
    }
    let current = current_snapshot(&backup.target_kind, &backup.target_name)?;
    if fingerprint(&current)? != backup.applied_fingerprint {
        return Err("identity state changed after this backup was created".to_string());
    }
    if let (IdentitySnapshot::User(Some(before_user)), IdentitySnapshot::User(None)) =
        (&backup.before, &backup.applied)
    {
        if let Some(primary_group) = before_user.primary_group.as_ref() {
            if group_record(&primary_group.name)?
                .is_some_and(|current| current != primary_group.clone())
            {
                return Err(
                    "the deleted user's primary group changed after the backup was created"
                        .to_string(),
                );
            }
        }
    }
    restore_snapshot(&backup.before, &backup.target_kind, &backup.target_name).await?;
    let restored = current_snapshot(&backup.target_kind, &backup.target_name)?;
    if restored != backup.before {
        return Err("identity restore completed but state does not match the backup".to_string());
    }
    backup.status = "restored".to_string();
    save_backup(&backup)?;
    Ok(json!({
        "restored": true,
        "backup_token": token,
        "state": public_snapshot(&restored),
    }))
}

async fn restore_snapshot(
    snapshot: &IdentitySnapshot,
    target_kind: &str,
    target_name: &str,
) -> Result<(), String> {
    match snapshot {
        IdentitySnapshot::User(None) => {
            if let IdentitySnapshot::User(Some(current)) =
                current_snapshot(target_kind, target_name)?
            {
                refuse_active_processes(&current)?;
                run_checked(userdel_path()?, &[&current.name], None, TOOL_TIMEOUT).await?;
                if let Some(group) = current.primary_group.as_ref() {
                    if group.name == current.name
                        && group.members.is_empty()
                        && group.admins.is_empty()
                        && group.locked
                        && group_record(&group.name)?.is_some()
                    {
                        run_checked(groupdel_path()?, &[&group.name], None, TOOL_TIMEOUT).await?;
                    }
                }
            }
        }
        IdentitySnapshot::User(Some(desired)) => restore_user(desired).await?,
        IdentitySnapshot::Group(None) => {
            if let IdentitySnapshot::Group(Some(current)) =
                current_snapshot(target_kind, target_name)?
            {
                run_checked(groupdel_path()?, &[&current.name], None, TOOL_TIMEOUT).await?;
            }
        }
        IdentitySnapshot::Group(Some(desired)) => restore_group(desired).await?,
    }
    Ok(())
}

async fn restore_user(desired: &UserRecord) -> Result<(), String> {
    if let Some(primary_group) = desired.primary_group.as_ref() {
        if group_record(&primary_group.name)? != Some(primary_group.clone()) {
            restore_group(primary_group).await?;
        }
    }
    if user_record(&desired.name)?.is_none() {
        let uid = desired.uid.to_string();
        let gid = desired.gid.to_string();
        run_checked(
            useradd_path()?,
            &[
                "--uid",
                &uid,
                "--gid",
                &gid,
                "--home-dir",
                &desired.home,
                "--shell",
                &desired.shell,
                "--comment",
                &desired.gecos,
                "--no-create-home",
                &desired.name,
            ],
            None,
            TOOL_TIMEOUT,
        )
        .await?;
    } else {
        let uid = desired.uid.to_string();
        let gid = desired.gid.to_string();
        run_checked(
            usermod_path()?,
            &[
                "--uid",
                &uid,
                "--gid",
                &gid,
                "--home",
                &desired.home,
                "--shell",
                &desired.shell,
                "--comment",
                &desired.gecos,
                &desired.name,
            ],
            None,
            TOOL_TIMEOUT,
        )
        .await?;
    }
    let groups = desired.supplementary_groups.join(",");
    run_checked(
        usermod_path()?,
        &["--groups", &groups, &desired.name],
        None,
        TOOL_TIMEOUT,
    )
    .await?;
    let hash_input = format!("{}:{}\n", desired.name, desired.password);
    run_checked(
        chpasswd_path()?,
        &["--encrypted"],
        Some(hash_input.as_bytes()),
        TOOL_TIMEOUT,
    )
    .await?;
    let values = [
        desired.last_change,
        desired.minimum,
        desired.maximum,
        desired.warning,
        desired.inactive,
        desired.expire,
    ]
    .map(|value| value.to_string());
    run_checked(
        chage_path()?,
        &[
            "--lastday",
            &values[0],
            "--mindays",
            &values[1],
            "--maxdays",
            &values[2],
            "--warndays",
            &values[3],
            "--inactive",
            &values[4],
            "--expiredate",
            &values[5],
            &desired.name,
        ],
        None,
        TOOL_TIMEOUT,
    )
    .await?;
    Ok(())
}

async fn restore_group(desired: &GroupRecord) -> Result<(), String> {
    if !desired.admins.is_empty() || !desired.locked {
        return Err("cannot restore group admins or an unlocked group password".to_string());
    }
    let gid = desired.gid.to_string();
    if group_record(&desired.name)?.is_none() {
        run_checked(
            groupadd_path()?,
            &["--gid", &gid, &desired.name],
            None,
            TOOL_TIMEOUT,
        )
        .await?;
    } else {
        run_checked(
            groupmod_path()?,
            &["--gid", &gid, &desired.name],
            None,
            TOOL_TIMEOUT,
        )
        .await?;
    }
    let members = desired.members.join(",");
    run_checked(
        gpasswd_path()?,
        &["--members", &members, &desired.name],
        None,
        TOOL_TIMEOUT,
    )
    .await?;
    Ok(())
}

fn current_snapshot(kind: &str, name: &str) -> Result<IdentitySnapshot, String> {
    match kind {
        "user" => Ok(IdentitySnapshot::User(user_record(name)?)),
        "group" => Ok(IdentitySnapshot::Group(group_record(name)?)),
        _ => Err(format!("unknown identity backup target kind: {kind}")),
    }
}

fn public_snapshot(snapshot: &IdentitySnapshot) -> Value {
    match snapshot {
        IdentitySnapshot::User(user) => json!({
            "kind": "user",
            "present": user.is_some(),
            "record": user.as_ref().map(public_user),
        }),
        IdentitySnapshot::Group(group) => json!({
            "kind": "group",
            "present": group.is_some(),
            "record": group,
        }),
    }
}

fn public_user(user: &UserRecord) -> Value {
    json!({
        "name": user.name,
        "uid": user.uid,
        "gid": user.gid,
        "gecos": user.gecos,
        "home": user.home,
        "shell": user.shell,
        "locked": password_locked(&user.password),
        "last_change": user.last_change,
        "minimum": user.minimum,
        "maximum": user.maximum,
        "warning": user.warning,
        "inactive": user.inactive,
        "expire": user.expire,
        "supplementary_groups": user.supplementary_groups,
        "primary_group": user.primary_group.as_ref().map(|group| json!({
            "name": group.name,
            "gid": group.gid,
        })),
    })
}

fn fingerprint(snapshot: &IdentitySnapshot) -> Result<String, String> {
    let data = serde_json::to_vec(snapshot)
        .map_err(|error| format!("serialize identity fingerprint: {error}"))?;
    Ok(hex::encode(Sha256::digest(data)))
}

fn user_record(name: &str) -> Result<Option<UserRecord>, String> {
    let passwd = passwd_records()?
        .into_iter()
        .find(|record| record.name == name);
    let Some(passwd) = passwd else {
        return Ok(None);
    };
    let shadow =
        shadow_record(name)?.ok_or_else(|| format!("shadow entry is missing for user {name}"))?;
    let supplementary_groups = group_records()?
        .into_iter()
        .filter(|group| group.members.iter().any(|member| member == name))
        .map(|group| group.name)
        .collect::<Vec<_>>();
    let primary_group = group_records()?
        .into_iter()
        .find(|group| group.gid == passwd.gid);
    Ok(Some(UserRecord {
        name: passwd.name,
        uid: passwd.uid,
        gid: passwd.gid,
        gecos: passwd.gecos,
        home: passwd.home,
        shell: passwd.shell,
        password: shadow.password,
        last_change: shadow.last_change,
        minimum: shadow.minimum,
        maximum: shadow.maximum,
        warning: shadow.warning,
        inactive: shadow.inactive,
        expire: shadow.expire,
        supplementary_groups,
        primary_group,
    }))
}

#[derive(Clone)]
struct PasswdRecord {
    name: String,
    uid: u32,
    gid: u32,
    gecos: String,
    home: String,
    shell: String,
}

fn passwd_records() -> Result<Vec<PasswdRecord>, String> {
    let data =
        fs::read_to_string("/etc/passwd").map_err(|error| format!("read /etc/passwd: {error}"))?;
    Ok(data
        .lines()
        .filter_map(|line| {
            let fields = line.split(':').collect::<Vec<_>>();
            if fields.len() != 7 {
                return None;
            }
            Some(PasswdRecord {
                name: fields[0].to_string(),
                uid: fields[2].parse().ok()?,
                gid: fields[3].parse().ok()?,
                gecos: fields[4].to_string(),
                home: fields[5].to_string(),
                shell: fields[6].to_string(),
            })
        })
        .collect())
}

struct ShadowRecord {
    password: String,
    last_change: i64,
    minimum: i64,
    maximum: i64,
    warning: i64,
    inactive: i64,
    expire: i64,
}

fn shadow_record(name: &str) -> Result<Option<ShadowRecord>, String> {
    let data =
        fs::read_to_string("/etc/shadow").map_err(|error| format!("read /etc/shadow: {error}"))?;
    for line in data.lines() {
        let fields = line.split(':').collect::<Vec<_>>();
        if fields.len() != 9 || fields[0] != name {
            continue;
        }
        return Ok(Some(ShadowRecord {
            password: fields[1].to_string(),
            last_change: parse_shadow_number(fields[2]),
            minimum: parse_shadow_number(fields[3]),
            maximum: parse_shadow_number(fields[4]),
            warning: parse_shadow_number(fields[5]),
            inactive: parse_shadow_number(fields[6]),
            expire: parse_shadow_number(fields[7]),
        }));
    }
    Ok(None)
}

fn parse_shadow_number(value: &str) -> i64 {
    if value.is_empty() {
        -1
    } else {
        value.parse().unwrap_or(-1)
    }
}

fn group_records() -> Result<Vec<GroupRecord>, String> {
    let group =
        fs::read_to_string("/etc/group").map_err(|error| format!("read /etc/group: {error}"))?;
    let gshadow = fs::read_to_string("/etc/gshadow")
        .map_err(|error| format!("read /etc/gshadow: {error}"))?;
    let shadow = gshadow
        .lines()
        .filter_map(|line| {
            let fields = line.split(':').collect::<Vec<_>>();
            (fields.len() == 4).then(|| {
                (
                    fields[0].to_string(),
                    (
                        password_locked(fields[1]),
                        split_list(fields[2]),
                        split_list(fields[3]),
                    ),
                )
            })
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    Ok(group
        .lines()
        .filter_map(|line| {
            let fields = line.split(':').collect::<Vec<_>>();
            if fields.len() != 4 {
                return None;
            }
            let (locked, admins, _) =
                shadow
                    .get(fields[0])
                    .cloned()
                    .unwrap_or((true, Vec::new(), Vec::new()));
            Some(GroupRecord {
                name: fields[0].to_string(),
                gid: fields[2].parse().ok()?,
                locked,
                members: split_list(fields[3]),
                admins,
            })
        })
        .collect())
}

fn group_record(name: &str) -> Result<Option<GroupRecord>, String> {
    Ok(group_records()?
        .into_iter()
        .find(|record| record.name == name))
}

fn split_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn require_managed_user(name: &str) -> Result<UserRecord, String> {
    let record = user_record(name)?.ok_or_else(|| format!("user not found: {name}"))?;
    if !managed_id(record.uid) {
        return Err(format!(
            "refusing to modify system user {name} (uid {})",
            record.uid
        ));
    }
    Ok(record)
}

fn require_managed_group(name: &str) -> Result<GroupRecord, String> {
    let record = group_record(name)?.ok_or_else(|| format!("group not found: {name}"))?;
    if !managed_id(record.gid) {
        return Err(format!(
            "refusing to modify system group {name} (gid {})",
            record.gid
        ));
    }
    Ok(record)
}

fn managed_id(id: u32) -> bool {
    (MIN_MANAGED_ID..=MAX_MANAGED_ID).contains(&id)
}

fn refuse_active_processes(user: &UserRecord) -> Result<(), String> {
    let mut pids = Vec::new();
    if let Ok(entries) = fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|value| value.parse::<u32>().ok())
            else {
                continue;
            };
            let Ok(status) = fs::read_to_string(entry.path().join("status")) else {
                continue;
            };
            let uid = status
                .lines()
                .find_map(|line| line.strip_prefix("Uid:"))
                .and_then(|line| line.split_whitespace().next())
                .and_then(|value| value.parse::<u32>().ok());
            if uid == Some(user.uid) {
                pids.push(pid);
                if pids.len() == 16 {
                    break;
                }
            }
        }
    }
    if pids.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "user {} still owns active processes: {:?}",
            user.name, pids
        ))
    }
}

fn ensure_groups_exist(groups: &[String]) -> Result<(), String> {
    let existing = group_records()?
        .into_iter()
        .map(|group| group.name)
        .collect::<BTreeSet<_>>();
    for group in groups {
        if !existing.contains(group) {
            return Err(format!("supplementary group not found: {group}"));
        }
    }
    Ok(())
}

fn parse_groups(value: &str) -> Result<Vec<String>, String> {
    let groups = split_list(value);
    if groups.is_empty() || groups.len() > 64 {
        return Err("groups must contain 1-64 comma-separated names".to_string());
    }
    for group in &groups {
        validate_account_name("group", group)?;
    }
    Ok(groups)
}

fn validate_account_name(kind: &str, value: &str) -> Result<(), String> {
    let mut bytes = value.bytes();
    let first = bytes.next().unwrap_or_default();
    if value.is_empty()
        || value.len() > 32
        || !(first.is_ascii_lowercase() || first == b'_')
        || !bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
    {
        return Err(format!("invalid {kind} name: {value:?}"));
    }
    Ok(())
}

fn validate_full_name(value: &str) -> Result<(), String> {
    if value.len() > 128
        || value.contains(':')
        || value.chars().any(|character| character.is_control())
    {
        Err("full name must be at most 128 characters without ':' or controls".to_string())
    } else {
        Ok(())
    }
}

fn validate_shell(value: &str) -> Result<(), String> {
    if !value.starts_with('/')
        || value.len() > 255
        || value.chars().any(|character| character.is_control())
    {
        return Err("shell must be an absolute path".to_string());
    }
    let shells =
        fs::read_to_string("/etc/shells").map_err(|error| format!("read /etc/shells: {error}"))?;
    if shells
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .any(|line| line == value)
    {
        Ok(())
    } else {
        Err(format!("shell is not listed in /etc/shells: {value}"))
    }
}

fn password_locked(password: &str) -> bool {
    password.is_empty() || password.starts_with('!') || password.starts_with('*')
}

fn validate_password(password: &str) -> Result<(), String> {
    if password.is_empty()
        || password.len() > 1024
        || password.chars().any(|character| character.is_control())
    {
        Err("password credential must be a non-empty single-line secret".to_string())
    } else {
        Ok(())
    }
}

fn parse_credential_ref(value: &str) -> Result<(String, String), String> {
    let (namespace, name) = value
        .split_once('/')
        .ok_or_else(|| "credential must use namespace/name form".to_string())?;
    validate_credential_name(namespace)?;
    validate_credential_name(name)?;
    Ok((namespace.to_string(), name.to_string()))
}

fn validate_credential_name(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 255
        || value.starts_with('-')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
    {
        Err(format!("invalid credential component: {value:?}"))
    } else {
        Ok(())
    }
}

fn validate_action(
    action: &str,
    user: Option<&str>,
    group: Option<&str>,
    full_name: Option<&str>,
    shell: Option<&str>,
    groups: Option<&str>,
    credential: Option<&str>,
    token: Option<&str>,
    confirm: bool,
) -> Result<(), String> {
    match action {
        "status"
            if user.is_none()
                && group.is_none()
                && full_name.is_none()
                && shell.is_none()
                && groups.is_none()
                && credential.is_none()
                && token.is_none()
                && !confirm =>
        {
            Ok(())
        }
        "create-user"
            if user.is_some()
                && group.is_none()
                && token.is_none()
                && credential.is_none()
                && !confirm =>
        {
            Ok(())
        }
        "delete-user"
            if user.is_some()
                && group.is_none()
                && full_name.is_none()
                && shell.is_none()
                && groups.is_none()
                && credential.is_none()
                && token.is_none()
                && confirm =>
        {
            Ok(())
        }
        "lock-user" | "unlock-user"
            if user.is_some()
                && group.is_none()
                && full_name.is_none()
                && shell.is_none()
                && groups.is_none()
                && credential.is_none()
                && token.is_none()
                && !confirm =>
        {
            Ok(())
        }
        "set-shell"
            if user.is_some()
                && shell.is_some()
                && group.is_none()
                && full_name.is_none()
                && groups.is_none()
                && credential.is_none()
                && token.is_none()
                && !confirm =>
        {
            Ok(())
        }
        "set-password"
            if user.is_some()
                && credential.is_some()
                && group.is_none()
                && full_name.is_none()
                && shell.is_none()
                && groups.is_none()
                && token.is_none()
                && !confirm =>
        {
            Ok(())
        }
        "create-group"
            if group.is_some()
                && user.is_none()
                && full_name.is_none()
                && shell.is_none()
                && groups.is_none()
                && credential.is_none()
                && token.is_none()
                && !confirm =>
        {
            Ok(())
        }
        "delete-group"
            if group.is_some()
                && user.is_none()
                && full_name.is_none()
                && shell.is_none()
                && groups.is_none()
                && credential.is_none()
                && token.is_none()
                && confirm =>
        {
            Ok(())
        }
        "add-to-group" | "remove-from-group"
            if user.is_some()
                && group.is_some()
                && full_name.is_none()
                && shell.is_none()
                && groups.is_none()
                && credential.is_none()
                && token.is_none()
                && !confirm =>
        {
            Ok(())
        }
        "restore"
            if token.is_some_and(|token| validate_token(token).is_ok())
                && user.is_none()
                && group.is_none()
                && full_name.is_none()
                && shell.is_none()
                && groups.is_none()
                && credential.is_none()
                && confirm =>
        {
            Ok(())
        }
        _ => Err(format!("invalid arguments for identity action {action:?}")),
    }
}

fn prepare_backup_dir() -> Result<(), String> {
    let dir = backup_dir();
    fs::create_dir_all(&dir)
        .map_err(|error| format!("create identity backup directory: {error}"))?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("secure identity backup directory: {error}"))
}

fn save_backup(backup: &IdentityBackup) -> Result<(), String> {
    prepare_backup_dir()?;
    let data = serde_json::to_vec_pretty(backup)
        .map_err(|error| format!("serialize identity backup: {error}"))?;
    crate::agent::util::atomic_write_with_fsync(&backup_path(&backup.token), &data)
        .map_err(|error| format!("write identity backup: {error}"))
}

fn load_backup(token: &str) -> Result<IdentityBackup, String> {
    let data =
        fs::read(backup_path(token)).map_err(|error| format!("read identity backup: {error}"))?;
    serde_json::from_slice(&data).map_err(|error| format!("parse identity backup: {error}"))
}

fn validate_token(token: &str) -> Result<(), String> {
    if token.len() == 32 && token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err("invalid identity backup token".to_string())
    }
}

fn backup_dir() -> PathBuf {
    crate::paths::data_dir()
        .join("clawd")
        .join("identity-backups")
}

fn backup_path(token: &str) -> PathBuf {
    backup_dir().join(format!("{token}.json"))
}

async fn run_checked(
    program: &'static str,
    args: &[&str],
    stdin_data: Option<&[u8]>,
    timeout: Duration,
) -> Result<CommandOutput, String> {
    let args = args.iter().map(|value| value.to_string()).collect();
    let stdin_data = stdin_data.map(Vec::from);
    tokio::task::spawn_blocking(move || run_checked_sync(program, args, stdin_data, timeout))
        .await
        .map_err(|error| format!("{program} worker failed: {error}"))?
}

fn run_checked_sync(
    program: &str,
    args: Vec<String>,
    stdin_data: Option<Vec<u8>>,
    timeout: Duration,
) -> Result<CommandOutput, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("HOME", "/root")
        .env("LC_ALL", "C.UTF-8")
        .current_dir("/")
        .stdin(if stdin_data.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to launch {program}: {error}"))?;
    if let Some(data) = stdin_data {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| format!("{program} stdin is unavailable"))?;
        stdin
            .write_all(&data)
            .map_err(|error| format!("write {program} input: {error}"))?;
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("{program} stdout is unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("{program} stderr is unavailable"))?;
    let stdout_reader = std::thread::spawn(move || read_bounded(stdout));
    let stderr_reader = std::thread::spawn(move || read_bounded(stderr));
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                timed_out = true;
                let _ = child.kill();
                break child
                    .wait()
                    .map_err(|error| format!("wait for timed-out {program}: {error}"))?;
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("wait for {program}: {error}"));
            }
        }
    };
    let (stdout, stdout_truncated) = stdout_reader
        .join()
        .map_err(|_| format!("{program} stdout reader panicked"))??;
    let (stderr, stderr_truncated) = stderr_reader
        .join()
        .map_err(|_| format!("{program} stderr reader panicked"))??;
    if timed_out {
        return Err(format!("{program} timed out after {}s", timeout.as_secs()));
    }
    let output = CommandOutput {
        status,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        stdout_truncated,
        stderr_truncated,
    };
    if !output.status.success() {
        return Err(format!(
            "{} exited {}: {}",
            program,
            output.status.code().unwrap_or(-1),
            tail(&output.stderr)
        ));
    }
    Ok(output)
}

struct CommandOutput {
    status: ExitStatus,
    stdout: String,
    stderr: String,
    #[allow(dead_code)]
    stdout_truncated: bool,
    #[allow(dead_code)]
    stderr_truncated: bool,
}

fn read_bounded(mut reader: impl Read) -> Result<(Vec<u8>, bool), String> {
    let mut kept = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    let mut truncated = false;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("read identity command output: {error}"))?;
        if read == 0 {
            break;
        }
        let remaining = STREAM_CAP_BYTES.saturating_sub(kept.len());
        let keep = remaining.min(read);
        kept.extend_from_slice(&buffer[..keep]);
        truncated |= keep < read;
    }
    Ok((kept, truncated))
}

fn tool_path(candidates: &[&'static str], name: &str) -> Result<&'static str, String> {
    candidates
        .iter()
        .copied()
        .find(|candidate| Path::new(candidate).is_file())
        .ok_or_else(|| format!("{name} is not installed"))
}

fn useradd_path() -> Result<&'static str, String> {
    tool_path(&["/usr/sbin/useradd", "/usr/bin/useradd"], "useradd")
}
fn userdel_path() -> Result<&'static str, String> {
    tool_path(&["/usr/sbin/userdel", "/usr/bin/userdel"], "userdel")
}
fn usermod_path() -> Result<&'static str, String> {
    tool_path(&["/usr/sbin/usermod", "/usr/bin/usermod"], "usermod")
}
fn groupadd_path() -> Result<&'static str, String> {
    tool_path(&["/usr/sbin/groupadd", "/usr/bin/groupadd"], "groupadd")
}
fn groupdel_path() -> Result<&'static str, String> {
    tool_path(&["/usr/sbin/groupdel", "/usr/bin/groupdel"], "groupdel")
}
fn groupmod_path() -> Result<&'static str, String> {
    tool_path(&["/usr/sbin/groupmod", "/usr/bin/groupmod"], "groupmod")
}
fn gpasswd_path() -> Result<&'static str, String> {
    tool_path(&["/usr/bin/gpasswd", "/usr/sbin/gpasswd"], "gpasswd")
}
fn chpasswd_path() -> Result<&'static str, String> {
    tool_path(&["/usr/sbin/chpasswd", "/usr/bin/chpasswd"], "chpasswd")
}
fn chage_path() -> Result<&'static str, String> {
    tool_path(&["/usr/bin/chage", "/usr/sbin/chage"], "chage")
}

fn optional_string(params: &Value, key: &str) -> Result<Option<String>, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value.clone())),
        Some(Value::String(_)) => Ok(None),
        Some(_) => Err(format!("parameter `{key}` must be a string or null")),
    }
}

fn required_string(params: &Value, key: &str) -> Result<String, String> {
    optional_string(params, key)?.ok_or_else(|| format!("missing required string parameter: {key}"))
}

fn tail(value: &str) -> String {
    const MAX: usize = 8 * 1024;
    if value.len() <= MAX {
        return value.trim().to_string();
    }
    let mut start = value.len() - MAX;
    while !value.is_char_boundary(start) {
        start += 1;
    }
    value[start..].trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_names_are_strict() {
        validate_account_name("user", "alice").unwrap();
        assert!(validate_account_name("user", "Alice").is_err());
        assert!(validate_account_name("user", "../root").is_err());
    }

    #[test]
    fn passwords_are_never_allowed_to_span_lines() {
        validate_password("correct horse battery staple").unwrap();
        assert!(validate_password("line1\nline2").is_err());
    }
}
