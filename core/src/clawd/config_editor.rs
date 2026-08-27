use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::caps::{Cap, Scope, Verb};

use super::authority::{Authorized, Decision};
use super::client_identity::ClientIdentity;

const MAX_SOURCE_BYTES: u64 = 1024 * 1024;
const MAX_TARGET_BYTES: u64 = 4 * 1024 * 1024;
const VALIDATOR_TIMEOUT: Duration = Duration::from_secs(30);
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const STREAM_CAP_BYTES: usize = 1024 * 1024;
static CONFIG_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

#[derive(Clone, Serialize, Deserialize)]
struct ConfigBackup {
    token: String,
    target: String,
    owner_uid: u32,
    created_at: String,
    existed: bool,
    backup_file: Option<String>,
    previous_sha256: Option<String>,
    applied_sha256: String,
    validator: String,
    status: String,
}

struct TargetSnapshot {
    identity: Option<(u64, u64)>,
    sha256: Option<String>,
}

pub async fn control(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client, authority);
        return Err("Safe Config Editor requires Linux".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err("Safe Config Editor requires root clawd".to_string());
        }
        let uid = client.require_uid()?;
        let action = required_string(&params, "action")?;
        let target = optional_string(&params, "target")?;
        let source = optional_string(&params, "source")?;
        let token = optional_string(&params, "token")?;
        let confirm = params
            .get("confirm")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        validate_action(
            &action,
            target.as_deref(),
            source.as_deref(),
            token.as_deref(),
            confirm,
        )?;
        let target = resolve_target(target.as_deref().unwrap())?;
        let source = source.as_deref().map(resolve_source).transpose()?;
        let requested = requested_caps(&target, source.as_deref());
        let authorized = authorize_session(authority, &requested)?;

        match action.as_str() {
            "inspect" => inspect_target(&target),
            "validate" => {
                let content = read_source(source.as_deref().unwrap())?;
                validate_content(&target, &content).await
            }
            "apply" | "restore" => {
                let _guard = tokio::time::timeout(
                    LOCK_TIMEOUT,
                    CONFIG_LOCK
                        .get_or_init(|| tokio::sync::Mutex::new(()))
                        .lock(),
                )
                .await
                .map_err(|_| "Safe Config Editor is busy with another mutation".to_string())?;
                if action == "apply" {
                    let content = read_source(source.as_deref().unwrap())?;
                    apply_config(&authorized, &target, &content, uid).await
                } else {
                    restore_config(&authorized, &target, token.as_deref().unwrap(), uid).await
                }
            }
            _ => unreachable!("validated config action"),
        }
    }
}

