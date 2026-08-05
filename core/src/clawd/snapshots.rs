use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use crate::caps::{Cap, CapSet, Scope, Verb};

use super::client_identity::ClientIdentity;

const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const LVM_MIN_SNAPSHOT_BYTES: u64 = 1024 * 1024 * 1024;
const LVM_MAX_SNAPSHOT_BYTES: u64 = 20 * 1024 * 1024 * 1024;
static SNAPSHOT_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Backend {
    Snapper,
    Btrfs,
    Lvm,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SnapshotRecord {
    id: String,
    backend: Backend,
    native_ref: String,
    description: String,
    created_at: String,
    status: String,
    rollback_supported: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rollback_boot_id: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SnapshotIndex {
    #[serde(default)]
    snapshots: Vec<SnapshotRecord>,
}

#[derive(Debug, Clone, Serialize)]
struct BackendStatus {
    backend: Option<Backend>,
    root_source: String,
    root_fstype: String,
    create_supported: bool,
    rollback_supported: bool,
    note: String,
}

pub async fn control(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client);
        return Err("system snapshots require Linux".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err("system snapshots require root clawd".to_string());
        }
        let uid = client.require_uid()?;
        let home = client.require_home_dir()?;
        let peer_pid = client
            .pid
            .ok_or_else(|| "clawd peer pid is unavailable".to_string())?;
        let session_id = required_string(&params, "session")?;
        let action = required_string(&params, "action")?;
        let verb = if matches!(action.as_str(), "status" | "list") {
            Verb::SYS_OBSERVE
        } else {
            Verb::SYS_SNAPSHOT
        };
        let scope = if verb == Verb::SYS_OBSERVE {
            Scope::name("system-snapshots")
        } else {
            Scope::Wild
        };
        crate::paths::with_user_override(uid, home, async {
            authorize_session(&session_id, peer_pid, verb, scope)
        })
        .await?;

        let _guard = SNAPSHOT_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        match action.as_str() {
            "status" => {
                let index = load_reconciled_index().await?;
                Ok(json!({
                    "status": detect_backend().await?,
                    "managed_snapshots": index.snapshots.len(),
                }))
            }
            "list" => {
                let index = load_reconciled_index().await?;
                Ok(json!({"snapshots": index.snapshots, "count": index.snapshots.len()}))
            }
            "create" => {
                let description = sanitize_description(
                    params
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("Claw OS recovery point"),
                )?;
                let record = create_snapshot(&description).await?;
                if let Err(index_error) = append_record(record.clone()) {
                    let cleanup = delete_snapshot(&record).await;
                    return Err(match cleanup {
                        Ok(()) => format!(
                            "snapshot was created but index persistence failed; backend snapshot was removed: {index_error}"
                        ),
                        Err(cleanup_error) => format!(
                            "snapshot was created but index persistence failed ({index_error}); backend cleanup also failed ({cleanup_error}); native reference: {}",
                            record.native_ref
                        ),
                    });
                }
                Ok(json!({"created": record}))
            }
            "delete" => {
                let id = required_string(&params, "id")?;
                validate_snapshot_id(&id)?;
                let mut record = find_record_reconciled(&id).await?;
                if record.status == "rollback-scheduled" {
                    return Err(format!(
                        "snapshot {} has a rollback scheduled for the current boot; reboot before deleting it",
                        record.id
                    ));
                }
                let previous_status = record.status.clone();
                record.status = "delete-pending".to_string();
                replace_record(record.clone())?;
                if let Err(error) = delete_snapshot(&record).await {
                    record.status = previous_status;
                    let _ = replace_record(record);
                    return Err(error);
                }
                remove_record(&id)?;
                Ok(json!({"deleted": id, "backend": record.backend}))
            }
            "rollback" => {
                let id = required_string(&params, "id")?;
                validate_snapshot_id(&id)?;
                if !params
                    .get("confirm")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    return Err("system snapshot rollback requires confirm=true".to_string());
                }
                let mut record = find_record_reconciled(&id).await?;
                if !record.rollback_supported {
                    return Err(format!(
                        "snapshot {} uses a backend that cannot safely roll back a live root filesystem",
                        record.id
                    ));
                }
                let previous = record.clone();
                record.status = "rollback-scheduled".to_string();
                record.rollback_boot_id = current_boot_id();
                replace_record(record.clone())?;
                if let Err(error) = rollback_snapshot(&record).await {
                    let _ = replace_record(previous);
                    return Err(error);
                }
                Ok(json!({
                    "rollback_scheduled": id,
                    "backend": record.backend,
                    "requires_reboot": true,
                }))
            }
            other => Err(format!(
                "unknown system snapshot action `{other}`; expected status, list, create, delete, or rollback"
            )),
        }
    }
}

