use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::caps::{Cap, Scope, Verb};

use super::authority::{Authorized, Decision};
use super::client_identity::ClientIdentity;

const TOOL_TIMEOUT: Duration = Duration::from_secs(60);
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const STREAM_CAP_BYTES: usize = 2 * 1024 * 1024;
const POLICY_PATH: &str = "/etc/udev/rules.d/99-claw-usb-guard.rules";
static USB_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

#[derive(Clone, Serialize, Deserialize)]
struct UsbGuardState {
    schema: u32,
    revision: String,
    rules: Vec<UsbBlockRule>,
}

impl Default for UsbGuardState {
    fn default() -> Self {
        Self {
            schema: 1,
            revision: "initial".to_string(),
            rules: Vec::new(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct UsbBlockRule {
    id: String,
    vendor_id: String,
    product_id: String,
    serial: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct UsbGuardBackup {
    token: String,
    owner_uid: u32,
    created_at: String,
    applied_revision: String,
    previous: UsbGuardState,
    #[serde(default)]
    authorizations: Vec<UsbAuthorizationSnapshot>,
    status: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct UsbAuthorizationSnapshot {
    sysfs_name: String,
    vendor_id: String,
    product_id: String,
    serial: Option<String>,
    authorized: bool,
}

#[derive(Clone)]
struct UsbDevice {
    sysfs_name: String,
    path: PathBuf,
    vendor_id: String,
    product_id: String,
    serial: Option<String>,
    manufacturer: Option<String>,
    product: Option<String>,
    device_class: String,
    authorized: bool,
    removable: Option<bool>,
    block_devices: Vec<PathBuf>,
}

pub async fn reconcile_on_start() -> Result<(), String> {
    #[cfg(not(target_os = "linux"))]
    {
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        if !state_path().exists() && !Path::new(POLICY_PATH).exists() {
            return Ok(());
        }
        apply_policy(&load_state()?).await
    }
}

pub async fn control(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client, authority);
        return Err("USB Guard requires Linux sysfs".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err("USB Guard requires root clawd".to_string());
        }
        let uid = client.require_uid()?;
        let action = required_string(&params, "action")?;
        let device = optional_string(&params, "device")?;
        let state = optional_string(&params, "state")?;
        let rule_id = optional_string(&params, "rule_id")?;
        let token = optional_string(&params, "token")?;
        let confirm = params
            .get("confirm")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        validate_action(
            &action,
            device.as_deref(),
            state.as_deref(),
            rule_id.as_deref(),
            token.as_deref(),
            confirm,
        )?;
        let requested = if action == "status" {
            Cap::new(Verb::SYS_OBSERVE, Scope::name("usb"))
        } else {
            Cap::new(Verb::DEVICE_USB, Scope::name("control"))
        };
        let _authorized = authorize_session(authority, requested)?;

        let _guard = tokio::time::timeout(
            LOCK_TIMEOUT,
            USB_LOCK.get_or_init(|| tokio::sync::Mutex::new(())).lock(),
        )
        .await
        .map_err(|_| "USB Guard is busy with another mutation".to_string())?;
        if action == "status" {
            return usb_status();
        }
        match action.as_str() {
            "authorize" => set_authorized(device.as_deref().unwrap(), state.as_deref().unwrap()),
            "block" => block_device(uid, device.as_deref().unwrap()).await,
            "unblock" => unblock_device(uid, rule_id.as_deref().unwrap()).await,
            "eject" => eject_device(device.as_deref().unwrap()).await,
            "restore" => restore_rules(uid, token.as_deref().unwrap()).await,
            _ => unreachable!("validated USB action"),
        }
    }
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
fn authorize_session(authority: &Decision, requested: Cap) -> Result<Authorized, String> {
    authority.require_app("usb-guard")?;
    authority.require_all(std::slice::from_ref(&requested))
}

fn usb_status() -> Result<Value, String> {
    let state = load_state()?;
    let devices = usb_devices()?;
    let values = devices
        .into_iter()
        .map(|device| {
            let matching_rules = state
                .rules
                .iter()
                .filter(|rule| rule_matches(rule, &device))
                .map(|rule| rule.id.clone())
                .collect::<Vec<_>>();
            device_value(&device, matching_rules)
        })
        .collect::<Vec<_>>();
    let count = values.len();
    let policy_in_sync = policy_matches_state(&state)?;
    Ok(json!({
        "devices": values,
        "count": count,
        "persistent_rules": state.rules,
        "revision": state.revision,
        "policy_path": POLICY_PATH,
        "policy_in_sync": policy_in_sync,
    }))
}

fn device_value(device: &UsbDevice, matching_rules: Vec<String>) -> Value {
    json!({
        "sysfs_name": device.sysfs_name,
        "vendor_id": device.vendor_id,
        "product_id": device.product_id,
        "serial": device.serial,
        "manufacturer": device.manufacturer,
        "product": device.product,
        "device_class": device.device_class,
        "authorized": device.authorized,
        "removable": device.removable,
        "block_devices": device.block_devices,
        "persistent_block_rules": matching_rules,
    })
}

fn set_authorized(device_name: &str, state: &str) -> Result<Value, String> {
    let device = require_device(device_name)?;
    if state == "off" {
        reject_hub(&device)?;
        reject_protected_storage(&device)?;
    } else if load_state()?
        .rules
        .iter()
        .any(|rule| rule_matches(rule, &device))
    {
        return Err(
            "device has a persistent block rule; remove that rule before authorizing it"
                .to_string(),
        );
    }
    let before = device.authorized;
    let after = state == "on";
    write_device_authorized(&device, after)?;
    Ok(json!({
        "action": "authorize",
        "device": device_name,
        "changed": before != after,
        "before": before,
        "after": after,
    }))
}

async fn block_device(owner_uid: u32, device_name: &str) -> Result<Value, String> {
    let device = require_device(device_name)?;
    reject_hub(&device)?;
    reject_protected_storage(&device)?;
    let serial = device
        .serial
        .as_deref()
        .ok_or_else(|| "persistent USB blocking requires a device serial".to_string())?;
    validate_serial(serial)?;
    let previous = load_state()?;
    if previous
        .rules
        .iter()
        .any(|rule| rule_matches(rule, &device))
    {
        return Err("this USB device already has a persistent block rule".to_string());
    }
    let matching_devices = usb_devices()?
        .into_iter()
        .filter(|candidate| {
            candidate.vendor_id.eq_ignore_ascii_case(&device.vendor_id)
                && candidate
                    .product_id
                    .eq_ignore_ascii_case(&device.product_id)
                && candidate.serial.as_deref() == Some(serial)
        })
        .count();
    if matching_devices != 1 {
        return Err("persistent USB fingerprint is not unique among connected devices".to_string());
    }
    let mut next = previous.clone();
    let rule = UsbBlockRule {
        id: uuid::Uuid::new_v4().simple().to_string(),
        vendor_id: device.vendor_id.clone(),
        product_id: device.product_id.clone(),
        serial: serial.to_string(),
    };
    next.revision = uuid::Uuid::new_v4().simple().to_string();
    next.rules.push(rule.clone());
    let backup = create_backup(
        owner_uid,
        previous.clone(),
        &next.revision,
        vec![authorization_snapshot(&device)],
    )?;
    apply_policy_change(&next, &previous, "apply USB block policy").await?;
    if let Err(error) = save_state(&next) {
        let policy_rollback = apply_policy(&previous).await;
        return Err(format!(
            "USB block state persistence failed ({error}); policy rollback: {}; backup token: {}",
            result_summary(policy_rollback),
            backup.token
        ));
    }
    if let Err(error) = write_device_authorized(&device, false) {
        let policy_rollback = apply_policy(&previous).await;
        let state_rollback = save_state(&previous);
        let authorization_rollback = write_device_authorized(&device, device.authorized);
        return Err(format!(
            "persistent USB rule was applied but device deauthorization failed ({error}); policy rollback: {}; state rollback: {}; authorization rollback: {}; backup token: {}",
            result_summary(policy_rollback),
            result_summary(state_rollback),
            result_summary(authorization_rollback),
            backup.token
        ));
    }
    let mut applied = backup.clone();
    applied.status = "applied".to_string();
    if let Err(error) = save_backup(&applied) {
        let policy_rollback = apply_policy(&previous).await;
        let state_rollback = save_state(&previous);
        let authorization_rollback = write_device_authorized(&device, device.authorized);
        return Err(format!(
            "USB block backup finalization failed ({error}); policy rollback: {}; state rollback: {}; authorization rollback: {}; backup token: {}",
            result_summary(policy_rollback),
            result_summary(state_rollback),
            result_summary(authorization_rollback),
            backup.token
        ));
    }
    let mut blocked_device = device.clone();
    blocked_device.authorized = false;
    Ok(json!({
        "blocked": true,
        "device": device_value(&blocked_device, vec![rule.id.clone()]),
        "rule": rule,
        "backup_token": backup.token,
        "revision": next.revision,
    }))
}

async fn unblock_device(owner_uid: u32, rule_id: &str) -> Result<Value, String> {
    validate_token(rule_id)?;
    let previous = load_state()?;
    let mut next = previous.clone();
    let index = next
        .rules
        .iter()
        .position(|rule| rule.id == rule_id)
        .ok_or_else(|| format!("USB block rule not found: {rule_id}"))?;
    let removed = next.rules.remove(index);
    next.revision = uuid::Uuid::new_v4().simple().to_string();
    let matches = usb_devices()?
        .into_iter()
        .filter(|device| rule_matches(&removed, device))
        .collect::<Vec<_>>();
    let authorization_snapshots = match matches.as_slice() {
        [device] => vec![authorization_snapshot(device)],
        _ => Vec::new(),
    };
    let backup = create_backup(
        owner_uid,
        previous.clone(),
        &next.revision,
        authorization_snapshots,
    )?;
    apply_policy_change(&next, &previous, "apply USB unblock policy").await?;
    if let Err(error) = save_state(&next) {
        let policy_rollback = apply_policy(&previous).await;
        return Err(format!(
            "USB unblock state persistence failed ({error}); policy rollback: {}; backup token: {}",
            result_summary(policy_rollback),
            backup.token
        ));
    }
    let mut applied = backup.clone();
    applied.status = "applied".to_string();
    if let Err(error) = save_backup(&applied) {
        let policy_rollback = apply_policy(&previous).await;
        let state_rollback = save_state(&previous);
        return Err(format!(
            "USB unblock backup finalization failed ({error}); policy rollback: {}; state rollback: {}; backup token: {}",
            result_summary(policy_rollback),
            result_summary(state_rollback),
            backup.token
        ));
    }
    let authorization_status = match matches.as_slice() {
        [] => "not-connected",
        [device] => {
            if let Err(error) = write_device_authorized(device, true) {
                let policy_rollback = apply_policy(&previous).await;
                let state_rollback = save_state(&previous);
                let authorization_rollback = write_device_authorized(device, device.authorized);
                let mut rolled_back = applied;
                rolled_back.status = "rollback-attempted".to_string();
                let backup_rollback = save_backup(&rolled_back);
                return Err(format!(
                    "USB unblock authorization failed ({error}); policy rollback: {}; state rollback: {}; authorization rollback: {}; backup record: {}; backup token: {}",
                    result_summary(policy_rollback),
                    result_summary(state_rollback),
                    result_summary(authorization_rollback),
                    result_summary(backup_rollback),
                    backup.token
                ));
            }
            "authorized"
        }
        _ => "ambiguous-fingerprint",
    };
    Ok(json!({
        "unblocked": true,
        "rule": removed,
        "current_device_authorization": authorization_status,
        "backup_token": backup.token,
        "revision": next.revision,
    }))
}

async fn restore_rules(owner_uid: u32, token: &str) -> Result<Value, String> {
    validate_token(token)?;
    let mut backup = load_backup(token)?;
    if backup.owner_uid != owner_uid {
        return Err("USB Guard backup belongs to another user".to_string());
    }
    let current = load_state()?;
    if current.revision != backup.applied_revision {
        return Err("USB Guard state changed after this backup was created".to_string());
    }
    let (authorization_targets, disconnected_authorizations) =
        prepare_authorization_restore(&backup.authorizations)?;
    for (device, desired) in &authorization_targets {
        if !desired {
            reject_hub(device)?;
            reject_protected_storage(device)?;
        }
    }
    apply_policy_change(&backup.previous, &current, "restore USB Guard policy").await?;
    if let Err(error) = save_state(&backup.previous) {
        let rollback = apply_policy(&current).await;
        return Err(format!(
            "USB Guard restore state persistence failed ({error}); policy rollback: {}",
            result_summary(rollback)
        ));
    }
    let authorization_results = match apply_authorization_targets(&authorization_targets) {
        Ok(results) => results,
        Err(error) => {
            let policy_rollback = apply_policy(&current).await;
            let state_rollback = save_state(&current);
            let authorization_rollback = rollback_authorization_targets(&authorization_targets);
            return Err(format!(
                "USB Guard authorization restore failed ({error}); policy rollback: {}; state rollback: {}; authorization rollback: {}",
                result_summary(policy_rollback),
                result_summary(state_rollback),
                result_summary(authorization_rollback)
            ));
        }
    };
    backup.status = "restored".to_string();
    if let Err(error) = save_backup(&backup) {
        let policy_rollback = apply_policy(&current).await;
        let state_rollback = save_state(&current);
        let authorization_rollback = rollback_authorization_targets(&authorization_targets);
        return Err(format!(
            "USB Guard restore backup finalization failed ({error}); policy rollback: {}; state rollback: {}; authorization rollback: {}",
            result_summary(policy_rollback),
            result_summary(state_rollback),
            result_summary(authorization_rollback)
        ));
    }
    Ok(json!({
        "restored": true,
        "backup_token": token,
        "revision": backup.previous.revision,
        "rules": backup.previous.rules,
        "authorization_results": authorization_results,
        "disconnected_authorizations": disconnected_authorizations,
    }))
}

async fn eject_device(device_name: &str) -> Result<Value, String> {
    let device = require_device(device_name)?;
    reject_hub(&device)?;
    reject_protected_storage(&device)?;
    let mut disks = device
        .block_devices
        .iter()
        .filter(|path| !is_partition(path))
        .cloned()
        .collect::<Vec<_>>();
    disks.sort();
    disks.dedup();
    if disks.is_empty() {
        return Err("USB device has no whole block disk to eject".to_string());
    }
    let current = require_same_device(&device)?;
    let disk = current
        .block_devices
        .iter()
        .find(|path| !is_partition(path) && disks.contains(path))
        .ok_or_else(|| "USB block-device mapping changed before eject".to_string())?;
    let output = run_checked(
        udisksctl_path()?,
        &[
            "power-off",
            "--block-device",
            path_str(disk)?,
            "--no-user-interaction",
        ],
        TOOL_TIMEOUT,
    )
    .await?;
    Ok(json!({
        "ejected": true,
        "usb_device": device_name,
        "block_devices": disks,
        "requested_via": disk,
        "stdout_tail": tail(&output.stdout),
        "stderr_tail": tail(&output.stderr),
    }))
}

fn usb_devices() -> Result<Vec<UsbDevice>, String> {
    let root = Path::new("/sys/bus/usb/devices");
    let entries = fs::read_dir(root).map_err(|error| format!("list USB devices: {error}"))?;
    let block_map = usb_block_map()?;
    let mut devices = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("read USB device entry: {error}"))?;
        let Some(sysfs_name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !valid_device_name(&sysfs_name) {
            continue;
        }
        let path = fs::canonicalize(entry.path())
            .map_err(|error| format!("resolve USB device {sysfs_name}: {error}"))?;
        let vendor_id = required_sysfs_value(&path, "idVendor")?;
        let product_id = required_sysfs_value(&path, "idProduct")?;
        let authorized = required_sysfs_value(&path, "authorized")?;
        if !matches!(authorized.as_str(), "0" | "1") {
            return Err(format!(
                "USB device {sysfs_name} has invalid authorized state {authorized:?}"
            ));
        }
        devices.push(UsbDevice {
            sysfs_name,
            path: path.clone(),
            vendor_id,
            product_id,
            serial: read_trim(path.join("serial")),
            manufacturer: read_trim(path.join("manufacturer")),
            product: read_trim(path.join("product")),
            device_class: required_sysfs_value(&path, "bDeviceClass")?,
            authorized: authorized == "1",
            removable: match read_trim(path.join("removable")).as_deref() {
                Some("removable") => Some(true),
                Some("fixed") => Some(false),
                _ => None,
            },
            block_devices: Vec::new(),
        });
    }
    for (devnode, device_path) in block_map {
        if let Some(device) = devices
            .iter_mut()
            .filter(|device| device_path.starts_with(&device.path))
            .max_by_key(|device| device.path.components().count())
        {
            device.block_devices.push(devnode);
        }
    }
    for device in &mut devices {
        device.block_devices.sort();
        device.block_devices.dedup();
    }
    devices.sort_by(|left, right| left.sysfs_name.cmp(&right.sysfs_name));
    Ok(devices)
}

fn usb_block_map() -> Result<Vec<(PathBuf, PathBuf)>, String> {
    let entries =
        fs::read_dir("/sys/class/block").map_err(|error| format!("list block devices: {error}"))?;
    let mut devices = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("read block device entry: {error}"))?;
        let sysfs = fs::canonicalize(entry.path()).map_err(|error| {
            format!(
                "resolve block device {}: {error}",
                entry.file_name().to_string_lossy()
            )
        })?;
        devices.push((Path::new("/dev").join(entry.file_name()), sysfs));
    }
    Ok(devices)
}

fn require_device(name: &str) -> Result<UsbDevice, String> {
    if !valid_device_name(name) {
        return Err(format!("invalid USB sysfs device name: {name:?}"));
    }
    usb_devices()?
        .into_iter()
        .find(|device| device.sysfs_name == name)
        .ok_or_else(|| format!("USB device not found: {name}"))
}

fn require_same_device(expected: &UsbDevice) -> Result<UsbDevice, String> {
    let current = require_device(&expected.sysfs_name)?;
    if current.path != expected.path
        || !current.vendor_id.eq_ignore_ascii_case(&expected.vendor_id)
        || !current
            .product_id
            .eq_ignore_ascii_case(&expected.product_id)
        || current.serial != expected.serial
    {
        return Err(format!(
            "USB device {} changed while the operation was in progress",
            expected.sysfs_name
        ));
    }
    Ok(current)
}

fn authorization_snapshot(device: &UsbDevice) -> UsbAuthorizationSnapshot {
    UsbAuthorizationSnapshot {
        sysfs_name: device.sysfs_name.clone(),
        vendor_id: device.vendor_id.clone(),
        product_id: device.product_id.clone(),
        serial: device.serial.clone(),
        authorized: device.authorized,
    }
}

fn snapshot_matches(snapshot: &UsbAuthorizationSnapshot, device: &UsbDevice) -> bool {
    device.vendor_id.eq_ignore_ascii_case(&snapshot.vendor_id)
        && device.product_id.eq_ignore_ascii_case(&snapshot.product_id)
        && device.serial == snapshot.serial
}

fn prepare_authorization_restore(
    snapshots: &[UsbAuthorizationSnapshot],
) -> Result<(Vec<(UsbDevice, bool)>, Vec<String>), String> {
    let devices = usb_devices()?;
    let mut targets = Vec::new();
    let mut disconnected = Vec::new();
    for snapshot in snapshots {
        let matches = devices
            .iter()
            .filter(|device| snapshot_matches(snapshot, device))
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [] => disconnected.push(snapshot.sysfs_name.clone()),
            [device] => targets.push(((*device).clone(), snapshot.authorized)),
            _ => {
                return Err(format!(
                    "USB fingerprint from {} matches multiple connected devices",
                    snapshot.sysfs_name
                ))
            }
        }
    }
    Ok((targets, disconnected))
}

fn apply_authorization_targets(targets: &[(UsbDevice, bool)]) -> Result<Vec<Value>, String> {
    let mut results = Vec::new();
    for (device, desired) in targets {
        if device.authorized != *desired {
            write_device_authorized(device, *desired)?;
        }
        results.push(json!({
            "device": device.sysfs_name,
            "before": device.authorized,
            "after": *desired,
            "changed": device.authorized != *desired,
        }));
    }
    Ok(results)
}

fn rollback_authorization_targets(targets: &[(UsbDevice, bool)]) -> Result<(), String> {
    let mut errors = Vec::new();
    for (device, desired) in targets {
        if device.authorized == *desired {
            continue;
        }
        if let Err(error) = write_device_authorized(device, device.authorized) {
            errors.push(format!("{}: {error}", device.sysfs_name));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn valid_device_name(value: &str) -> bool {
    if value.len() > 64 {
        return false;
    }
    let Some((bus, ports)) = value.split_once('-') else {
        return false;
    };
    !bus.is_empty()
        && bus.bytes().all(|byte| byte.is_ascii_digit())
        && !ports.is_empty()
        && ports
            .split('.')
            .all(|port| !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()))
}

fn reject_hub(device: &UsbDevice) -> Result<(), String> {
    if device.device_class.eq_ignore_ascii_case("09") {
        Err("refusing to deauthorize or block a USB hub".to_string())
    } else {
        Ok(())
    }
}

fn reject_protected_storage(device: &UsbDevice) -> Result<(), String> {
    let protected = [
        "/",
        "/boot",
        "/boot/efi",
        "/home",
        "/usr",
        "/var",
        "/opt",
        "/srv",
    ];
    let related = related_block_devices(&device.block_devices)?;
    let mut block_ids = BTreeSet::new();
    for path in &related {
        let name = path
            .file_name()
            .ok_or_else(|| format!("block device has no file name: {}", path.display()))?;
        block_ids.insert(required_sysfs_value(
            &Path::new("/sys/class/block").join(name),
            "dev",
        )?);
    }
    let mountinfo = fs::read_to_string("/proc/1/mountinfo")
        .map_err(|error| format!("read system mountinfo: {error}"))?;
    for line in mountinfo.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() >= 5
            && protected.contains(&unescape_mount(fields[4]).as_str())
            && (block_ids.contains(fields[2]) || mount_source_is_related(&fields, &related)?)
        {
            return Err("USB device backs a protected system mount".to_string());
        }
    }
    let swaps =
        fs::read_to_string("/proc/swaps").map_err(|error| format!("read active swaps: {error}"))?;
    for line in swaps.lines().skip(1) {
        let Some(path) = line.split_whitespace().next() else {
            continue;
        };
        let path = unescape_mount(path);
        let metadata =
            fs::metadata(&path).map_err(|error| format!("inspect active swap {path}: {error}"))?;
        let encoded = if metadata.file_type().is_block_device() {
            metadata.rdev()
        } else {
            metadata.dev()
        };
        let swap_id = format!(
            "{}:{}",
            libc::major(encoded as libc::dev_t),
            libc::minor(encoded as libc::dev_t)
        );
        let mut swap_file_is_related = false;
        if !metadata.file_type().is_block_device() {
            for line in mountinfo.lines() {
                let fields = line.split_whitespace().collect::<Vec<_>>();
                if fields.get(2).is_some_and(|id| *id == swap_id.as_str())
                    && mount_source_is_related(&fields, &related)?
                {
                    swap_file_is_related = true;
                    break;
                }
            }
        }
        if block_ids.contains(&swap_id) || swap_file_is_related {
            return Err("USB device contains active swap".to_string());
        }
    }
    Ok(())
}

fn mount_source_is_related(fields: &[&str], related: &BTreeSet<PathBuf>) -> Result<bool, String> {
    let Some(separator) = fields.iter().position(|field| *field == "-") else {
        return Err("malformed mountinfo entry without separator".to_string());
    };
    let Some(source) = fields.get(separator + 2) else {
        return Err("malformed mountinfo entry without source".to_string());
    };
    let source = unescape_mount(source);
    if !source.starts_with("/dev/") {
        return Ok(false);
    }
    let canonical = fs::canonicalize(&source)
        .map_err(|error| format!("resolve mounted block source {source}: {error}"))?;
    Ok(related.contains(&canonical))
}

fn related_block_devices(initial: &[PathBuf]) -> Result<BTreeSet<PathBuf>, String> {
    let mut related = initial.iter().cloned().collect::<BTreeSet<_>>();
    let mut pending = initial.to_vec();
    while let Some(path) = pending.pop() {
        let name = path
            .file_name()
            .ok_or_else(|| format!("block device has no file name: {}", path.display()))?;
        let holders = Path::new("/sys/class/block").join(name).join("holders");
        let entries = match fs::read_dir(&holders) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("inspect holders for {}: {error}", path.display())),
        };
        for entry in entries {
            let entry =
                entry.map_err(|error| format!("read block-device holder entry: {error}"))?;
            let holder = Path::new("/dev").join(entry.file_name());
            if related.insert(holder.clone()) {
                pending.push(holder);
            }
        }
    }
    Ok(related)
}