fn requested_caps(target: &Path, source: Option<&Path>) -> Vec<Cap> {
    let mut caps = vec![Cap::new(
        Verb::SYS_CONFIG,
        Scope::path(target.to_string_lossy().into_owned()),
    )];
    if let Some(source) = source {
        caps.push(Cap::new(
            Verb::FS_READ,
            Scope::path(source.to_string_lossy().into_owned()),
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
fn authorize_session(authority: &Decision, requested: &[Cap]) -> Result<Authorized, String> {
    authority.require_app("config-editor")?;
    authority.require_all(requested)
}

fn inspect_target(target: &Path) -> Result<Value, String> {
    if !target.exists() {
        return Ok(json!({
            "target": target,
            "exists": false,
            "validator": validator_for(target)?.name(),
        }));
    }
    let (mut file, metadata) = open_regular_nofollow(target, MAX_TARGET_BYTES)?;
    let mut content = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut content)
        .map_err(|error| format!("read {}: {error}", target.display()))?;
    Ok(json!({
        "target": target,
        "exists": true,
        "size_bytes": metadata.len(),
        "uid": metadata.uid(),
        "gid": metadata.gid(),
        "mode": format!("{:04o}", metadata.mode() & 0o7777),
        "sha256": sha256(&content),
        "validator": validator_for(target)?.name(),
        "content": String::from_utf8_lossy(&content),
    }))
}

async fn validate_content(target: &Path, content: &[u8]) -> Result<Value, String> {
    let validator = validator_for(target)?;
    let mut temp = temp_for_target(target)?;
    temp.as_file_mut()
        .write_all(content)
        .and_then(|_| temp.as_file_mut().sync_all())
        .map_err(|error| format!("write validation file: {error}"))?;
    let result = validator.validate(temp.path(), content).await?;
    Ok(json!({
        "target": target,
        "validator": validator.name(),
        "valid": result.valid,
        "stdout": result.stdout,
        "stderr": result.stderr,
        "source_sha256": sha256(content),
    }))
}

async fn apply_config(
    _authorized: &Authorized,
    target: &Path,
    content: &[u8],
    owner_uid: u32,
) -> Result<Value, String> {
    let validator = validator_for(target)?;
    let parent = target
        .parent()
        .ok_or_else(|| "config target has no parent".to_string())?;
    let existed = target.exists();
    let (previous, mut original, original_identity) = if existed {
        let (mut file, metadata) = open_regular_nofollow(target, MAX_TARGET_BYTES)?;
        if metadata.nlink() != 1 {
            return Err("config target must be a regular file with one hard link".to_string());
        }
        let mut content = Vec::with_capacity(metadata.len() as usize);
        file.read_to_end(&mut content)
            .map_err(|error| format!("read existing config {}: {error}", target.display()))?;
        (
            Some(content),
            Some(file),
            Some((metadata.dev(), metadata.ino())),
        )
    } else {
        (None, None, None)
    };
    let mut replacement = temp_for_target(target)?;
    if existed {
        copy_preserving_file(
            original
                .as_mut()
                .expect("existing config must retain its open file"),
            replacement.path(),
        )
        .await?;
        ensure_target_unchanged(
            target,
            existed,
            original_identity,
            previous.as_deref(),
            original.as_mut(),
        )?;
        replacement
            .as_file_mut()
            .seek(SeekFrom::Start(0))
            .and_then(|_| replacement.as_file_mut().set_len(0))
            .map_err(|error| format!("prepare config replacement: {error}"))?;
    } else {
        fs::set_permissions(replacement.path(), fs::Permissions::from_mode(0o644))
            .map_err(|error| format!("set new config permissions: {error}"))?;
    }
    replacement
        .as_file_mut()
        .write_all(content)
        .and_then(|_| replacement.as_file_mut().sync_all())
        .map_err(|error| format!("write config replacement: {error}"))?;
    let validation = validator.validate(replacement.path(), content).await?;
    if !validation.valid {
        return Ok(json!({
            "target": target,
            "applied": false,
            "validator": validator.name(),
            "validation": validation,
        }));
    }

    ensure_target_unchanged(
        target,
        existed,
        original_identity,
        previous.as_deref(),
        original.as_mut(),
    )?;
    let backup = create_backup(
        target,
        owner_uid,
        existed,
        previous.as_deref(),
        original.as_mut(),
        content,
        validator.name(),
    )
    .await?;
    replacement
        .persist(target)
        .map_err(|error| format!("atomically replace {}: {}", target.display(), error.error))?;
    if !existed {
        if let Some(restorecon) =
            tool_path_optional(&["/usr/sbin/restorecon", "/usr/bin/restorecon"])
        {
            let output =
                match run_command(restorecon, &[path_str(target)?], VALIDATOR_TIMEOUT).await {
                    Ok(output) => output,
                    Err(error) => return auto_rollback_error(&backup, error).await,
                };
            if !output.status.success() {
                return auto_rollback_error(
                    &backup,
                    format!("restorecon failed: {}", tail(&output.stderr)),
                )
                .await;
            }
        }
    }
    if let Err(error) = sync_directory(parent) {
        return auto_rollback_error(&backup, error).await;
    }
    let post = validator.validate(target, content).await?;
    if !post.valid {
        let rollback = restore_from_backup(&backup, true, None).await;
        return match rollback {
            Ok(()) => {
                let mut rolled_back = backup.clone();
                rolled_back.status = "auto-rolled-back".to_string();
                save_backup_metadata(&rolled_back)?;
                Err(format!(
                    "post-write validation failed and the previous config was restored: {}",
                    post.stderr
                ))
            }
            Err(rollback_error) => Err(format!(
                "post-write validation failed ({}) and automatic rollback failed ({rollback_error}); backup token: {}",
                post.stderr, backup.token
            )),
        };
    }
    let mut applied = backup.clone();
    applied.status = "applied".to_string();
    if let Err(error) = save_backup_metadata(&applied) {
        return Ok(json!({
            "target": target,
            "applied": true,
            "action_applied": true,
            "backup_token": backup.token,
            "before_sha256": backup.previous_sha256,
            "after_sha256": backup.applied_sha256,
            "validator": validator.name(),
            "metadata_error": error,
        }));
    }
    Ok(json!({
        "target": target,
        "applied": true,
        "changed": backup.previous_sha256.as_deref() != Some(backup.applied_sha256.as_str()),
        "backup_token": backup.token,
        "before_sha256": backup.previous_sha256,
        "after_sha256": backup.applied_sha256,
        "validator": validator.name(),
        "validation": post,
    }))
}

async fn restore_config(
    _authorized: &Authorized,
    target: &Path,
    token: &str,
    owner_uid: u32,
) -> Result<Value, String> {
    validate_token(token)?;
    let mut backup = load_backup(token)?;
    if backup.owner_uid != owner_uid {
        return Err("config backup belongs to another user".to_string());
    }
    if Path::new(&backup.target) != target {
        return Err("config backup target does not match the requested path".to_string());
    }
    let snapshot = target_snapshot(target)?;
    if let Some(current_sha256) = snapshot.sha256.as_deref() {
        if current_sha256 != backup.applied_sha256 {
            return Err(
                "config target changed after apply; refusing to overwrite newer edits".to_string(),
            );
        }
    } else if !backup.existed {
        backup.status = "restored".to_string();
        save_backup_metadata(&backup)?;
        return Ok(json!({
            "target": target,
            "restored": true,
            "already_restored": true,
            "backup_token": token,
        }));
    }
    restore_from_backup(&backup, false, Some(&snapshot)).await?;
    backup.status = "restored".to_string();
    save_backup_metadata(&backup)?;
    Ok(json!({
        "target": target,
        "restored": true,
        "backup_token": token,
        "sha256": backup.previous_sha256,
    }))
}

async fn auto_rollback_error(backup: &ConfigBackup, error: String) -> Result<Value, String> {
    match restore_from_backup(backup, true, None).await {
        Ok(()) => {
            let mut rolled_back = backup.clone();
            rolled_back.status = "auto-rolled-back".to_string();
            save_backup_metadata(&rolled_back)?;
            Err(format!(
                "config apply failed and the previous config was restored: {error}"
            ))
        }
        Err(rollback_error) => Err(format!(
            "config apply failed ({error}) and automatic rollback failed ({rollback_error}); backup token: {}",
            backup.token
        )),
    }
}

async fn create_backup(
    target: &Path,
    owner_uid: u32,
    existed: bool,
    previous: Option<&[u8]>,
    original: Option<&mut File>,
    applied: &[u8],
    validator: &str,
) -> Result<ConfigBackup, String> {
    prepare_backup_dir()?;
    let token = uuid::Uuid::new_v4().simple().to_string();
    let backup_file = if existed {
        let path = backup_dir().join(format!("{token}.previous"));
        copy_preserving_file(
            original.ok_or_else(|| "open original config file is unavailable".to_string())?,
            &path,
        )
        .await?;
        let (backup_file, _) = open_regular_nofollow(&path, MAX_TARGET_BYTES)?;
        backup_file
            .sync_all()
            .map_err(|error| format!("fsync config backup {}: {error}", path.display()))?;
        sync_directory(&backup_dir())?;
        Some(path.to_string_lossy().into_owned())
    } else {
        None
    };
    let backup = ConfigBackup {
        token,
        target: target.to_string_lossy().into_owned(),
        owner_uid,
        created_at: chrono::Utc::now().to_rfc3339(),
        existed,
        backup_file,
        previous_sha256: previous.map(sha256),
        applied_sha256: sha256(applied),
        validator: validator.to_string(),
        status: "prepared".to_string(),
    };
    save_backup_metadata(&backup)?;
    Ok(backup)
}

async fn restore_from_backup(
    backup: &ConfigBackup,
    automatic: bool,
    expected_target: Option<&TargetSnapshot>,
) -> Result<(), String> {
    let target = Path::new(&backup.target);
    if backup.existed {
        let backup_file = backup
            .backup_file
            .as_deref()
            .ok_or_else(|| "config backup file is missing from metadata".to_string())?;
        let backup_file = Path::new(backup_file);
        if !backup_file.is_file() {
            return Err(format!(
                "config backup file is missing: {}",
                backup_file.display()
            ));
        }
        let (mut backup_source, backup_metadata) =
            open_regular_nofollow(backup_file, MAX_TARGET_BYTES)?;
        if backup_metadata.nlink() != 1 {
            return Err("config backup must have exactly one hard link".to_string());
        }
        let mut content = Vec::with_capacity(backup_metadata.len() as usize);
        backup_source
            .read_to_end(&mut content)
            .map_err(|error| format!("read config backup: {error}"))?;
        let expected = backup
            .previous_sha256
            .as_deref()
            .ok_or_else(|| "config backup has no previous hash".to_string())?;
        if sha256(&content) != expected {
            return Err("config backup content hash does not match its metadata".to_string());
        }
        let mut replacement = temp_for_target(target)?;
        copy_preserving_file(&mut backup_source, replacement.path()).await?;
        let validator = validator_for(target)?;
        let validation = validator.validate(replacement.path(), &content).await?;
        if !validation.valid {
            return Err(format!("backup no longer validates: {}", validation.stderr));
        }
        replacement
            .as_file_mut()
            .sync_all()
            .map_err(|error| format!("fsync config restore file: {error}"))?;
        if let Some(expected_target) = expected_target {
            ensure_snapshot_unchanged(target, expected_target)?;
        }
        replacement
            .persist(target)
            .map_err(|error| format!("restore {}: {}", target.display(), error.error))?;
    } else if target.exists() {
        if let Some(expected_target) = expected_target {
            ensure_snapshot_unchanged(target, expected_target)?;
        }
        fs::remove_file(target).map_err(|error| {
            format!("remove newly-created config {}: {error}", target.display())
        })?;
    }
    sync_directory(
        target
            .parent()
            .ok_or_else(|| "config target has no parent".to_string())?,
    )?;
    if automatic {
        tracing::warn!(target = %target.display(), "automatically restored invalid config");
    }
    Ok(())
}

fn target_snapshot(target: &Path) -> Result<TargetSnapshot, String> {
    match fs::symlink_metadata(target) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err("config target changed type after apply; refusing to overwrite it".to_string())
        }
        Ok(_) => {
            let (mut file, metadata) = open_regular_nofollow(target, MAX_TARGET_BYTES)?;
            let mut content = Vec::with_capacity(metadata.len() as usize);
            file.read_to_end(&mut content)
                .map_err(|error| format!("read current config {}: {error}", target.display()))?;
            Ok(TargetSnapshot {
                identity: Some((metadata.dev(), metadata.ino())),
                sha256: Some(sha256(&content)),
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(TargetSnapshot {
            identity: None,
            sha256: None,
        }),
        Err(error) => Err(format!(
            "inspect current config {}: {error}",
            target.display()
        )),
    }
}

fn ensure_snapshot_unchanged(target: &Path, snapshot: &TargetSnapshot) -> Result<(), String> {
    match snapshot.identity {
        None => match fs::symlink_metadata(target) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Ok(_) => Err("config target appeared during restore validation".to_string()),
            Err(error) => Err(format!(
                "recheck missing config target {}: {error}",
                target.display()
            )),
        },
        Some(identity) => {
            let path_metadata = fs::symlink_metadata(target)
                .map_err(|error| format!("recheck config target {}: {error}", target.display()))?;
            if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
                return Err("config target changed type during restore validation".to_string());
            }
            let (mut file, metadata) = open_regular_nofollow(target, MAX_TARGET_BYTES)?;
            if (metadata.dev(), metadata.ino()) != identity {
                return Err("config target changed identity during restore validation".to_string());
            }
            let mut content = Vec::with_capacity(metadata.len() as usize);
            file.read_to_end(&mut content)
                .map_err(|error| format!("re-read config target {}: {error}", target.display()))?;
            let current_sha256 = sha256(&content);
            if snapshot.sha256.as_deref() != Some(current_sha256.as_str()) {
                return Err("config target changed content during restore validation".to_string());
            }
            Ok(())
        }
    }
}

fn read_source(path: &Path) -> Result<Vec<u8>, String> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| format!("open config source {}: {error}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("inspect config source {}: {error}", path.display()))?;
    if !metadata.is_file() || metadata.len() > MAX_SOURCE_BYTES {
        return Err(format!(
            "config source must be a regular file no larger than {MAX_SOURCE_BYTES} bytes"
        ));
    }

    let mut content = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut content)
        .map_err(|error| format!("read config source {}: {error}", path.display()))?;
    Ok(content)
}

fn open_regular_nofollow(path: &Path, max_bytes: u64) -> Result<(File, fs::Metadata), String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| format!("open {}: {error}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("inspect {}: {error}", path.display()))?;
    if !metadata.is_file() || metadata.len() > max_bytes {
        return Err(format!(
            "{} must be a regular file no larger than {max_bytes} bytes",
            path.display()
        ));
    }
    Ok((file, metadata))
}

fn ensure_target_unchanged(
    target: &Path,
    existed: bool,
    original_identity: Option<(u64, u64)>,
    previous: Option<&[u8]>,
    original: Option<&mut File>,
) -> Result<(), String> {
    if !existed {
        return match fs::symlink_metadata(target) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Ok(_) => Err(
                "config target appeared during validation; refusing to overwrite it".to_string(),
            ),
            Err(error) => Err(format!(
                "recheck new config target {}: {error}",
                target.display()
            )),
        };
    }
    let metadata = fs::symlink_metadata(target)
        .map_err(|error| format!("recheck config target {}: {error}", target.display()))?;
    let expected =
        original_identity.ok_or_else(|| "original config identity is unavailable".to_string())?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || (metadata.dev(), metadata.ino()) != expected
    {
        return Err("config target changed identity during validation".to_string());
    }
    let original =
        original.ok_or_else(|| "open original config file is unavailable".to_string())?;
    original
        .seek(SeekFrom::Start(0))
        .map_err(|error| format!("rewind original config: {error}"))?;
    let mut current = Vec::new();
    original
        .read_to_end(&mut current)
        .map_err(|error| format!("re-read original config: {error}"))?;
    original
        .seek(SeekFrom::Start(0))
        .map_err(|error| format!("rewind original config after verification: {error}"))?;
    if previous
        .map(|previous| sha256(previous) != sha256(&current))
        .unwrap_or(true)
    {
        return Err("config target content changed during validation".to_string());
    }
    Ok(())
}