async fn create_snapshot(description: &str) -> Result<SnapshotRecord, String> {
    let status = detect_backend().await?;
    let backend = status
        .backend
        .ok_or_else(|| format!("no supported snapshot backend: {}", status.note))?;
    let id = format!("snap_{}", uuid::Uuid::new_v4().simple());
    let (native_ref, rollback_supported) = match backend {
        Backend::Snapper => {
            let output = run(
                snapper_path(),
                &[
                    "--no-dbus",
                    "-c",
                    "root",
                    "create",
                    "--type",
                    "single",
                    "--cleanup-algorithm",
                    "number",
                    "--description",
                    description,
                    "--print-number",
                ],
            )
            .await?;
            let number = output
                .stdout
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .map(str::trim)
                .filter(|line| line.bytes().all(|byte| byte.is_ascii_digit()))
                .ok_or_else(|| "snapper did not return a snapshot number".to_string())?;
            (number.to_string(), true)
        }
        Backend::Btrfs => {
            let destination = snapshots_dir().join(&id);
            crate::storage::ensure_private_dir(&snapshots_dir())
                .map_err(|error| format!("create snapshot directory: {error}"))?;
            run(
                btrfs_path(),
                &[
                    "subvolume",
                    "snapshot",
                    "-r",
                    "/",
                    destination
                        .to_str()
                        .ok_or_else(|| "snapshot path is not UTF-8".to_string())?,
                ],
            )
            .await?;
            (destination.to_string_lossy().into_owned(), false)
        }
        Backend::Lvm => {
            let source = status.root_source;
            let vg = run(lvs_path(), &["--noheadings", "-o", "vg_name", &source])
                .await?
                .stdout
                .trim()
                .to_string();
            if vg.is_empty() || !safe_lvm_name(&vg) {
                return Err("could not determine a safe LVM volume group".to_string());
            }
            let free = run(
                vgs_path(),
                &[
                    "--noheadings",
                    "--units",
                    "b",
                    "--nosuffix",
                    "-o",
                    "vg_free",
                    &vg,
                ],
            )
            .await?
            .stdout
            .trim()
            .trim_start_matches('<')
            .parse::<f64>()
            .map_err(|error| format!("parse LVM free bytes: {error}"))?
                as u64;
            let size = (free / 5).min(LVM_MAX_SNAPSHOT_BYTES);
            if size < LVM_MIN_SNAPSHOT_BYTES {
                return Err(format!(
                    "LVM volume group `{vg}` has insufficient free space for a safe snapshot"
                ));
            }
            let name = format!("cos_{}", &id[5..17]);
            run(
                lvcreate_path(),
                &[
                    "--snapshot",
                    "--name",
                    &name,
                    "--size",
                    &format!("{size}B"),
                    &source,
                ],
            )
            .await?;
            (format!("/dev/{vg}/{name}"), true)
        }
    };
    Ok(SnapshotRecord {
        id,
        backend,
        native_ref,
        description: description.to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        status: "ready".to_string(),
        rollback_supported,
        rollback_boot_id: None,
    })
}

async fn delete_snapshot(record: &SnapshotRecord) -> Result<(), String> {
    match record.backend {
        Backend::Snapper => {
            if !snapper_snapshot_exists(&record.native_ref).await? {
                return Ok(());
            }
            run(
                snapper_path(),
                &["--no-dbus", "-c", "root", "delete", &record.native_ref],
            )
            .await?;
        }
        Backend::Btrfs => {
            if !Path::new(&record.native_ref).exists() {
                return Ok(());
            }
            ensure_snapshot_path(&record.native_ref)?;
            run(btrfs_path(), &["subvolume", "delete", &record.native_ref]).await?;
        }
        Backend::Lvm => {
            ensure_lvm_snapshot_path(&record.native_ref)?;
            if !lvm_snapshot_exists(&record.native_ref).await? {
                return Ok(());
            }
            run(lvremove_path(), &["--yes", &record.native_ref]).await?;
        }
    }
    Ok(())
}