fn is_partition(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        Path::new("/sys/class/block")
            .join(name)
            .join("partition")
            .is_file()
    })
}

fn write_device_authorized(device: &UsbDevice, authorized: bool) -> Result<(), String> {
    let current = require_same_device(device)?;
    let authorized_path = current.path.join("authorized");
    let mut file = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&authorized_path)
        .map_err(|error| format!("open {}: {error}", authorized_path.display()))?;
    file.write_all(if authorized { b"1" } else { b"0" })
        .map_err(|error| format!("write {}: {error}", authorized_path.display()))?;
    let actual = required_sysfs_value(&current.path, "authorized")?;
    let expected = if authorized { "1" } else { "0" };
    if actual != expected {
        return Err(format!(
            "USB authorization write did not take effect: expected {expected}, got {actual:?}"
        ));
    }
    Ok(())
}

fn required_sysfs_value(path: &Path, attribute: &str) -> Result<String, String> {
    let attribute_path = path.join(attribute);
    fs::read_to_string(&attribute_path)
        .map_err(|error| format!("read {}: {error}", attribute_path.display()))
        .map(|value| value.trim().to_string())
        .and_then(|value| {
            if value.is_empty() {
                Err(format!("{} is empty", attribute_path.display()))
            } else {
                Ok(value)
            }
        })
}