fn resolve_target(raw: &str) -> Result<PathBuf, String> {
    validate_absolute_path(raw, "config target")?;
    let raw_path = Path::new(raw);
    let target = if raw_path.exists() {
        let metadata = fs::symlink_metadata(raw_path)
            .map_err(|error| format!("inspect config target {raw:?}: {error}"))?;
        if metadata.file_type().is_symlink() {
            return Err(
                "config target symlinks are not allowed; use the canonical target".to_string(),
            );
        }
        fs::canonicalize(raw_path)
            .map_err(|error| format!("resolve config target {raw:?}: {error}"))?
    } else {
        let parent = raw_path
            .parent()
            .ok_or_else(|| "config target has no parent".to_string())?;
        let parent = fs::canonicalize(parent)
            .map_err(|error| format!("resolve config target parent: {error}"))?;
        parent.join(
            raw_path
                .file_name()
                .ok_or_else(|| "config target has no filename".to_string())?,
        )
    };
    if !target.starts_with("/etc") || target == Path::new("/etc") {
        return Err("Safe Config Editor only permits regular files below /etc".to_string());
    }
    validator_for(&target)?;
    Ok(target)
}

fn resolve_source(raw: &str) -> Result<PathBuf, String> {
    validate_absolute_path(raw, "config source")?;
    let metadata = fs::symlink_metadata(raw)
        .map_err(|error| format!("inspect config source {raw:?}: {error}"))?;
    if metadata.file_type().is_symlink() {
        return Err("config source symlinks are not allowed".to_string());
    }
    let path =
        fs::canonicalize(raw).map_err(|error| format!("resolve config source {raw:?}: {error}"))?;
    if !path.is_file() {
        return Err("config source must be a regular file".to_string());
    }
    Ok(path)
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

#[derive(Clone, Copy)]
enum Validator {
    Json,
    Sudoers,
    Sshd,
    Systemd,
    Fstab,
    Sysctl,
    Shell,
    Hosts,
    Hostname,
    ResolvConf,
}

impl Validator {
    fn name(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Sudoers => "visudo",
            Self::Sshd => "sshd",
            Self::Systemd => "systemd-analyze",
            Self::Fstab => "findmnt",
            Self::Sysctl => "sysctl-syntax",
            Self::Shell => "shell-syntax",
            Self::Hosts => "hosts-syntax",
            Self::Hostname => "hostname-syntax",
            Self::ResolvConf => "resolv-conf-syntax",
        }
    }

    async fn validate(self, path: &Path, content: &[u8]) -> Result<ValidationResult, String> {
        match self {
            Self::Json => match serde_json::from_slice::<Value>(content) {
                Ok(_) => Ok(ValidationResult::valid()),
                Err(error) => Ok(ValidationResult::invalid(error.to_string())),
            },
            Self::Sysctl => Ok(validate_sysctl(content)),
            Self::Hosts => Ok(validate_hosts(content)),
            Self::Hostname => Ok(validate_hostname(content)),
            Self::ResolvConf => Ok(validate_resolv_conf(content)),
            Self::Sudoers => {
                run_validator(
                    tool_path(&["/usr/sbin/visudo", "/usr/bin/visudo"], "visudo")?,
                    &["-c", "-f", path_str(path)?],
                )
                .await
            }
            Self::Sshd => {
                run_validator(
                    tool_path(&["/usr/sbin/sshd", "/usr/bin/sshd"], "sshd")?,
                    &["-t", "-f", path_str(path)?],
                )
                .await
            }
            Self::Systemd => {
                run_validator(
                    tool_path(
                        &["/usr/bin/systemd-analyze", "/bin/systemd-analyze"],
                        "systemd-analyze",
                    )?,
                    &["verify", path_str(path)?],
                )
                .await
            }
            Self::Fstab => {
                run_validator(
                    tool_path(&["/usr/bin/findmnt", "/bin/findmnt"], "findmnt")?,
                    &["--verify", "--tab-file", path_str(path)?],
                )
                .await
            }
            Self::Shell => {
                run_validator(
                    tool_path(&["/bin/sh", "/usr/bin/sh"], "sh")?,
                    &["-n", path_str(path)?],
                )
                .await
            }
        }
    }
}