async fn rollback_snapshot(record: &SnapshotRecord) -> Result<(), String> {
    match record.backend {
        Backend::Snapper => {
            run(
                snapper_path(),
                &["--no-dbus", "-c", "root", "rollback", &record.native_ref],
            )
            .await?;
        }
        Backend::Lvm => {
            ensure_lvm_snapshot_path(&record.native_ref)?;
            run(lvconvert_path(), &["--merge", &record.native_ref]).await?;
        }
        Backend::Btrfs => {
            return Err(
                "direct Btrfs snapshots require a bootloader-aware restore workflow".to_string(),
            );
        }
    }
    Ok(())
}

async fn detect_backend() -> Result<BackendStatus, String> {
    let findmnt = run(findmnt_path(), &["-n", "-o", "SOURCE,FSTYPE", "/"]).await?;
    let mut fields = findmnt.stdout.split_whitespace();
    let source = fields.next().unwrap_or_default().to_string();
    let fstype = fields.next().unwrap_or_default().to_string();
    if executable(snapper_path()) && Path::new("/etc/snapper/configs/root").is_file() {
        return Ok(BackendStatus {
            backend: Some(Backend::Snapper),
            root_source: source,
            root_fstype: fstype,
            create_supported: true,
            rollback_supported: true,
            note: "Snapper root configuration detected.".to_string(),
        });
    }
    if fstype == "btrfs" && executable(btrfs_path()) {
        return Ok(BackendStatus {
            backend: Some(Backend::Btrfs),
            root_source: source,
            root_fstype: fstype,
            create_supported: true,
            rollback_supported: false,
            note:
                "Direct read-only Btrfs snapshots are supported; live root rollback is not enabled."
                    .to_string(),
        });
    }
    if source.starts_with("/dev/") && executable(lvs_path()) && executable(lvcreate_path()) {
        let lvm = run_allow_failure(lvs_path(), &["--noheadings", &source]).await?;
        if lvm.status == 0 {
            return Ok(BackendStatus {
                backend: Some(Backend::Lvm),
                root_source: source,
                root_fstype: fstype,
                create_supported: true,
                rollback_supported: true,
                note: "LVM root logical volume detected.".to_string(),
            });
        }
    }
    Ok(BackendStatus {
        backend: None,
        root_source: source,
        root_fstype: fstype,
        create_supported: false,
        rollback_supported: false,
        note: "Install/configure Snapper, use Btrfs, or place root on LVM.".to_string(),
    })
}

fn authorize_session(
    session_id: &str,
    peer_pid: u32,
    verb: Verb,
    scope: Scope,
) -> Result<(), String> {
    let session = crate::proc::session_info_by_id(session_id)
        .ok_or_else(|| format!("snapshot session not found: {session_id}"))?;
    if session.app_id.as_deref() != Some("system-snapshot") {
        return Err("system snapshots are restricted to the system-snapshot App".to_string());
    }
    if session.pending_bind || session.pid == 0 {
        return Err("snapshot session is not bound to a process".to_string());
    }
    let expected_start = session
        .start_time_ticks
        .ok_or_else(|| "snapshot session has no process identity".to_string())?;
    if crate::proc::read_start_time_ticks_pub(session.pid) != Some(expected_start) {
        return Err("snapshot session process identity is stale".to_string());
    }
    if !crate::proc::process_descends_from(peer_pid, session.pid) {
        return Err("snapshot request did not originate from the authorized session".to_string());
    }
    let mut caps = session.caps.unwrap_or_else(CapSet::new);
    if let Some(transient) = session.transient_caps {
        caps.extend(transient.iter().cloned());
    }
    if !caps.covers(&Cap::new(verb, scope)) {
        return Err(format!("snapshot session lacks {}", verb.as_str()));
    }
    Ok(())
}

fn load_index() -> Result<SnapshotIndex, String> {
    match crate::filelock::read_locked(&index_path())? {
        Some(data) => {
            serde_json::from_str(&data).map_err(|error| format!("parse snapshot index: {error}"))
        }
        None => Ok(SnapshotIndex::default()),
    }
}