fn rule_matches(rule: &UsbBlockRule, device: &UsbDevice) -> bool {
    device.vendor_id.eq_ignore_ascii_case(&rule.vendor_id)
        && device.product_id.eq_ignore_ascii_case(&rule.product_id)
        && device.serial.as_deref() == Some(rule.serial.as_str())
}

fn validate_serial(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
    {
        Err("USB serial cannot be represented safely in a persistent rule".to_string())
    } else {
        Ok(())
    }
}

async fn apply_policy(state: &UsbGuardState) -> Result<(), String> {
    let content = render_policy(state)?;
    crate::agent::util::atomic_write_with_fsync(Path::new(POLICY_PATH), content.as_bytes())
        .map_err(|error| format!("write USB Guard udev policy: {error}"))?;
    run_checked(
        udevadm_path()?,
        &["control", "--reload-rules"],
        TOOL_TIMEOUT,
    )
    .await?;
    Ok(())
}

fn render_policy(state: &UsbGuardState) -> Result<String, String> {
    let mut content =
        String::from("# Managed by Claw OS USB Guard. Manual edits are overwritten.\n");
    for rule in &state.rules {
        validate_token(&rule.id)?;
        validate_hex_id(&rule.vendor_id)?;
        validate_hex_id(&rule.product_id)?;
        validate_serial(&rule.serial)?;
        content.push_str(&format!(
            "ACTION==\"add\", SUBSYSTEM==\"usb\", ATTR{{idVendor}}==\"{}\", ATTR{{idProduct}}==\"{}\", ATTR{{serial}}==\"{}\", ATTR{{authorized}}=\"0\"\n",
            rule.vendor_id.to_ascii_lowercase(),
            rule.product_id.to_ascii_lowercase(),
            rule.serial
        ));
    }
    Ok(content)
}