fn validator_for(target: &Path) -> Result<Validator, String> {
    let path = target.to_string_lossy();
    let extension = target.extension().and_then(|value| value.to_str());
    if path == "/etc/sudoers" || path.starts_with("/etc/sudoers.d/") {
        Ok(Validator::Sudoers)
    } else if path == "/etc/ssh/sshd_config" || path.starts_with("/etc/ssh/sshd_config.d/") {
        Ok(Validator::Sshd)
    } else if path.starts_with("/etc/systemd/system/")
        && matches!(
            extension,
            Some("service" | "socket" | "timer" | "mount" | "target" | "path")
        )
    {
        Ok(Validator::Systemd)
    } else if path == "/etc/fstab" {
        Ok(Validator::Fstab)
    } else if path == "/etc/sysctl.conf" || path.starts_with("/etc/sysctl.d/") {
        Ok(Validator::Sysctl)
    } else if path == "/etc/hosts" {
        Ok(Validator::Hosts)
    } else if path == "/etc/hostname" {
        Ok(Validator::Hostname)
    } else if path == "/etc/resolv.conf" {
        Ok(Validator::ResolvConf)
    } else if extension == Some("json") {
        Ok(Validator::Json)
    } else if extension == Some("sh") {
        Ok(Validator::Shell)
    } else {
        Err(format!(
            "no safe validator is registered for {}",
            target.display()
        ))
    }
}