async fn load_reconciled_index() -> Result<SnapshotIndex, String> {
    let mut index = load_index()?;
    let boot_id = current_boot_id();
    let mut changed = false;
    let mut retained = Vec::with_capacity(index.snapshots.len());
    for mut record in index.snapshots {
        if record.status == "rollback-scheduled" {
            let rebooted = match (&record.rollback_boot_id, &boot_id) {
                (Some(previous), Some(current)) => previous != current,
                _ => false,
            };
            let lvm_merged = matches!(record.backend, Backend::Lvm)
                && !lvm_snapshot_exists(&record.native_ref).await?;
            if rebooted || lvm_merged {
                record.status = "rollback-completed".to_string();
                changed = true;
            }
        }
        if record.status == "delete-pending" && !backend_snapshot_exists(&record).await? {
            changed = true;
            continue;
        }
        retained.push(record);
    }
    index.snapshots = retained;
    if changed {
        save_index(&index)?;
    }
    Ok(index)
}

fn save_index(index: &SnapshotIndex) -> Result<(), String> {
    crate::storage::ensure_private_dir(&snapshot_root())
        .map_err(|error| format!("create snapshot state directory: {error}"))?;
    let data = serde_json::to_string_pretty(index)
        .map_err(|error| format!("serialize snapshot index: {error}"))?;
    crate::filelock::write_locked(&index_path(), &data)
}

fn append_record(record: SnapshotRecord) -> Result<(), String> {
    let mut index = load_index()?;
    index.snapshots.push(record);
    save_index(&index)
}

fn replace_record(record: SnapshotRecord) -> Result<(), String> {
    let mut index = load_index()?;
    let existing = index
        .snapshots
        .iter_mut()
        .find(|item| item.id == record.id)
        .ok_or_else(|| format!("snapshot record not found: {}", record.id))?;
    *existing = record;
    save_index(&index)
}

fn remove_record(id: &str) -> Result<(), String> {
    let mut index = load_index()?;
    let before = index.snapshots.len();
    index.snapshots.retain(|record| record.id != id);
    if index.snapshots.len() == before {
        return Err(format!("snapshot record not found: {id}"));
    }
    save_index(&index)
}

fn find_record(id: &str) -> Result<SnapshotRecord, String> {
    load_index()?
        .snapshots
        .into_iter()
        .find(|record| record.id == id)
        .ok_or_else(|| format!("snapshot record not found: {id}"))
}

async fn find_record_reconciled(id: &str) -> Result<SnapshotRecord, String> {
    load_reconciled_index()
        .await?
        .snapshots
        .into_iter()
        .find(|record| record.id == id)
        .ok_or_else(|| format!("snapshot record not found: {id}"))
}

fn snapshot_root() -> PathBuf {
    crate::paths::data_dir().join("system-snapshots")
}

fn snapshots_dir() -> PathBuf {
    snapshot_root().join("snapshots")
}

fn index_path() -> PathBuf {
    snapshot_root().join("index.json")
}

fn sanitize_description(description: &str) -> Result<String, String> {
    let value = description
        .chars()
        .filter(|character| !character.is_control())
        .take(200)
        .collect::<String>();
    if value.trim().is_empty() {
        return Err("snapshot description must not be empty".to_string());
    }
    Ok(value)
}

fn validate_snapshot_id(id: &str) -> Result<(), String> {
    if id.len() != 37
        || !id.starts_with("snap_")
        || !id[5..].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(format!("invalid snapshot id: {id:?}"));
    }
    Ok(())
}

fn ensure_snapshot_path(path: &str) -> Result<(), String> {
    let root = snapshots_dir()
        .canonicalize()
        .map_err(|error| format!("canonicalize snapshot root: {error}"))?;
    let path = Path::new(path)
        .canonicalize()
        .map_err(|error| format!("canonicalize snapshot path: {error}"))?;
    if path.parent() != Some(root.as_path()) {
        return Err("snapshot path escapes the managed snapshot directory".to_string());
    }
    Ok(())
}

fn ensure_lvm_snapshot_path(path: &str) -> Result<(), String> {
    if !path.starts_with("/dev/") || path.contains("..") || path.split('/').count() != 4 {
        return Err("invalid managed LVM snapshot path".to_string());
    }
    Ok(())
}

fn safe_lvm_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

