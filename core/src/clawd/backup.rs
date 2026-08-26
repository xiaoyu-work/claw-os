use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::caps::{Cap, CapSet, Scope, Verb};
use crate::proc::SessionInfo;

use super::client_identity::ClientIdentity;

const QUERY_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const BACKUP_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const STREAM_CAP_BYTES: usize = 2 * 1024 * 1024;
static BACKUP_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

pub async fn control(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client);
        return Err("Backup Center requires Linux".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err("Backup Center requires root clawd".to_string());
        }
        let uid = client.require_uid()?;
        let gid = client
            .gid
            .ok_or_else(|| "clawd peer gid is unavailable".to_string())?;
        let home = client.require_home_dir()?;
        let peer_pid = client
            .pid
            .ok_or_else(|| "clawd peer pid is unavailable".to_string())?;
        let session_id = required_string(&params, "session")?;
        let action = required_string(&params, "action")?;
        let repo = required_string(&params, "repo")?;
        let source = optional_string(&params, "source")?;
        let destination = optional_string(&params, "destination")?;
        let snapshot = optional_string(&params, "snapshot")?;
        let credential = required_string(&params, "credential")?;
        let tag = optional_string(&params, "tag")?;
        let keep_daily = optional_u64(&params, "keep_daily")?;
        let keep_weekly = optional_u64(&params, "keep_weekly")?;
        let keep_monthly = optional_u64(&params, "keep_monthly")?;
        let confirm = params
            .get("confirm")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        validate_action(
            &action,
            source.as_deref(),
            destination.as_deref(),
            snapshot.as_deref(),
            tag.as_deref(),
            keep_daily,
            keep_weekly,
            keep_monthly,
            confirm,
        )?;
        let repo = resolve_repo(&repo, action == "init")?;
        let source = source.as_deref().map(resolve_existing_path).transpose()?;
        let destination = destination
            .as_deref()
            .map(resolve_destination)
            .transpose()?;
        if let Some(source) = source.as_deref() {
            if repo.starts_with(source) || source.starts_with(&repo) {
                return Err("backup source and repository must not contain one another".to_string());
            }
        }
        if let Some(destination) = destination.as_deref() {
            if repo.starts_with(destination) || destination.starts_with(&repo) {
                return Err(
                    "restore destination and repository must not contain one another".to_string(),
                );
            }
        }
        let credential = parse_credential_ref(&credential)?;
        let requested = requested_caps(
            &repo,
            source.as_deref(),
            destination.as_deref(),
            &credential,
        );
        let session = crate::paths::with_user_override(uid, home.clone(), async {
            authorize_session(&session_id, peer_pid, &requested)
        })
        .await?;
        let user = UserEnvironment::new(uid, gid, home)?;
        let password = crate::paths::with_user_override(uid, user.home.clone(), async {
            crate::credential::load_for_broker(
                &credential.1,
                &credential.0,
                session.tier.unwrap_or(u8::MAX),
            )
        })
        .await?;
        validate_password(&password)?;
        let mount = mounted_destination(&repo)?;

        let _guard = tokio::time::timeout(
            LOCK_TIMEOUT,
            BACKUP_LOCK
                .get_or_init(|| tokio::sync::Mutex::new(()))
                .lock(),
        )
        .await
        .map_err(|_| "Backup Center is busy with another repository operation".to_string())?;
        let mut password_file = PasswordFile::new(&user, &password)?;
        let before_mount = mounted_destination(&repo)?;
        if !before_mount.same_identity(&mount) {
            return Err("backup destination mount changed before operation".to_string());
        }
        let result = run_action(
            &action,
            &repo,
            source.as_deref(),
            destination.as_deref(),
            snapshot.as_deref(),
            tag.as_deref(),
            keep_daily,
            keep_weekly,
            keep_monthly,
            &user,
            password_file.path(),
        )
        .await;
        password_file.close();
        let result = result?;
        let after_mount = match mounted_destination(&repo) {
            Ok(after_mount) => after_mount,
            Err(error) => {
                return Ok(json!({
                    "action": action,
                    "action_applied": true,
                    "result": result,
                    "before_mount": before_mount,
                    "post_mount_error": error,
                }));
            }
        };
        if !after_mount.same_identity(&before_mount) {
            return Ok(json!({
                "action": action,
                "action_applied": true,
                "result": result,
                "mount_changed": true,
                "before_mount": before_mount,
                "after_mount": after_mount,
            }));
        }
        Ok(json!({
            "action": action,
            "repository": repo,
            "mount": after_mount,
            "result": result,
        }))
    }
}