#[derive(Clone, Serialize)]
struct ValidationResult {
    valid: bool,
    stdout: String,
    stderr: String,
}

impl ValidationResult {
    fn valid() -> Self {
        Self {
            valid: true,
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    fn invalid(error: String) -> Self {
        Self {
            valid: false,
            stdout: String::new(),
            stderr: error,
        }
    }
}

fn validate_sysctl(content: &[u8]) -> ValidationResult {
    validate_text_lines(content, |line| {
        let Some((key, _)) = line.split_once('=') else {
            return false;
        };
        let key = key.trim().trim_start_matches('-');
        !key.is_empty()
            && key.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'/' | b'-')
            })
    })
}

fn validate_hosts(content: &[u8]) -> ValidationResult {
    validate_text_lines(content, |line| {
        let mut fields = line.split_whitespace();
        fields
            .next()
            .is_some_and(|address| address.parse::<std::net::IpAddr>().is_ok())
            && fields.next().is_some()
    })
}

fn validate_hostname(content: &[u8]) -> ValidationResult {
    let Ok(value) = std::str::from_utf8(content) else {
        return ValidationResult::invalid("hostname is not UTF-8".to_string());
    };
    let value = value.trim();
    if !value.is_empty()
        && value.len() <= 253
        && value.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    {
        ValidationResult::valid()
    } else {
        ValidationResult::invalid("invalid hostname".to_string())
    }
}