fn policy_matches_state(state: &UsbGuardState) -> Result<bool, String> {
    let expected = render_policy(state)?;
    match fs::read(POLICY_PATH) {
        Ok(actual) => Ok(actual == expected.as_bytes()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && state.rules.is_empty() => {
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("read USB Guard udev policy: {error}")),
    }
}

async fn apply_policy_change(
    next: &UsbGuardState,
    previous: &UsbGuardState,
    context: &str,
) -> Result<(), String> {
    if let Err(error) = apply_policy(next).await {
        let rollback = apply_policy(previous).await;
        return Err(format!(
            "{context} failed ({error}); rollback: {}",
            result_summary(rollback)
        ));
    }
    Ok(())
}

fn validate_hex_id(value: &str) -> Result<(), String> {
    if value.len() == 4 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err("invalid USB vendor/product id".to_string())
    }
}

fn load_state() -> Result<UsbGuardState, String> {
    let path = state_path();
    let data = match fs::read(&path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(UsbGuardState::default())
        }
        Err(error) => return Err(format!("read USB Guard state: {error}")),
    };
    let state: UsbGuardState =
        serde_json::from_slice(&data).map_err(|error| format!("parse USB Guard state: {error}"))?;
    if state.schema != 1 {
        return Err(format!("unsupported USB Guard schema: {}", state.schema));
    }
    validate_state(&state)?;
    Ok(state)
}