fn requested_caps(
    repo: &Path,
    source: Option<&Path>,
    destination: Option<&Path>,
    credential: &(String, String),
) -> Vec<Cap> {
    let mut caps = vec![
        Cap::new(
            Verb::DATA_BACKUP,
            Scope::path(repo.to_string_lossy().into_owned()),
        ),
        Cap::new(
            Verb::SECRET_READ,
            Scope::name(format!("{}/{}", credential.0, credential.1)),
        ),
    ];
    if let Some(source) = source {
        caps.push(Cap::new(
            Verb::DATA_BACKUP,
            Scope::path(source.to_string_lossy().into_owned()),
        ));
    }
    if let Some(destination) = destination {
        caps.push(Cap::new(
            Verb::DATA_BACKUP,
            Scope::path(destination.to_string_lossy().into_owned()),
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
        .ok_or_else(|| format!("backup-center session not found: {session_id}"))?;
    if session.app_id.as_deref() != Some("backup-center") {
        return Err("backup operations are restricted to the backup-center App".to_string());
    }
    if session.pending_bind || session.pid == 0 {
        return Err("backup-center session is not bound to a process".to_string());
    }
    let expected_start = session
        .start_time_ticks
        .ok_or_else(|| "backup-center session has no process identity".to_string())?;
    if crate::proc::read_start_time_ticks_pub(session.pid) != Some(expected_start) {
        return Err("backup-center session process identity is stale".to_string());
    }
    if !crate::proc::process_descends_from(peer_pid, session.pid) {
        return Err("backup request did not originate from the authorized session".to_string());
    }
    let mut caps = session.caps.clone().unwrap_or_else(CapSet::new);
    if let Some(transient) = &session.transient_caps {
        caps.extend(transient.iter().cloned());
    }
    for cap in requested {
        if !caps.covers(cap) {
            return Err(format!(
                "backup-center session lacks {}:{}",
                cap.verb.as_str(),
                cap.scope
            ));
        }
    }
    Ok(session)
}

async fn run_action(
    action: &str,
    repo: &Path,
    source: Option<&Path>,
    destination: Option<&Path>,
    snapshot: Option<&str>,
    tag: Option<&str>,
    keep_daily: Option<u64>,
    keep_weekly: Option<u64>,
    keep_monthly: Option<u64>,
    user: &UserEnvironment,
    password_file: &Path,
) -> Result<Value, String> {
    match action {
        "init" => {
            let output = run_restic(repo, &["init"], user, password_file, QUERY_TIMEOUT).await?;
            Ok(command_result(output))
        }
        "snapshots" => {
            ensure_restic_repo(repo)?;
            let output = run_restic(
                repo,
                &["snapshots", "--json"],
                user,
                password_file,
                QUERY_TIMEOUT,
            )
            .await?;
            let snapshots = serde_json::from_str::<Value>(&output.stdout)
                .map_err(|error| format!("parse restic snapshots JSON: {error}"))?;
            Ok(json!({"snapshots": snapshots}))
        }
        "check" => {
            ensure_restic_repo(repo)?;
            let output = run_restic(
                repo,
                &["check", "--json"],
                user,
                password_file,
                BACKUP_TIMEOUT,
            )
            .await?;
            Ok(command_result(output))
        }
        "backup" => {
            ensure_restic_repo(repo)?;
            let source = source.expect("validated backup source");
            let mut args = vec![
                "backup".to_string(),
                source.to_string_lossy().into_owned(),
                "--json".to_string(),
            ];
            if let Some(tag) = tag {
                args.push("--tag".to_string());
                args.push(tag.to_string());
            }
            let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
            let output = run_restic(repo, &refs, user, password_file, BACKUP_TIMEOUT).await?;
            Ok(json!({
                "source": source,
                "events": parse_json_lines(&output.stdout),
                "stderr_tail": tail(&output.stderr),
            }))
        }
        "restore" => {
            ensure_restic_repo(repo)?;
            let snapshot = snapshot.expect("validated backup snapshot");
            let destination = destination.expect("validated restore destination");
            ensure_empty_destination(destination)?;
            let output = run_restic(
                repo,
                &["restore", snapshot, "--target", path_str(destination)?],
                user,
                password_file,
                BACKUP_TIMEOUT,
            )
            .await?;
            Ok(json!({
                "snapshot": snapshot,
                "destination": destination,
                "restored": true,
                "stdout_tail": tail(&output.stdout),
                "stderr_tail": tail(&output.stderr),
            }))
        }
        "forget" => {
            ensure_restic_repo(repo)?;
            let snapshot = snapshot.expect("validated backup snapshot");
            let output = run_restic(
                repo,
                &["forget", snapshot],
                user,
                password_file,
                QUERY_TIMEOUT,
            )
            .await?;
            Ok(json!({
                "snapshot": snapshot,
                "forgotten": true,
                "stdout_tail": tail(&output.stdout),
                "stderr_tail": tail(&output.stderr),
            }))
        }
        "retention" => {
            ensure_restic_repo(repo)?;
            let daily = keep_daily.unwrap().to_string();
            let weekly = keep_weekly.unwrap().to_string();
            let monthly = keep_monthly.unwrap().to_string();
            let output = run_restic(
                repo,
                &[
                    "forget",
                    "--keep-daily",
                    &daily,
                    "--keep-weekly",
                    &weekly,
                    "--keep-monthly",
                    &monthly,
                    "--prune",
                ],
                user,
                password_file,
                BACKUP_TIMEOUT,
            )
            .await?;
            Ok(json!({
                "retention": {
                    "daily": keep_daily,
                    "weekly": keep_weekly,
                    "monthly": keep_monthly,
                },
                "applied": true,
                "stdout_tail": tail(&output.stdout),
                "stderr_tail": tail(&output.stderr),
            }))
        }
        _ => unreachable!("validated backup action"),
    }
}

fn command_result(output: CommandOutput) -> Value {
    json!({
        "stdout": output.stdout,
        "stderr": output.stderr,
        "stdout_truncated": output.stdout_truncated,
        "stderr_truncated": output.stderr_truncated,
    })
}

fn parse_json_lines(output: &str) -> Vec<Value> {
    output
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect()
}

fn ensure_restic_repo(repo: &Path) -> Result<(), String> {
    if repo.join("config").is_file()
        && repo.join("data").is_dir()
        && repo.join("snapshots").is_dir()
    {
        Ok(())
    } else {
        Err(format!(
            "{} is not an initialized Restic repository",
            repo.display()
        ))
    }
}

fn ensure_empty_destination(destination: &Path) -> Result<(), String> {
    if !destination.exists() {
        let parent = destination
            .parent()
            .ok_or_else(|| "restore destination has no parent".to_string())?;
        if !parent.is_dir() {
            return Err("restore destination parent is not a directory".to_string());
        }
        return Ok(());
    }
    if !destination.is_dir() {
        return Err("restore destination must be a directory".to_string());
    }
    if fs::read_dir(destination)
        .map_err(|error| format!("inspect restore destination: {error}"))?
        .next()
        .is_some()
    {
        return Err("restore destination must be empty".to_string());
    }
    Ok(())
}

#[derive(Clone, serde::Serialize)]
struct MountInfo {
    id: u64,
    parent_id: u64,
    major_minor: String,
    root: String,
    mountpoint: String,
    options: String,
    filesystem: String,
    source: String,
    super_options: String,
}

impl MountInfo {
    fn same_identity(&self, other: &Self) -> bool {
        self.id == other.id
            && self.major_minor == other.major_minor
            && self.root == other.root
            && self.mountpoint == other.mountpoint
            && self.filesystem == other.filesystem
            && self.source == other.source
    }
}

fn mounted_destination(repo: &Path) -> Result<MountInfo, String> {
    let repo = if repo.exists() {
        fs::canonicalize(repo).map_err(|error| format!("resolve repository: {error}"))?
    } else {
        repo.to_path_buf()
    };
    let data = fs::read_to_string("/proc/self/mountinfo")
        .map_err(|error| format!("read mountinfo: {error}"))?;
    let mut best = None::<MountInfo>;
    for line in data.lines() {
        let Some(info) = parse_mountinfo(line) else {
            continue;
        };
        let mountpoint = Path::new(&info.mountpoint);
        if repo.starts_with(mountpoint)
            && best
                .as_ref()
                .map(|current| info.mountpoint.len() > current.mountpoint.len())
                .unwrap_or(true)
        {
            best = Some(info);
        }
    }
    let info = best.ok_or_else(|| format!("no mount contains {}", repo.display()))?;
    if info.mountpoint == "/" {
        return Err(
            "backup repository must be on an explicitly mounted non-root filesystem".to_string(),
        );
    }
    Ok(info)
}

fn parse_mountinfo(line: &str) -> Option<MountInfo> {
    let (left, right) = line.split_once(" - ")?;
    let left = left.split_whitespace().collect::<Vec<_>>();
    let right = right.split_whitespace().collect::<Vec<_>>();
    if left.len() < 6 || right.len() < 3 {
        return None;
    }
    Some(MountInfo {
        id: left[0].parse().ok()?,
        parent_id: left[1].parse().ok()?,
        major_minor: left[2].to_string(),
        root: unescape_mount(left[3]),
        mountpoint: unescape_mount(left[4]),
        options: left[5].to_string(),
        filesystem: right[0].to_string(),
        source: unescape_mount(right[1]),
        super_options: right[2].to_string(),
    })
}

fn unescape_mount(value: &str) -> String {
    value
        .replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

fn resolve_repo(raw: &str, allow_missing: bool) -> Result<PathBuf, String> {
    validate_absolute_path(raw, "repository")?;
    let path = Path::new(raw);
    if path.exists() {
        let metadata =
            fs::symlink_metadata(path).map_err(|error| format!("inspect repository: {error}"))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err("repository must be a canonical directory, not a symlink".to_string());
        }
        fs::canonicalize(path).map_err(|error| format!("resolve repository: {error}"))
    } else if allow_missing {
        let parent = fs::canonicalize(
            path.parent()
                .ok_or_else(|| "repository has no parent".to_string())?,
        )
        .map_err(|error| format!("resolve repository parent: {error}"))?;
        Ok(parent.join(
            path.file_name()
                .ok_or_else(|| "repository has no name".to_string())?,
        ))
    } else {
        Err(format!("repository does not exist: {raw}"))
    }
}

fn resolve_existing_path(raw: &str) -> Result<PathBuf, String> {
    validate_absolute_path(raw, "backup source")?;
    let metadata =
        fs::symlink_metadata(raw).map_err(|error| format!("inspect backup source: {error}"))?;
    if metadata.file_type().is_symlink() {
        return Err("backup source symlinks are not allowed".to_string());
    }
    fs::canonicalize(raw).map_err(|error| format!("resolve backup source: {error}"))
}

fn resolve_destination(raw: &str) -> Result<PathBuf, String> {
    validate_absolute_path(raw, "restore destination")?;
    let path = Path::new(raw);
    if path.exists() {
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| format!("inspect restore destination: {error}"))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err("restore destination must be a canonical directory".to_string());
        }
        fs::canonicalize(path).map_err(|error| format!("resolve restore destination: {error}"))
    } else {
        let parent = fs::canonicalize(
            path.parent()
                .ok_or_else(|| "restore destination has no parent".to_string())?,
        )
        .map_err(|error| format!("resolve restore destination parent: {error}"))?;
        Ok(parent.join(
            path.file_name()
                .ok_or_else(|| "restore destination has no name".to_string())?,
        ))
    }
}

fn validate_absolute_path(value: &str, kind: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 4096
        || !value.starts_with('/')
        || value.chars().any(|character| character.is_control())
        || Path::new(value)
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(format!("invalid {kind}: {value:?}"));
    }
    Ok(())
}

