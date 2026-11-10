use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::future::Future;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::caps::{Cap, Scope, Verb};
use crate::proc::SessionInfo;

use super::authority::{Audience, Authorized, Decision};
use super::client_identity::{ClientIdentity, FsIdentityGuard};
use super::protocol::BrokerError;

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

pub async fn control(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, BrokerError> {
    let owner = validate_owner(&params, client, authority).map_err(BrokerError::authorization)?;
    if let Some(change) = AccountChange::decode(&params).map_err(BrokerError::execution)? {
        return account_control(change, owner, client, authority).await;
    }
    for field in ["new_name", "icon", "account_type"] {
        if params.get(field).is_some_and(|value| !value.is_null()) {
            return Err(BrokerError::execution(format!(
                "{field} is not valid for this identity action"
            )));
        }
    }
    if expected_uid(&params)
        .map_err(BrokerError::execution)?
        .is_some()
    {
        let action = required_string(&params, "action").map_err(BrokerError::execution)?;
        if !matches!(
            action.as_str(),
            "delete-user" | "lock-user" | "unlock-user" | "set-shell" | "set-password"
        ) {
            return Err(BrokerError::execution(
                "uid is not valid for this identity action",
            ));
        }
        let user = required_string(&params, "user").map_err(BrokerError::execution)?;
        validate_account_name("user", &user).map_err(BrokerError::execution)?;
    }
    control_legacy(params, client, authority)
        .await
        .map_err(BrokerError::from)
}

fn validate_owner(
    params: &Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<u32, String> {
    let owner = client.require_uid()?;
    if authority.owner_uid() != owner
        || authority.session_id() != params.get("session").and_then(Value::as_str)
        || authority.audience() != Audience::SystemService
    {
        return Err("identity owner/session mismatch".into());
    }
    authority.ensure_current()?;
    Ok(owner)
}

fn expected_uid(params: &Value) -> Result<Option<u32>, String> {
    match params.get("uid") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| "uid must be an unsigned 32-bit target identity".into()),
    }
}