fn validate_state(state: &UsbGuardState) -> Result<(), String> {
    if state.revision != "initial" {
        validate_token(&state.revision)?;
    }
    let mut ids = BTreeSet::new();
    let mut fingerprints = BTreeSet::new();
    for rule in &state.rules {
        validate_token(&rule.id)?;
        validate_hex_id(&rule.vendor_id)?;
        validate_hex_id(&rule.product_id)?;
        validate_serial(&rule.serial)?;
        if !ids.insert(rule.id.to_ascii_lowercase()) {
            return Err(format!("duplicate USB Guard rule id: {}", rule.id));
        }
        let fingerprint = (
            rule.vendor_id.to_ascii_lowercase(),
            rule.product_id.to_ascii_lowercase(),
            rule.serial.clone(),
        );
        if !fingerprints.insert(fingerprint) {
            return Err("duplicate USB Guard device fingerprint".to_string());
        }
    }
    Ok(())
}

fn save_state(state: &UsbGuardState) -> Result<(), String> {
    let data = serde_json::to_vec_pretty(state)
        .map_err(|error| format!("serialize USB Guard state: {error}"))?;
    crate::agent::util::atomic_write_with_fsync(&state_path(), &data)
        .map_err(|error| format!("write USB Guard state: {error}"))
}