fn validate_resolv_conf(content: &[u8]) -> ValidationResult {
    validate_text_lines(content, |line| {
        matches!(
            line.split_whitespace().next(),
            Some("nameserver" | "search" | "domain" | "sortlist" | "options")
        )
    })
}

fn validate_text_lines(content: &[u8], validator: impl Fn(&str) -> bool) -> ValidationResult {
    let Ok(value) = std::str::from_utf8(content) else {
        return ValidationResult::invalid("config is not UTF-8".to_string());
    };
    for (index, line) in value.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if !validator(line) {
            return ValidationResult::invalid(format!("invalid syntax on line {}", index + 1));
        }
    }
    ValidationResult::valid()
}

async fn run_validator(program: &'static str, args: &[&str]) -> Result<ValidationResult, String> {
    let output = run_command(program, args, VALIDATOR_TIMEOUT).await?;
    Ok(ValidationResult {
        valid: output.status.success(),
        stdout: tail(&output.stdout),
        stderr: tail(&output.stderr),
    })
}

async fn copy_preserving_file(source: &mut File, destination: &Path) -> Result<(), String> {
    source
        .seek(SeekFrom::Start(0))
        .map_err(|error| format!("rewind config source file: {error}"))?;
    let source_path = format!("/proc/{}/fd/{}", std::process::id(), source.as_raw_fd());
    let cp = tool_path(&["/usr/bin/cp", "/bin/cp"], "cp")?;
    let output = run_command(
        cp,
        &[
            "--preserve=all",
            "--reflink=auto",
            "--dereference",
            "--",
            &source_path,
            path_str(destination)?,
        ],
        VALIDATOR_TIMEOUT,
    )
    .await?;
    if !output.status.success() {
        return Err(format!("cp failed: {}", tail(&output.stderr)));
    }
    source
        .seek(SeekFrom::Start(0))
        .map_err(|error| format!("rewind config source after copy: {error}"))?;
    Ok(())
}