fn validate_action(
    action: &str,
    source: Option<&str>,
    destination: Option<&str>,
    snapshot: Option<&str>,
    tag: Option<&str>,
    keep_daily: Option<u64>,
    keep_weekly: Option<u64>,
    keep_monthly: Option<u64>,
    confirm: bool,
) -> Result<(), String> {
    match action {
        "init" | "snapshots" | "check"
            if source.is_none()
                && destination.is_none()
                && snapshot.is_none()
                && tag.is_none()
                && keep_daily.is_none()
                && keep_weekly.is_none()
                && keep_monthly.is_none()
                && !confirm =>
        {
            Ok(())
        }
        "backup"
            if source.is_some()
                && destination.is_none()
                && snapshot.is_none()
                && tag.map(valid_tag).unwrap_or(true)
                && keep_daily.is_none()
                && keep_weekly.is_none()
                && keep_monthly.is_none()
                && !confirm =>
        {
            Ok(())
        }
        "restore"
            if source.is_none()
                && destination.is_some()
                && snapshot.is_some_and(valid_snapshot)
                && tag.is_none()
                && keep_daily.is_none()
                && keep_weekly.is_none()
                && keep_monthly.is_none()
                && confirm =>
        {
            Ok(())
        }
        "forget"
            if source.is_none()
                && destination.is_none()
                && snapshot.is_some_and(valid_snapshot_id)
                && tag.is_none()
                && keep_daily.is_none()
                && keep_weekly.is_none()
                && keep_monthly.is_none()
                && confirm =>
        {
            Ok(())
        }
        "retention"
            if source.is_none()
                && destination.is_none()
                && snapshot.is_none()
                && tag.is_none()
                && keep_daily.is_some_and(|value| value <= 365)
                && keep_weekly.is_some_and(|value| value <= 260)
                && keep_monthly.is_some_and(|value| value <= 120)
                && confirm =>
        {
            Ok(())
        }
        _ => Err(format!("invalid arguments for backup action {action:?}")),
    }
}