fn create_backup(
    owner_uid: u32,
    previous: UsbGuardState,
    applied_revision: &str,
    authorizations: Vec<UsbAuthorizationSnapshot>,
) -> Result<UsbGuardBackup, String> {
    let backup = UsbGuardBackup {
        token: uuid::Uuid::new_v4().simple().to_string(),
        owner_uid,
        created_at: chrono::Utc::now().to_rfc3339(),
        applied_revision: applied_revision.to_string(),
        previous,
        authorizations,
        status: "prepared".to_string(),
    };
    save_backup(&backup)?;
    Ok(backup)
}

fn save_backup(backup: &UsbGuardBackup) -> Result<(), String> {
    let data = serde_json::to_vec_pretty(backup)
        .map_err(|error| format!("serialize USB Guard backup: {error}"))?;
    crate::agent::util::atomic_write_with_fsync(&backup_path(&backup.token), &data)
        .map_err(|error| format!("write USB Guard backup: {error}"))
}

fn load_backup(token: &str) -> Result<UsbGuardBackup, String> {
    let data =
        fs::read(backup_path(token)).map_err(|error| format!("read USB Guard backup: {error}"))?;
    let backup: UsbGuardBackup = serde_json::from_slice(&data)
        .map_err(|error| format!("parse USB Guard backup: {error}"))?;
    validate_token(&backup.token)?;
    validate_token(&backup.applied_revision)?;
    validate_state(&backup.previous)?;
    validate_authorization_snapshots(&backup.authorizations)?;
    if backup.token != token {
        return Err("USB Guard backup token does not match its file name".to_string());
    }
    if !matches!(
        backup.status.as_str(),
        "prepared" | "applied" | "restored" | "rollback-attempted"
    ) {
        return Err(format!(
            "USB Guard backup has invalid status: {:?}",
            backup.status
        ));
    }
    Ok(backup)
}