async fn control_legacy(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client, authority);
        return Err("User Manager requires Linux shadow utilities".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err("User Manager requires root clawd".to_string());
        }
        let uid = client.require_uid()?;
        let home = client.require_home_dir()?;
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
        let requested = requested_caps(&action, credential_ref.as_ref());
        if action == "status" {
            let (_, _authorized) = authorize_session(authority, &requested)?;
            return identity_status();
        }
        let _guard = match tokio::time::timeout(
            LOCK_TIMEOUT,
            USER_LOCK.get_or_init(|| tokio::sync::Mutex::new(())).lock(),
        )
        .await
        {
            Ok(guard) => guard,
            Err(_) => {
                let (_, _authorized) = authorize_session(authority, &requested)?;
                return Err("User Manager is busy with another identity mutation".into());
            }
        };
        let (session, authorized) = authorize_session(authority, &requested)?;
        if let Some(uid) = expected_uid(&params)? {
            local_target(user.as_deref().ok_or("identity target is missing")?, uid)?;
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

        let mut result = mutate(
            &authorized,
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
        .await?;
        if let Some(uid) = expected_uid(&params)? {
            result["uid"] = json!(uid);
            result["user"] = json!(user);
        }
        Ok(result)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum AccountValue {
    Show,
    Name(String),
    FullName(String),
    Icon(PathBuf),
    Type(i32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AccountChange {
    user: String,
    uid: u32,
    value: AccountValue,
}

impl AccountChange {
    fn decode(params: &Value) -> Result<Option<Self>, String> {
        let action = params.get("action").and_then(Value::as_str).unwrap_or("");
        let field = match action {
            "show-user" => None,
            "rename-user" => Some("new_name"),
            "set-full-name" => Some("full_name"),
            "set-icon" => Some("icon"),
            "set-account-type" => Some("account_type"),
            _ => return Ok(None),
        };
        let object = params
            .as_object()
            .ok_or("identity request must be an object")?;
        for (key, value) in object {
            if matches!(key.as_str(), "session" | "action" | "user" | "uid")
                || field == Some(key.as_str())
                || (value.is_null()
                    && matches!(
                        key.as_str(),
                        "group"
                            | "full_name"
                            | "shell"
                            | "groups"
                            | "credential"
                            | "token"
                            | "new_name"
                            | "icon"
                            | "account_type"
                            | "confirm"
                    ))
                || (key == "confirm" && value == &Value::Bool(false))
            {
                continue;
            }
            return Err(format!("{key} is not valid for {action}"));
        }
        let user = required_string(params, "user")?;
        validate_account_name("user", &user)?;
        let uid = expected_uid(params)?.ok_or("this identity action requires an expected uid")?;
        if !managed_id(uid) {
            return Err("refusing to modify or disclose a system account".into());
        }
        let value = match action {
            "show-user" => AccountValue::Show,
            "rename-user" => {
                let name = required_string(params, "new_name")?;
                validate_account_name("new user", &name)?;
                AccountValue::Name(name)
            }
            "set-full-name" => {
                let name = params
                    .get("full_name")
                    .and_then(Value::as_str)
                    .ok_or("full_name must be a string")?
                    .to_owned();
                validate_full_name(&name)?;
                if name.contains(',') {
                    return Err("AccountsService real names cannot contain commas".into());
                }
                AccountValue::FullName(name)
            }
            "set-icon" => {
                let raw = params
                    .get("icon")
                    .and_then(Value::as_str)
                    .ok_or("icon must be a string")?;
                AccountValue::Icon(icon_path(raw)?)
            }
            "set-account-type" => {
                AccountValue::Type(match required_string(params, "account_type")?.as_str() {
                    "standard" => 0,
                    "administrator" => 1,
                    _ => return Err("account_type must be standard or administrator".into()),
                })
            }
            _ => unreachable!(),
        };
        Ok(Some(Self { user, uid, value }))
    }

    fn action(&self) -> &'static str {
        match self.value {
            AccountValue::Show => "show-user",
            AccountValue::Name(_) => "rename-user",
            AccountValue::FullName(_) => "set-full-name",
            AccountValue::Icon(_) => "set-icon",
            AccountValue::Type(_) => "set-account-type",
        }
    }

    fn caps(&self) -> Vec<Cap> {
        let mut caps = if matches!(self.value, AccountValue::Show) {
            vec![Cap::new(Verb::SYS_OBSERVE, Scope::name("identities"))]
        } else {
            vec![Cap::new(Verb::SYS_IDENTITY, Scope::name("manage"))]
        };
        if let AccountValue::Icon(path) = &self.value {
            caps.push(Cap::new(
                Verb::FS_READ,
                Scope::path(path.to_string_lossy().into_owned()),
            ));
        }
        caps
    }
}

fn icon_path(raw: &str) -> Result<PathBuf, String> {
    let path = Path::new(raw);
    let normalized = path.components().collect::<PathBuf>();
    if raw.len() > 4096
        || raw.chars().any(char::is_control)
        || !path.is_absolute()
        || normalized.as_os_str() != path.as_os_str()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err("icon must be one normalized absolute file path".into());
    }
    Ok(path.to_owned())
}

fn local_target(name: &str, uid: u32) -> Result<PasswdRecord, String> {
    let record = passwd_records()?
        .into_iter()
        .find(|record| record.name == name)
        .ok_or_else(|| format!("local user not found: {name}"))?;
    validate_target(&record, name, uid)?;
    Ok(record)
}

fn validate_target(record: &PasswdRecord, name: &str, uid: u32) -> Result<(), String> {
    if !managed_id(uid) || record.uid != uid || record.name != name {
        return Err("identity target name/uid changed or is not a managed local account".into());
    }
    Ok(())
}

fn account_preflight(
    change: &AccountChange,
    owner: u32,
    authority: &Decision,
) -> Result<(), BrokerError> {
    if authority.owner_uid() != owner || authority.audience() != Audience::SystemService {
        return Err(BrokerError::authorization("identity owner mismatch"));
    }
    authority
        .ensure_current()
        .map_err(BrokerError::authorization)?;
    if !change.caps().iter().all(|cap| authority.caps().covers(cap)) {
        return Err(BrokerError::authorization(
            "identity action requires its exact declared capabilities",
        ));
    }
    Ok(())
}

async fn account_control(
    change: AccountChange,
    owner: u32,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, BrokerError> {
    account_preflight(&change, owner, authority)?;
    let result = account_apply(&change, owner, client, authority).await;
    if !super::authority::obligation_met(Some(authority)) {
        let _authorized = authority
            .require_all(&change.caps())
            .map_err(BrokerError::authorization)?;
    }
    result
}

async fn account_apply(
    change: &AccountChange,
    owner: u32,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, BrokerError> {
    if unsafe { libc::geteuid() } != 0 {
        return Err(BrokerError::unavailable(
            "identity actions require root clawd",
        ));
    }
    let _guard = tokio::time::timeout(
        LOCK_TIMEOUT,
        USER_LOCK.get_or_init(|| tokio::sync::Mutex::new(())).lock(),
    )
    .await
    .map_err(|_| BrokerError::unavailable("identity provider is busy; no change was dispatched"))?;
    authority
        .ensure_current()
        .map_err(BrokerError::authorization)?;
    let record = local_target(&change.user, change.uid).map_err(BrokerError::execution)?;
    if let AccountValue::Name(name) = &change.value {
        if name != &change.user
            && passwd_records()
                .map_err(BrokerError::execution)?
                .iter()
                .any(|record| record.name == *name)
        {
            return Err(BrokerError::execution(
                "the requested login name already exists",
            ));
        }
    }
    let backend = AccountsBackend::system().await?;
    let user = backend.user(change.uid).await?;
    backend.check_target(&user, &record).await?;
    let icon_home = if matches!(change.value, AccountValue::Icon(_)) {
        Some(
            client
                .require_home_dir()
                .map_err(BrokerError::authorization)?,
        )
    } else {
        None
    };
    backend.require_current(&user).await?;
    local_target(&change.user, change.uid).map_err(BrokerError::execution)?;
    let authorized = authority
        .require_all(&change.caps())
        .map_err(BrokerError::authorization)?;
    let icon = match (&change.value, icon_home.as_deref()) {
        (AccountValue::Icon(path), Some(home)) => {
            let source = icon_source(path, owner, home, &authorized)?;
            Some(seal_icon(source, &authorized)?)
        }
        _ => None,
    };
    backend
        .apply(&user, change, &record, icon.as_ref(), &authorized)
        .await
}

fn icon_source(
    path: &Path,
    owner: u32,
    home: &Path,
    authorized: &Authorized,
) -> Result<File, BrokerError> {
    let exact = Cap::new(
        Verb::FS_READ,
        Scope::path(path.to_string_lossy().into_owned()),
    );
    if !authorized.spent().contains(&exact) {
        return Err(BrokerError::authorization(
            "icon source requires its exact spent file-read capability",
        ));
    }
    crate::worker::derive::reject_forbidden_for_owner(path, home)
        .map_err(BrokerError::authorization)?;
    let _identity = FsIdentityGuard::enter(owner).map_err(BrokerError::authorization)?;
    let file = super::owner_files::source(path).map_err(BrokerError::execution)?;
    let metadata = file
        .metadata()
        .map_err(|error| BrokerError::execution(error.to_string()))?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > 1_048_576 {
        return Err(BrokerError::execution(
            "icon must be a single-link regular file of at most 1 MiB",
        ));
    }
    Ok(file)
}

fn seal_icon(file: File, _authorized: &Authorized) -> Result<File, BrokerError> {
    let mut bytes = Vec::new();
    file.take(1_048_577)
        .read_to_end(&mut bytes)
        .map_err(|error| BrokerError::execution(format!("read authorized icon: {error}")))?;
    if bytes.len() > 1_048_576 {
        return Err(BrokerError::execution(
            "icon grew beyond 1 MiB; no change was dispatched",
        ));
    }
    let fd = unsafe {
        libc::memfd_create(
            c"account-icon".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if fd < 0 {
        return Err(BrokerError::unavailable(format!(
            "create private icon: {}",
            std::io::Error::last_os_error()
        )));
    }
    let mut sealed = unsafe { File::from_raw_fd(fd) };
    sealed
        .write_all(&bytes)
        .map_err(|error| BrokerError::execution(error.to_string()))?;
    sealed
        .set_permissions(fs::Permissions::from_mode(0o400))
        .map_err(|error| BrokerError::execution(error.to_string()))?;
    if unsafe {
        libc::fcntl(
            fd,
            libc::F_ADD_SEALS,
            libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL,
        )
    } < 0
    {
        return Err(BrokerError::unavailable(format!(
            "seal private icon: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(sealed)
}

struct AccountsBackend {
    connection: zbus::Connection,
    accounts: zbus::Proxy<'static>,
}

impl AccountsBackend {
    async fn system() -> Result<Self, BrokerError> {
        let connection = account_before("connect system bus", async {
            zbus::connection::Builder::address("unix:path=/run/dbus/system_bus_socket")?
                .build()
                .await
        })
        .await?;
        let bus = zbus::fdo::DBusProxy::new(&connection)
            .await
            .map_err(account_unavailable)?;
        let bus_uid = account_before("authenticate system bus", async {
            bus.get_connection_unix_user(zbus::names::BusName::try_from("org.freedesktop.DBus")?)
                .await
                .map_err(Into::into)
        })
        .await?;
        if bus_uid != 0 {
            return Err(BrokerError::unavailable(
                "identity provider requires the root-owned system bus",
            ));
        }
        let name = zbus::names::WellKnownName::try_from("org.freedesktop.Accounts")
            .map_err(account_unavailable)?;
        let service = account_before("identify AccountsService", async {
            match bus.get_name_owner(name.clone().into()).await {
                Ok(owner) => Ok(owner),
                Err(zbus::fdo::Error::NameHasNoOwner(_)) => {
                    bus.start_service_by_name(name.clone(), 0).await?;
                    bus.get_name_owner(name.into()).await.map_err(Into::into)
                }
                Err(error) => Err(error.into()),
            }
        })
        .await?;
        let service_uid = account_before("authenticate AccountsService", async {
            bus.get_connection_unix_user(service.clone().into())
                .await
                .map_err(Into::into)
        })
        .await?;
        if service_uid != 0 {
            return Err(BrokerError::unavailable(
                "AccountsService is not root-owned",
            ));
        }
        let accounts = zbus::proxy::Builder::<zbus::Proxy<'static>>::new(&connection)
            .destination(service)
            .map_err(account_unavailable)?
            .path("/org/freedesktop/Accounts")
            .map_err(account_unavailable)?
            .interface("org.freedesktop.Accounts")
            .map_err(account_unavailable)?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await
            .map_err(account_unavailable)?;
        Ok(Self {
            connection,
            accounts,
        })
    }

    async fn user(&self, uid: u32) -> Result<zbus::Proxy<'static>, BrokerError> {
        let path: zbus::zvariant::OwnedObjectPath = account_before(
            "resolve local account",
            self.accounts.call("FindUserById", &(i64::from(uid),)),
        )
        .await?;
        if path.as_str() != format!("/org/freedesktop/Accounts/User{uid}") {
            return Err(BrokerError::authorization(
                "AccountsService returned a different target path",
            ));
        }
        zbus::proxy::Builder::<zbus::Proxy<'static>>::new(&self.connection)
            .destination(self.accounts.destination().to_owned())
            .map_err(account_unavailable)?
            .path(path)
            .map_err(account_unavailable)?
            .interface("org.freedesktop.Accounts.User")
            .map_err(account_unavailable)?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await
            .map_err(account_unavailable)
    }

    async fn check_target(
        &self,
        user: &zbus::Proxy<'_>,
        record: &PasswdRecord,
    ) -> Result<(), BrokerError> {
        let uid: u64 = account_before("verify target uid", user.get_property("Uid")).await?;
        let name: String =
            account_before("verify target name", user.get_property("UserName")).await?;
        let home: String =
            account_before("verify target home", user.get_property("HomeDirectory")).await?;
        let local: bool =
            account_before("verify local account", user.get_property("LocalAccount")).await?;
        if uid != u64::from(record.uid) || name != record.name || home != record.home || !local {
            return Err(BrokerError::authorization(
                "AccountsService target no longer matches the local account",
            ));
        }
        Ok(())
    }

    async fn require_current(&self, user: &zbus::Proxy<'_>) -> Result<(), BrokerError> {
        let bus = zbus::fdo::DBusProxy::new(&self.connection)
            .await
            .map_err(account_unavailable)?;
        let current = account_before("revalidate AccountsService", async {
            bus.get_name_owner(zbus::names::BusName::try_from("org.freedesktop.Accounts")?)
                .await
                .map_err(Into::into)
        })
        .await?;
        if current.as_str() != user.destination().as_str() {
            return Err(BrokerError::unavailable(
                "AccountsService changed before dispatch",
            ));
        }
        Ok(())
    }

    async fn apply(
        &self,
        user: &zbus::Proxy<'_>,
        change: &AccountChange,
        record: &PasswdRecord,
        icon: Option<&File>,
        authorized: &Authorized,
    ) -> Result<Value, BrokerError> {
        if user.destination() != self.accounts.destination() {
            return Err(BrokerError::authorization(
                "account proxy is not bound to the authenticated backend",
            ));
        }
        let value = match &change.value {
            AccountValue::Show => {
                let full_name: String =
                    account_before("read real name", user.get_property("RealName")).await?;
                let account_type: i32 =
                    account_before("read account type", user.get_property("AccountType")).await?;
                let icon_file: String =
                    account_before("read account icon", user.get_property("IconFile")).await?;
                let account_type = match account_type {
                    0 => "standard",
                    1 => "administrator",
                    _ => {
                        return Err(BrokerError::unavailable(
                            "AccountsService returned an unknown account type",
                        ))
                    }
                };
                json!({"full_name":full_name,"account_type":account_type,"icon_file":icon_file})
            }
            AccountValue::Name(name) => {
                // SetUserName preserves home/login semantics and moves only
                // AccountsService's cached metadata. Never call SetHomeDirectory.
                account_mutate(
                    authorized,
                    user.call::<_, _, ()>("SetUserName", &(name.as_str(),)),
                )
                .await?;
                let observed: String =
                    account_after("read login name", user.get_property("UserName")).await?;
                if observed != *name {
                    return Err(BrokerError::indeterminate(
                        "login rename readback differs; refresh before retrying",
                    ));
                }
                json!(observed)
            }
            AccountValue::FullName(name) => {
                // AccountsService preserves the other GECOS fields.
                account_mutate(
                    authorized,
                    user.call::<_, _, ()>("SetRealName", &(name.as_str(),)),
                )
                .await?;
                let observed: String =
                    account_after("read real name", user.get_property("RealName")).await?;
                if observed != *name {
                    return Err(BrokerError::indeterminate(
                        "real name readback differs; refresh before retrying",
                    ));
                }
                json!(observed)
            }
            AccountValue::Icon(path) => {
                let file =
                    icon.ok_or_else(|| BrokerError::execution("authorized icon is missing"))?;
                let source = format!("/proc/{}/fd/{}", std::process::id(), file.as_raw_fd());
                account_mutate(
                    authorized,
                    user.call::<_, _, ()>("SetIconFile", &(source.as_str(),)),
                )
                .await?;
                let observed: String =
                    account_after("read account icon", user.get_property("IconFile")).await?;
                if observed.is_empty() || !Path::new(&observed).is_absolute() {
                    return Err(BrokerError::indeterminate(
                        "account icon readback is missing; refresh before retrying",
                    ));
                }
                json!(path)
            }
            AccountValue::Type(value) => {
                // Preserve the distro's complete admin-group set, not just sudo.
                account_mutate(
                    authorized,
                    user.call::<_, _, ()>("SetAccountType", &(*value,)),
                )
                .await?;
                let observed: i32 =
                    account_after("read account type", user.get_property("AccountType")).await?;
                if observed != *value {
                    return Err(BrokerError::indeterminate(
                        "account type readback differs; refresh before retrying",
                    ));
                }
                json!(if observed == 1 {
                    "administrator"
                } else {
                    "standard"
                })
            }
        };
        if !matches!(change.value, AccountValue::Show) {
            let uid: u64 = account_after("revalidate target uid", user.get_property("Uid")).await?;
            let home: String =
                account_after("revalidate target home", user.get_property("HomeDirectory")).await?;
            if uid != u64::from(change.uid) || home != record.home {
                return Err(BrokerError::indeterminate("account identity/home changed; no home migration or rollback was requested; refresh before retrying"));
            }
        }
        Ok(json!({
            "action":change.action(),
            "status":if matches!(change.value, AccountValue::Show) { "observed" } else { "applied" },
            "user":change.user,"uid":change.uid,"value":value,
            "home":record.home,"rollback_supported":false,
        }))
    }
}

fn account_unavailable(error: impl std::fmt::Display) -> BrokerError {
    BrokerError::unavailable(format!(
        "account backend unavailable before mutation: {error}"
    ))
}

async fn account_before<T>(
    phase: &str,
    future: impl Future<Output = zbus::Result<T>>,
) -> Result<T, BrokerError> {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .map_err(|_| BrokerError::unavailable(format!("{phase} timed out before mutation")))?
        .map_err(account_unavailable)
}

async fn account_mutate<T>(
    _authorized: &Authorized,
    future: impl Future<Output = zbus::Result<T>>,
) -> Result<T, BrokerError> {
    match tokio::time::timeout(Duration::from_secs(5), future).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(zbus::Error::MethodError(name, _, _)))
            if matches!(
                name.as_str(),
                "org.freedesktop.Accounts.Error.PermissionDenied"
                    | "org.freedesktop.DBus.Error.AccessDenied"
            ) =>
        {
            Err(BrokerError::authorization(
                "account mutation denied by the OS backend",
            ))
        }
        Ok(Err(error)) => Err(BrokerError::indeterminate(format!(
            "account mutation was not confirmed: {error}; refresh before retrying"
        ))),
        Err(_) => Err(BrokerError::indeterminate(
            "account mutation timed out and may have applied; refresh before retrying",
        )),
    }
}

async fn account_after<T>(
    phase: &str,
    future: impl Future<Output = zbus::Result<T>>,
) -> Result<T, BrokerError> {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .map_err(|_| {
            BrokerError::indeterminate(format!(
                "{phase} timed out after mutation; refresh before retrying"
            ))
        })?
        .map_err(|error| {
            BrokerError::indeterminate(format!(
                "{phase} failed after mutation: {error}; refresh before retrying"
            ))
        })
}

fn requested_caps(action: &str, credential: Option<&(String, String)>) -> Vec<Cap> {
    if action == "status" {
        return vec![Cap::new(Verb::SYS_OBSERVE, Scope::name("identities"))];
    }
    let mut caps = vec![Cap::new(Verb::SYS_IDENTITY, Scope::name("manage"))];
    if let Some((namespace, name)) = credential {
        caps.push(Cap::new(
            Verb::SECRET_READ,
            Scope::name(format!("{namespace}/{name}")),
        ));
    }
    caps
}

/// Final provider check, taken against the decision the broker already
/// made.
///
/// The session, its App identity, the process allowed to act under it,
/// and the capabilities it holds all come from the grant `clawd`
/// issued and the middleware resolved. Nothing here re-reads the
/// process registry or re-derives policy, so the two can no longer
/// disagree; the check still runs, because a privileged mutation
/// should be refused twice.
fn authorize_session(
    authority: &Decision,
    requested: &[Cap],
) -> Result<(SessionInfo, Authorized), String> {
    let proof = authority.require_all(requested)?;
    Ok((authority.session()?.clone(), proof))
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
    _authorized: &Authorized,
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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/users.rs"
    ));
}