fn valid_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 128
        && tag
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn valid_snapshot(value: &str) -> bool {
    value == "latest" || valid_snapshot_id(value)
}

fn valid_snapshot_id(value: &str) -> bool {
    (8..=64).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn parse_credential_ref(value: &str) -> Result<(String, String), String> {
    let (namespace, name) = value
        .split_once('/')
        .ok_or_else(|| "credential must use namespace/name form".to_string())?;
    validate_name("credential namespace", namespace)?;
    validate_name("credential name", name)?;
    Ok((namespace.to_string(), name.to_string()))
}

fn validate_name(kind: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 255
        || value.starts_with('-')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
    {
        return Err(format!("invalid {kind}: {value:?}"));
    }
    Ok(())
}

fn validate_password(password: &str) -> Result<(), String> {
    if password.is_empty()
        || password.len() > 1024
        || password.chars().any(|character| character.is_control())
    {
        return Err("backup credential must be a non-empty single-line secret".to_string());
    }
    Ok(())
}

struct PasswordFile {
    file: Option<tempfile::NamedTempFile>,
    path: PathBuf,
}

impl PasswordFile {
    fn new(user: &UserEnvironment, password: &str) -> Result<Self, String> {
        user.validate_runtime()?;
        let mut file = tempfile::Builder::new()
            .prefix(".claw-restic-password-")
            .tempfile_in(&user.runtime_dir)
            .map_err(|error| format!("create Restic password file: {error}"))?;
        writeln!(file, "{password}")
            .and_then(|_| file.flush())
            .map_err(|error| format!("write Restic password file: {error}"))?;
        fs::set_permissions(file.path(), fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("secure Restic password file: {error}"))?;
        chown(file.path(), user.uid, user.gid)?;
        let path = file.path().to_path_buf();
        Ok(Self {
            file: Some(file),
            path,
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn close(&mut self) {
        self.file.take();
    }
}

fn chown(path: &Path, uid: u32, gid: u32) -> Result<(), String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| "password file path contains NUL".to_string())?;
    if unsafe { libc::chown(path.as_ptr(), uid, gid) } != 0 {
        return Err(format!(
            "chown Restic password file: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[derive(Clone)]
struct UserEnvironment {
    uid: u32,
    gid: u32,
    home: PathBuf,
    runtime_dir: PathBuf,
    username: String,
}

impl UserEnvironment {
    fn new(uid: u32, gid: u32, home: PathBuf) -> Result<Self, String> {
        let metadata = fs::metadata(&home)
            .map_err(|error| format!("inspect backup user home {}: {error}", home.display()))?;
        if metadata.uid() != uid {
            return Err(format!(
                "backup user home {} belongs to uid {}, expected {uid}",
                home.display(),
                metadata.uid()
            ));
        }
        Ok(Self {
            uid,
            gid,
            home,
            runtime_dir: PathBuf::from(format!("/run/user/{uid}")),
            username: username_for_uid(uid)?,
        })
    }

    fn validate_runtime(&self) -> Result<(), String> {
        let metadata = fs::symlink_metadata(&self.runtime_dir)
            .map_err(|error| format!("inspect backup runtime: {error}"))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() || metadata.uid() != self.uid {
            return Err("backup runtime directory is not user-owned".to_string());
        }
        Ok(())
    }
}

fn username_for_uid(uid: u32) -> Result<String, String> {
    use std::ffi::CStr;
    const BUF_SIZE: usize = 16 * 1024;
    let mut buffer = vec![0 as libc::c_char; BUF_SIZE];
    let mut passwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result = std::ptr::null_mut();
    let rc = unsafe {
        libc::getpwuid_r(
            uid,
            &mut passwd,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() || passwd.pw_name.is_null() {
        return Err(format!("passwd entry is unavailable for uid {uid}"));
    }
    unsafe { CStr::from_ptr(passwd.pw_name) }
        .to_str()
        .map(str::to_string)
        .map_err(|_| format!("username is not UTF-8 for uid {uid}"))
}

async fn run_restic(
    repo: &Path,
    args: &[&str],
    user: &UserEnvironment,
    password_file: &Path,
    timeout: Duration,
) -> Result<CommandOutput, String> {
    let restic = tool_path(&["/usr/bin/restic", "/usr/local/bin/restic"])
        .ok_or_else(|| "restic is not installed".to_string())?;
    let mut owned = vec![
        "--repo".to_string(),
        path_str(repo)?.to_string(),
        "--password-file".to_string(),
        path_str(password_file)?.to_string(),
    ];
    owned.extend(args.iter().map(|value| value.to_string()));
    run_command(restic, owned, user.clone(), timeout).await
}

async fn run_command(
    program: &'static str,
    args: Vec<String>,
    user: UserEnvironment,
    timeout: Duration,
) -> Result<CommandOutput, String> {
    tokio::task::spawn_blocking(move || run_command_sync(program, args, user, timeout))
        .await
        .map_err(|error| format!("{program} worker failed: {error}"))?
}

fn run_command_sync(
    program: &str,
    args: Vec<String>,
    user: UserEnvironment,
    timeout: Duration,
) -> Result<CommandOutput, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("HOME", &user.home)
        .env("USER", &user.username)
        .env("LOGNAME", &user.username)
        .env("XDG_RUNTIME_DIR", &user.runtime_dir)
        .env("LC_ALL", "C.UTF-8")
        .current_dir(&user.home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let uid = user.uid;
    let gid = user.gid;
    unsafe {
        command.pre_exec(move || {
            if libc::setgroups(0, std::ptr::null()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::setgid(gid) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::setuid(uid) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::setrlimit(libc::RLIMIT_CORE as _, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to launch {program}: {error}"))?;
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
                std::thread::sleep(Duration::from_millis(50));
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
    stdout_truncated: bool,
    stderr_truncated: bool,
}

fn read_bounded(mut reader: impl std::io::Read) -> Result<(Vec<u8>, bool), String> {
    let mut kept = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    let mut truncated = false;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("read backup command output: {error}"))?;
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

fn optional_u64(params: &Value, key: &str) -> Result<Option<u64>, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(value)) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("parameter `{key}` must be a non-negative integer")),
        Some(Value::String(value)) => value
            .parse::<u64>()
            .map(Some)
            .map_err(|_| format!("parameter `{key}` must be an integer")),
        Some(_) => Err(format!("parameter `{key}` must be an integer or null")),
    }
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

fn tool_path(candidates: &[&'static str]) -> Option<&'static str> {
    candidates
        .iter()
        .copied()
        .find(|candidate| Path::new(candidate).is_file())
}

fn path_str(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| format!("path is not UTF-8: {}", path.display()))
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
        "/test/unit/clawd/backup.rs"
    ));
}