fn validate_authorization_snapshots(snapshots: &[UsbAuthorizationSnapshot]) -> Result<(), String> {
    let mut fingerprints = BTreeSet::new();
    for snapshot in snapshots {
        if !valid_device_name(&snapshot.sysfs_name) {
            return Err(format!(
                "USB Guard backup has invalid sysfs device name: {:?}",
                snapshot.sysfs_name
            ));
        }
        validate_hex_id(&snapshot.vendor_id)?;
        validate_hex_id(&snapshot.product_id)?;
        let serial = snapshot
            .serial
            .as_deref()
            .ok_or_else(|| "USB Guard authorization snapshot has no serial".to_string())?;
        validate_serial(serial)?;
        let fingerprint = (
            snapshot.vendor_id.to_ascii_lowercase(),
            snapshot.product_id.to_ascii_lowercase(),
            serial.to_string(),
        );
        if !fingerprints.insert(fingerprint) {
            return Err("duplicate USB Guard authorization fingerprint".to_string());
        }
    }
    Ok(())
}

fn state_path() -> PathBuf {
    crate::paths::data_dir()
        .join("clawd")
        .join("usb-guard-state.json")
}

fn backup_path(token: &str) -> PathBuf {
    crate::paths::data_dir()
        .join("clawd")
        .join("usb-guard-backups")
        .join(format!("{token}.json"))
}