fn temp_for_target(target: &Path) -> Result<tempfile::NamedTempFile, String> {
    let parent = target
        .parent()
        .ok_or_else(|| "config target has no parent".to_string())?;
    let suffix = target
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{value}"))
        .unwrap_or_default();
    tempfile::Builder::new()
        .prefix(".claw-config-")
        .suffix(&suffix)
        .tempfile_in(parent)
        .map_err(|error| format!("create config temporary file: {error}"))
}

fn prepare_backup_dir() -> Result<(), String> {
    let dir = backup_dir();
    fs::create_dir_all(&dir)
        .map_err(|error| format!("create config backup directory {}: {error}", dir.display()))?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("secure config backup directory {}: {error}", dir.display()))
}

fn backup_dir() -> PathBuf {
    crate::paths::data_dir()
        .join("clawd")
        .join("config-backups")
}

fn metadata_path(token: &str) -> PathBuf {
    backup_dir().join(format!("{token}.json"))
}

fn save_backup_metadata(backup: &ConfigBackup) -> Result<(), String> {
    let data = serde_json::to_vec_pretty(backup)
        .map_err(|error| format!("serialize config backup metadata: {error}"))?;
    crate::agent::util::atomic_write_with_fsync(&metadata_path(&backup.token), &data)
        .map_err(|error| format!("write config backup metadata: {error}"))
}