async fn backend_snapshot_exists(record: &SnapshotRecord) -> Result<bool, String> {
    match record.backend {
        Backend::Snapper => snapper_snapshot_exists(&record.native_ref).await,
        Backend::Btrfs => Ok(Path::new(&record.native_ref).exists()),
        Backend::Lvm => lvm_snapshot_exists(&record.native_ref).await,
    }
}

async fn snapper_snapshot_exists(native_ref: &str) -> Result<bool, String> {
    let output = run(
        snapper_path(),
        &["--no-dbus", "-c", "root", "list", "--columns", "number"],
    )
    .await?;
    Ok(output
        .stdout
        .lines()
        .flat_map(str::split_whitespace)
        .any(|field| field == native_ref))
}

async fn lvm_snapshot_exists(path: &str) -> Result<bool, String> {
    ensure_lvm_snapshot_path(path)?;
    let output = run_allow_failure(lvs_path(), &["--noheadings", path]).await?;
    Ok(output.status == 0)
}

fn current_boot_id() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

struct CommandOutput {
    status: i32,
    stdout: String,
    stderr_tail: String,
}

async fn run(path: &str, args: &[&str]) -> Result<CommandOutput, String> {
    let output = run_allow_failure(path, args).await?;
    if output.status != 0 {
        return Err(format!(
            "{} {} exited {}: {}",
            path,
            args.join(" "),
            output.status,
            output.stderr_tail,
        ));
    }
    Ok(output)
}

async fn run_allow_failure(path: &str, args: &[&str]) -> Result<CommandOutput, String> {
    let mut command = tokio::process::Command::new(path);
    command
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("LC_ALL", "C.UTF-8")
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = tokio::time::timeout(SNAPSHOT_TIMEOUT, command.output())
        .await
        .map_err(|_| format!("{} timed out after {}s", path, SNAPSHOT_TIMEOUT.as_secs()))?
        .map_err(|error| format!("failed to launch {path}: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let status = output.status.code().unwrap_or(-1);
    if status != 0 && !stderr.trim().is_empty() {
        tracing::debug!(
            command = path,
            args = %args.join(" "),
            status,
            stderr = %tail(&stderr),
            "snapshot backend command returned non-zero"
        );
    }
    Ok(CommandOutput {
        status,
        stdout,
        stderr_tail: tail(&stderr),
    })
}

fn tail(value: &str) -> String {
    const MAX: usize = 8 * 1024;
    let start = value.len().saturating_sub(MAX);
    value.get(start..).unwrap_or(value).trim().to_string()
}

fn executable(path: &str) -> bool {
    Path::new(path).is_file()
}

fn required_string(params: &Value, key: &str) -> Result<String, String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("missing required string parameter: {key}"))
}

fn findmnt_path() -> &'static str {
    "/usr/bin/findmnt"
}

fn snapper_path() -> &'static str {
    "/usr/bin/snapper"
}

fn btrfs_path() -> &'static str {
    "/usr/bin/btrfs"
}

fn lvs_path() -> &'static str {
    "/usr/sbin/lvs"
}

fn vgs_path() -> &'static str {
    "/usr/sbin/vgs"
}

fn lvcreate_path() -> &'static str {
    "/usr/sbin/lvcreate"
}

fn lvremove_path() -> &'static str {
    "/usr/sbin/lvremove"
}

fn lvconvert_path() -> &'static str {
    "/usr/sbin/lvconvert"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_ids_are_strict() {
        validate_snapshot_id("snap_0123456789abcdef0123456789abcdef").unwrap();
        assert!(validate_snapshot_id("../snapshot").is_err());
        assert!(validate_snapshot_id("snap_not-hex").is_err());
    }

    #[test]
    fn descriptions_strip_controls_and_limit_size() {
        let value = sanitize_description(&format!("hello\n{}", "x".repeat(300))).unwrap();
        assert!(!value.contains('\n'));
        assert_eq!(value.chars().count(), 200);
    }

    #[test]
    fn lvm_paths_and_names_are_restricted() {
        ensure_lvm_snapshot_path("/dev/vg/cos_123").unwrap();
        assert!(ensure_lvm_snapshot_path("/tmp/x").is_err());
        assert!(ensure_lvm_snapshot_path("/dev/vg/../root").is_err());
        assert!(safe_lvm_name("ubuntu-vg"));
        assert!(!safe_lvm_name("../vg"));
    }
}