fn validate_token(value: &str) -> Result<(), String> {
    if value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err("invalid USB Guard token".to_string())
    }
}

fn validate_action(
    action: &str,
    device: Option<&str>,
    state: Option<&str>,
    rule_id: Option<&str>,
    token: Option<&str>,
    confirm: bool,
) -> Result<(), String> {
    match action {
        "status"
            if device.is_none()
                && state.is_none()
                && rule_id.is_none()
                && token.is_none()
                && !confirm =>
        {
            Ok(())
        }
        "authorize"
            if device.is_some_and(valid_device_name)
                && matches!(state, Some("on" | "off"))
                && rule_id.is_none()
                && token.is_none()
                && confirm == (state == Some("off")) =>
        {
            Ok(())
        }
        "block" | "eject"
            if device.is_some_and(valid_device_name)
                && state.is_none()
                && rule_id.is_none()
                && token.is_none()
                && confirm =>
        {
            Ok(())
        }
        "unblock"
            if device.is_none()
                && state.is_none()
                && rule_id.is_some_and(|id| validate_token(id).is_ok())
                && token.is_none()
                && confirm =>
        {
            Ok(())
        }
        "restore"
            if device.is_none()
                && state.is_none()
                && rule_id.is_none()
                && token.is_some_and(|token| validate_token(token).is_ok())
                && confirm =>
        {
            Ok(())
        }
        _ => Err(format!("invalid arguments for USB Guard action {action:?}")),
    }
}

async fn run_checked(
    program: &'static str,
    args: &[&str],
    timeout: Duration,
) -> Result<CommandOutput, String> {
    let args = args.iter().map(|value| value.to_string()).collect();
    tokio::task::spawn_blocking(move || run_checked_sync(program, args, timeout))
        .await
        .map_err(|error| format!("{program} worker failed: {error}"))?
}

fn run_checked_sync(
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
    let output = CommandOutput {
        status,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        stdout_truncated,
        stderr_truncated,
    };
    require_success(program, &output)?;
    Ok(output)
}

fn require_success(program: &str, output: &CommandOutput) -> Result<(), String> {
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{program} exited {}: {}",
            output.status.code().unwrap_or(-1),
            tail(&output.stderr)
        ))
    }
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
            .map_err(|error| format!("read USB command output: {error}"))?;
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

fn read_trim(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn unescape_mount(value: &str) -> String {
    value
        .replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

fn path_str(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| format!("path is not UTF-8: {}", path.display()))
}

fn tool_path(candidates: &[&'static str], name: &str) -> Result<&'static str, String> {
    candidates
        .iter()
        .copied()
        .find(|candidate| Path::new(candidate).is_file())
        .ok_or_else(|| format!("{name} is not installed"))
}

fn udevadm_path() -> Result<&'static str, String> {
    tool_path(&["/usr/bin/udevadm", "/bin/udevadm"], "udevadm")
}

fn udisksctl_path() -> Result<&'static str, String> {
    tool_path(&["/usr/bin/udisksctl", "/bin/udisksctl"], "udisksctl")
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

fn result_summary<T>(result: Result<T, String>) -> String {
    result.err().unwrap_or_else(|| "ok".to_string())
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
        "/test/unit/clawd/usb_guard.rs"
    ));
}