fn load_backup(token: &str) -> Result<ConfigBackup, String> {
    let data = fs::read(metadata_path(token))
        .map_err(|error| format!("read config backup metadata: {error}"))?;
    serde_json::from_slice(&data).map_err(|error| format!("parse config backup metadata: {error}"))
}

fn validate_token(token: &str) -> Result<(), String> {
    if token.len() == 32 && token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err("invalid config backup token".to_string())
    }
}

fn sync_directory(path: &Path) -> Result<(), String> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| format!("fsync directory {}: {error}", path.display()))
}

fn sha256(content: &[u8]) -> String {
    hex::encode(Sha256::digest(content))
}

fn validate_action(
    action: &str,
    target: Option<&str>,
    source: Option<&str>,
    token: Option<&str>,
    confirm: bool,
) -> Result<(), String> {
    match action {
        "inspect" if target.is_some() && source.is_none() && token.is_none() && !confirm => Ok(()),
        "validate" if target.is_some() && source.is_some() && token.is_none() && !confirm => Ok(()),
        "apply" if target.is_some() && source.is_some() && token.is_none() && confirm => Ok(()),
        "restore" if target.is_some() && source.is_none() && token.is_some() && confirm => Ok(()),
        "inspect" => Err("inspect requires only <target>".to_string()),
        "validate" => Err("validate requires <target> <source>".to_string()),
        "apply" => Err("apply requires <target> <source> --confirm".to_string()),
        "restore" => Err("restore requires <target> <backup-token> --confirm".to_string()),
        _ => Err(format!("unknown config action: {action}")),
    }
}

async fn run_command(
    program: &'static str,
    args: &[&str],
    timeout: Duration,
) -> Result<CommandOutput, String> {
    let args = args.iter().map(|value| value.to_string()).collect();
    tokio::task::spawn_blocking(move || run_command_sync(program, args, timeout))
        .await
        .map_err(|error| format!("{program} worker failed: {error}"))?
}

fn run_command_sync(
    program: &str,
    args: Vec<String>,
    timeout: Duration,
) -> Result<CommandOutput, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("HOME", "/root")
        .env("LC_ALL", "C.UTF-8")
        .env("SYSTEMD_PAGER", "cat")
        .env("PAGER", "cat")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
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
    Ok(CommandOutput {
        status,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        stdout_truncated,
        stderr_truncated,
    })
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
            .map_err(|error| format!("read config command output: {error}"))?;
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

fn tool_path(candidates: &[&'static str], name: &str) -> Result<&'static str, String> {
    tool_path_optional(candidates).ok_or_else(|| format!("{name} is not installed"))
}

fn tool_path_optional(candidates: &[&'static str]) -> Option<&'static str> {
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
        "/test/unit/clawd/config_editor.rs"
    ));
}
