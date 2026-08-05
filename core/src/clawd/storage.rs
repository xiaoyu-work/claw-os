use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::fs;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::caps::{Cap, CapSet, Scope, Verb};

use super::client_identity::ClientIdentity;

const STORAGE_SCOPE: &str = "diagnose";
const TOOL_TIMEOUT: Duration = Duration::from_secs(60);
const UDISKS_TIMEOUT: Duration = Duration::from_secs(180);
const CHECK_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const STREAM_CAP_BYTES: usize = 2 * 1024 * 1024;
const LSBLK_FIELDS: &str = "PATH,NAME,KNAME,TYPE,PKNAME,SIZE,RO,RM,HOTPLUG,TRAN,FSTYPE,FSVER,LABEL,UUID,PARTUUID,MOUNTPOINTS,MODEL,SERIAL,VENDOR,STATE";
const STORAGE_JOURNAL_FIELDS: &str =
    "__REALTIME_TIMESTAMP,_BOOT_ID,PRIORITY,SYSLOG_IDENTIFIER,MESSAGE";
static STORAGE_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

pub async fn control(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client);
        return Err("Storage Manager requires Linux".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err("Storage Manager requires root clawd".to_string());
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
        let device = optional_string(&params, "device")?;
        validate_action(&action, device.as_deref())?;
        let canonical_device = match device.as_deref() {
            Some(device) => Some(canonical_block_device(device)?),
            None => None,
        };
        let (verb, scope) = match action.as_str() {
            "status" => (Verb::SYS_OBSERVE, Scope::name("storage")),
            "health" | "check" => (Verb::SYS_STORAGE, Scope::name(STORAGE_SCOPE)),
            "mount" | "unmount" | "eject" => (
                Verb::SYS_MOUNT,
                Scope::path(path_str(
                    canonical_device
                        .as_ref()
                        .expect("mutating storage action requires a device"),
                )?),
            ),
            _ => unreachable!("validated storage action"),
        };
        crate::paths::with_user_override(uid, home.clone(), async {
            authorize_session(&session_id, peer_pid, verb, scope)
        })
        .await?;
        let mount_session_cgroup = if action == "mount" && uid != 0 {
            Some(active_session_cgroup(uid)?)
        } else {
            None
        };
        if action == "status" {
            return storage_status().await;
        }
        if action == "health" {
            return health_report(canonical_device.as_deref().unwrap()).await;
        }

        let _guard = tokio::time::timeout(
            LOCK_TIMEOUT,
            STORAGE_LOCK
                .get_or_init(|| tokio::sync::Mutex::new(()))
                .lock(),
        )
        .await
        .map_err(|_| "Storage Manager is busy with another operation".to_string())?;
        match action.as_str() {
            "check" => filesystem_check(canonical_device.as_deref().unwrap()).await,
            "mount" | "unmount" | "eject" => {
                mutate(
                    &action,
                    canonical_device.as_deref().unwrap(),
                    uid,
                    gid,
                    &home,
                    mount_session_cgroup,
                )
                .await
            }
            _ => unreachable!("validated storage action"),
        }
    }
}

fn authorize_session(
    session_id: &str,
    peer_pid: u32,
    verb: Verb,
    scope: Scope,
) -> Result<(), String> {
    let session = crate::proc::session_info_by_id(session_id)
        .ok_or_else(|| format!("storage-manager session not found: {session_id}"))?;
    if session.app_id.as_deref() != Some("storage-manager") {
        return Err("storage control is restricted to the storage-manager App".to_string());
    }
    if session.pending_bind || session.pid == 0 {
        return Err("storage-manager session is not bound to a process".to_string());
    }
    let expected_start = session
        .start_time_ticks
        .ok_or_else(|| "storage-manager session has no process identity".to_string())?;
    if crate::proc::read_start_time_ticks_pub(session.pid) != Some(expected_start) {
        return Err("storage-manager session process identity is stale".to_string());
    }
    if !crate::proc::process_descends_from(peer_pid, session.pid) {
        return Err("storage request did not originate from the authorized session".to_string());
    }
    let mut caps = session.caps.unwrap_or_else(CapSet::new);
    if let Some(transient) = session.transient_caps {
        caps.extend(transient.iter().cloned());
    }
    if !caps.covers(&Cap::new(verb, scope)) {
        return Err(format!("storage-manager session lacks {}", verb.as_str()));
    }
    Ok(())
}

async fn storage_status() -> Result<Value, String> {
    let devices = block_inventory().await?;
    let mounts = devices
        .iter()
        .flat_map(|device| {
            device["mountpoints"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|mountpoint| {
                    json!({
                        "device": device["canonical_path"],
                        "mountpoint": mountpoint,
                        "fstype": device["fstype"],
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let count = devices.len();
    let mount_count = mounts.len();
    Ok(json!({
        "providers": {
            "udisks2": tool_path(&["/usr/bin/udisksctl", "/bin/udisksctl"]).is_some(),
            "smartctl": tool_path(&["/usr/sbin/smartctl", "/usr/bin/smartctl"]).is_some(),
        },
        "devices": devices,
        "count": count,
        "mounts": mounts,
        "mount_count": mount_count,
    }))
}

async fn mutate(
    action: &str,
    device: &Path,
    uid: u32,
    gid: u32,
    home: &Path,
    mount_session_cgroup: Option<PathBuf>,
) -> Result<Value, String> {
    let before = device_state(device).await?;
    validate_mutation_target(action, device, &before).await?;
    if action == "mount" && is_mounted(&before) {
        return Ok(json!({
            "action": action,
            "device": device,
            "changed": false,
            "before": before.clone(),
            "after": before,
            "note": "device is already mounted",
        }));
    }
    if action == "unmount" && !is_mounted(&before) {
        return Ok(json!({
            "action": action,
            "device": device,
            "changed": false,
            "before": before.clone(),
            "after": before,
            "note": "device is already unmounted",
        }));
    }

    let output = match action {
        "mount" => {
            run_udisks(
                &[
                    "mount",
                    "--block-device",
                    path_str(device)?,
                    "--no-user-interaction",
                ],
                Some((uid, gid)),
                Some(home.to_path_buf()),
                mount_session_cgroup,
            )
            .await?
        }
        "unmount" => {
            run_udisks(
                &[
                    "unmount",
                    "--block-device",
                    path_str(device)?,
                    "--no-user-interaction",
                ],
                None,
                None,
                None,
            )
            .await?
        }
        "eject" if before["type"].as_str() == Some("rom") => eject_optical(device).await?,
        "eject" => {
            run_udisks(
                &[
                    "power-off",
                    "--block-device",
                    path_str(device)?,
                    "--no-user-interaction",
                ],
                None,
                None,
                None,
            )
            .await?
        }
        _ => unreachable!("validated storage mutation"),
    };
    let expect_absent = action == "eject" && before["type"].as_str() != Some("rom");
    let after = match wait_for_state(device, expect_absent).await {
        Ok(after) => after,
        Err(error) => {
            return Ok(json!({
                "action": action,
                "device": device,
                "changed": Value::Null,
                "action_applied": true,
                "before": before,
                "stdout_tail": output.stdout.trim(),
                "stderr_tail": output.stderr.trim(),
                "post_state_error": error,
            }));
        }
    };
    Ok(json!({
        "action": action,
        "device": device,
        "changed": before != after,
        "action_applied": true,
        "before": before,
        "after": after,
        "stdout_tail": output.stdout.trim(),
        "stderr_tail": output.stderr.trim(),
        "reversible": matches!(action, "mount" | "unmount"),
        "inverse_action": match action {
            "mount" => Some("unmount"),
            "unmount" => Some("mount"),
            _ => None,
        },
    }))
}

async fn validate_mutation_target(
    action: &str,
    device: &Path,
    state: &Value,
) -> Result<(), String> {
    match action {
        "mount" => {
            let fstype = state["fstype"].as_str().unwrap_or_default();
            if fstype.is_empty() || fstype == "swap" {
                return Err(format!("{} has no mountable filesystem", device.display()));
            }
            if !matches!(
                state["type"].as_str(),
                Some("part" | "lvm" | "crypt" | "rom")
            ) {
                return Err(format!(
                    "{} is not a mountable block-device type",
                    device.display()
                ));
            }
        }
        "unmount" => {
            reject_protected_mounts(device, state)?;
        }
        "eject" => {
            if !matches!(state["type"].as_str(), Some("disk" | "rom")) {
                return Err("eject requires a whole disk or optical drive".to_string());
            }
            let removable = state["removable"].as_bool().unwrap_or(false);
            let hotplug = state["hotplug"].as_bool().unwrap_or(false);
            let transport = state["transport"].as_str().unwrap_or_default();
            if !removable && !hotplug && !matches!(transport, "usb" | "mmc" | "firewire") {
                return Err(format!(
                    "{} is not reported as removable or hot-pluggable",
                    device.display()
                ));
            }
            for member in device_tree(device).await? {
                reject_protected_mounts(device, &member)?;
                if is_swap(member["canonical_path"].as_str().unwrap_or_default()) {
                    return Err(format!(
                        "{} contains active swap and cannot be ejected",
                        device.display()
                    ));
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn reject_protected_mounts(device: &Path, state: &Value) -> Result<(), String> {
    const PROTECTED: &[&str] = &[
        "/",
        "/boot",
        "/boot/efi",
        "/home",
        "/usr",
        "/var",
        "/opt",
        "/srv",
    ];
    if state["mountpoints"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .any(|mountpoint| PROTECTED.contains(&mountpoint))
    {
        return Err(format!(
            "{} backs a protected system mount and cannot be detached",
            device.display()
        ));
    }
    Ok(())
}

async fn wait_for_state(device: &Path, expect_absent: bool) -> Result<Value, String> {
    let mut last_error = None;
    for _ in 0..10 {
        match device_state_optional(device).await {
            Ok(state) if state["present"].as_bool() == Some(!expect_absent) => return Ok(state),
            Ok(_) => {}
            Err(error) => last_error = Some(error),
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    if expect_absent {
        Ok(json!({
            "present": device.exists(),
            "canonical_path": device,
        }))
    } else {
        Err(last_error
            .unwrap_or_else(|| format!("timed out reading state for {}", device.display())))
    }
}

async fn device_state(device: &Path) -> Result<Value, String> {
    let state = device_state_optional(device).await?;
    if state["present"].as_bool() != Some(true) {
        return Err(format!("block device disappeared: {}", device.display()));
    }
    Ok(state)
}

async fn device_state_optional(device: &Path) -> Result<Value, String> {
    if !device.exists() {
        return Ok(json!({
            "present": false,
            "canonical_path": device,
        }));
    }
    let devices = device_tree(device).await?;
    devices
        .into_iter()
        .find(|item| item["canonical_path"].as_str() == device.to_str())
        .map(|mut item| {
            item["present"] = Value::Bool(true);
            item
        })
        .ok_or_else(|| format!("lsblk did not return {}", device.display()))
}

async fn device_tree(device: &Path) -> Result<Vec<Value>, String> {
    let lsblk = lsblk_path()?;
    let output = run_checked(
        lsblk,
        vec![
            "--json".to_string(),
            "--bytes".to_string(),
            "--paths".to_string(),
            "--output".to_string(),
            LSBLK_FIELDS.to_string(),
            path_str(device)?.to_string(),
        ],
        TOOL_TIMEOUT,
        ChildPolicy::default(),
    )
    .await?;
    parse_lsblk(&output.stdout)
}

async fn block_inventory() -> Result<Vec<Value>, String> {
    let lsblk = lsblk_path()?;
    let output = run_checked(
        lsblk,
        vec![
            "--json".to_string(),
            "--bytes".to_string(),
            "--paths".to_string(),
            "--output".to_string(),
            LSBLK_FIELDS.to_string(),
        ],
        TOOL_TIMEOUT,
        ChildPolicy::default(),
    )
    .await?;
    parse_lsblk(&output.stdout)
}

fn parse_lsblk(output: &str) -> Result<Vec<Value>, String> {
    let value: Value =
        serde_json::from_str(output).map_err(|error| format!("parse lsblk JSON: {error}"))?;
    let mut devices = Vec::new();
    for device in value["blockdevices"]
        .as_array()
        .ok_or_else(|| "lsblk JSON has no blockdevices array".to_string())?
    {
        flatten_lsblk(device, &mut devices);
    }
    Ok(devices)
}

fn flatten_lsblk(device: &Value, out: &mut Vec<Value>) {
    let reported_path = json_string(device, "path")
        .or_else(|| json_string(device, "name"))
        .unwrap_or_default();
    let canonical_path = fs::canonicalize(&reported_path)
        .ok()
        .and_then(|path| path.to_str().map(str::to_string))
        .unwrap_or_else(|| reported_path.clone());
    let parent = json_string(device, "pkname").and_then(|value| {
        let path = if value.starts_with('/') {
            PathBuf::from(value)
        } else {
            PathBuf::from("/dev").join(value)
        };
        fs::canonicalize(&path)
            .ok()
            .and_then(|value| value.to_str().map(str::to_string))
            .or_else(|| path.to_str().map(str::to_string))
    });
    let mountpoints = device["mountpoints"]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .or_else(|| {
            device["mountpoint"]
                .as_str()
                .filter(|value| !value.is_empty())
                .map(|value| vec![value.to_string()])
        })
        .unwrap_or_default();
    out.push(json!({
        "path": reported_path,
        "canonical_path": canonical_path,
        "name": json_string(device, "name"),
        "kernel_name": json_string(device, "kname"),
        "type": json_string(device, "type"),
        "parent": parent,
        "size_bytes": json_u64(device, "size"),
        "read_only": json_bool(device, "ro"),
        "removable": json_bool(device, "rm"),
        "hotplug": json_bool(device, "hotplug"),
        "transport": json_string(device, "tran"),
        "fstype": json_string(device, "fstype"),
        "fs_version": json_string(device, "fsver"),
        "label": json_string(device, "label"),
        "uuid": json_string(device, "uuid"),
        "partuuid": json_string(device, "partuuid"),
        "mountpoints": mountpoints,
        "model": json_string(device, "model").map(|value| value.trim().to_string()),
        "serial": json_string(device, "serial").map(|value| value.trim().to_string()),
        "vendor": json_string(device, "vendor").map(|value| value.trim().to_string()),
        "state": json_string(device, "state"),
    }));
    if let Some(children) = device["children"].as_array() {
        for child in children {
            flatten_lsblk(child, out);
        }
    }
}

async fn health_report(device: &Path) -> Result<Value, String> {
    let info = device_state(device).await?;
    let inventory = block_inventory().await?;
    let smart_device = resolve_smart_device(device, &inventory);
    let smart = smart_report(&smart_device).await;
    let filesystem = filesystem_metadata(device, &info).await;
    let kernel_events = storage_kernel_events(device, &smart_device).await?;
    let mut findings = Vec::new();

    match &smart {
        Ok(value) => {
            if value
                .pointer("/data/smart_status/passed")
                .and_then(Value::as_bool)
                == Some(false)
            {
                findings.push(json!({
                    "code": "smart-failing",
                    "severity": "critical",
                    "title": "SMART reports the drive is failing",
                    "recommendation": "Back up data immediately and replace the drive.",
                }));
            }
            if value["exit_flags"].as_array().is_some_and(|flags| {
                flags.iter().any(|flag| {
                    matches!(
                        flag.as_str(),
                        Some(
                            "disk-failing" | "prefail-attribute" | "error-log" | "self-test-errors"
                        )
                    )
                })
            }) {
                findings.push(json!({
                    "code": "smart-warnings",
                    "severity": "critical",
                    "title": "SMART reported failure or error-history flags",
                    "recommendation": "Preserve important data and inspect the SMART attributes before further stress.",
                }));
            }
        }
        Err(error) => findings.push(json!({
            "code": "smart-unavailable",
            "severity": "warning",
            "title": "SMART health could not be read",
            "detail": error,
            "recommendation": "Install smartmontools or verify that the device exposes SMART data.",
        })),
    }
    if let Ok(value) = &filesystem {
        if value["state"]
            .as_str()
            .is_some_and(|state| !state.eq_ignore_ascii_case("clean"))
        {
            findings.push(json!({
                "code": "filesystem-state",
                "severity": "critical",
                "title": "Filesystem metadata does not report a clean state",
                "detail": value["state"],
                "recommendation": "Unmount the filesystem and run the read-only check before planning a repair.",
            }));
        }
    }
    if !kernel_events.is_empty() {
        findings.push(json!({
            "code": "kernel-storage-errors",
            "severity": "critical",
            "title": "Recent kernel storage errors match this device",
            "detail": format!("{} matching event(s) were found.", kernel_events.len()),
            "recommendation": "Back up data, inspect cabling/power and SMART, then avoid writes until the cause is known.",
        }));
    }
    let status = if findings
        .iter()
        .any(|finding| finding["severity"].as_str() == Some("critical"))
    {
        "critical"
    } else if findings.is_empty() {
        "ok"
    } else {
        "warning"
    };
    Ok(json!({
        "status": status,
        "device": info,
        "smart_device": smart_device,
        "smart": smart.unwrap_or_else(|error| json!({"available": false, "error": error})),
        "filesystem": filesystem.unwrap_or_else(|error| json!({"available": false, "error": error})),
        "kernel_events": kernel_events,
        "findings": findings,
    }))
}

fn resolve_smart_device(device: &Path, inventory: &[Value]) -> PathBuf {
    let mut by_path = BTreeMap::new();
    for item in inventory {
        if let Some(path) = item["canonical_path"].as_str() {
            by_path.insert(path.to_string(), item);
        }
    }
    let mut current = device.to_string_lossy().into_owned();
    let mut seen = BTreeSet::new();
    while seen.insert(current.clone()) {
        let Some(info) = by_path.get(&current) else {
            break;
        };
        if info["type"].as_str() == Some("disk") {
            return PathBuf::from(current);
        }
        let Some(parent) = info["parent"].as_str() else {
            break;
        };
        current = parent.to_string();
    }
    device.to_path_buf()
}

async fn smart_report(device: &Path) -> Result<Value, String> {
    let smartctl = tool_path(&["/usr/sbin/smartctl", "/usr/bin/smartctl"])
        .ok_or_else(|| "smartctl is not installed".to_string())?;
    let output = run_command(
        smartctl,
        vec![
            "--all".to_string(),
            "--json=c".to_string(),
            path_str(device)?.to_string(),
        ],
        TOOL_TIMEOUT,
        ChildPolicy::default(),
    )
    .await?;
    let data = serde_json::from_str::<Value>(&output.stdout)
        .map_err(|error| format!("parse smartctl JSON: {error}; {}", tail(&output.stderr)))?;
    let code = output.status.code().unwrap_or(255).clamp(0, 255) as u8;
    Ok(json!({
        "available": true,
        "device": device,
        "exit_code": code,
        "exit_flags": smart_exit_flags(code),
        "data": data,
        "stderr_tail": tail(&output.stderr),
        "stdout_truncated": output.stdout_truncated,
        "stderr_truncated": output.stderr_truncated,
    }))
}

fn smart_exit_flags(code: u8) -> Vec<&'static str> {
    [
        (0, "command-line-error"),
        (1, "device-open-failed"),
        (2, "smart-command-failed"),
        (3, "disk-failing"),
        (4, "prefail-attribute"),
        (5, "past-prefail-attribute"),
        (6, "error-log"),
        (7, "self-test-errors"),
    ]
    .into_iter()
    .filter_map(|(bit, label)| (code & (1 << bit) != 0).then_some(label))
    .collect()
}

async fn filesystem_metadata(device: &Path, info: &Value) -> Result<Value, String> {
    let fstype = info["fstype"].as_str().unwrap_or_default();
    let mountpoint = info["mountpoints"]
        .as_array()
        .and_then(|values| values.first())
        .and_then(Value::as_str);
    match fstype {
        "ext2" | "ext3" | "ext4" => {
            let tune2fs = tool_path(&["/usr/sbin/tune2fs", "/usr/bin/tune2fs"])
                .ok_or_else(|| "tune2fs is not installed".to_string())?;
            let output = run_checked(
                tune2fs,
                vec!["-l".to_string(), path_str(device)?.to_string()],
                TOOL_TIMEOUT,
                ChildPolicy::default(),
            )
            .await?;
            let fields = parse_colon_fields(&output.stdout);
            Ok(json!({
                "available": true,
                "fstype": fstype,
                "mounted": mountpoint.is_some(),
                "mountpoint": mountpoint,
                "state": fields.get("Filesystem state"),
                "errors_behavior": fields.get("Errors behavior"),
                "last_checked": fields.get("Last checked"),
                "check_interval": fields.get("Check interval"),
                "mount_count": fields.get("Mount count"),
                "maximum_mount_count": fields.get("Maximum mount count"),
            }))
        }
        "btrfs" if mountpoint.is_some() => {
            let btrfs = tool_path(&["/usr/bin/btrfs", "/usr/sbin/btrfs"])
                .ok_or_else(|| "btrfs tools are not installed".to_string())?;
            let output = run_command(
                btrfs,
                vec![
                    "device".to_string(),
                    "stats".to_string(),
                    "--check".to_string(),
                    mountpoint.unwrap().to_string(),
                ],
                TOOL_TIMEOUT,
                ChildPolicy::default(),
            )
            .await?;
            Ok(json!({
                "available": true,
                "fstype": fstype,
                "mounted": true,
                "mountpoint": mountpoint,
                "state": if output.status.success() { "clean" } else { "errors-reported" },
                "device_stats": output.stdout,
                "stderr_tail": tail(&output.stderr),
            }))
        }
        "xfs" if mountpoint.is_some() => {
            let xfs_info = tool_path(&["/usr/sbin/xfs_info", "/usr/bin/xfs_info"])
                .ok_or_else(|| "xfs_info is not installed".to_string())?;
            let output = run_checked(
                xfs_info,
                vec![mountpoint.unwrap().to_string()],
                TOOL_TIMEOUT,
                ChildPolicy::default(),
            )
            .await?;
            Ok(json!({
                "available": true,
                "fstype": fstype,
                "mounted": true,
                "mountpoint": mountpoint,
                "state": "online",
                "info": output.stdout,
            }))
        }
        "" => Err("device has no detected filesystem".to_string()),
        _ => Ok(json!({
            "available": true,
            "fstype": fstype,
            "mounted": mountpoint.is_some(),
            "mountpoint": mountpoint,
            "state": Value::Null,
            "note": "No filesystem-specific metadata reader is available.",
        })),
    }
}

async fn storage_kernel_events(device: &Path, smart_device: &Path) -> Result<Vec<Value>, String> {
    let journalctl = journalctl_path()?;
    let output = run_checked(
        journalctl,
        vec![
            "--no-pager".to_string(),
            "--quiet".to_string(),
            "--dmesg".to_string(),
            "--since=-24h".to_string(),
            "--reverse".to_string(),
            "-n".to_string(),
            "1000".to_string(),
            "--output=json".to_string(),
            format!("--output-fields={STORAGE_JOURNAL_FIELDS}"),
        ],
        TOOL_TIMEOUT,
        ChildPolicy::default(),
    )
    .await?;
    let names = [device, smart_device]
        .iter()
        .filter_map(|path| path.file_name().and_then(|value| value.to_str()))
        .map(|value| value.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    Ok(parse_json_records(&output.stdout)
        .into_iter()
        .filter_map(|record| {
            let message = record["MESSAGE"].as_str()?.to_string();
            let lower = message.to_ascii_lowercase();
            let storage_error = [
                "i/o error",
                "buffer i/o",
                "blk_update_request",
                "critical medium error",
                "uncorrectable error",
                "device offline",
                "reset controller",
                "nvme timeout",
                "ext4-fs error",
                "xfs error",
                "xfs corruption",
                "btrfs error",
                "ata bus error",
            ]
            .iter()
            .any(|needle| lower.contains(needle));
            if !storage_error || !names.iter().any(|name| lower.contains(name)) {
                return None;
            }
            Some(json!({
                "timestamp_us": json_u64(&record, "__REALTIME_TIMESTAMP"),
                "priority": json_string(&record, "PRIORITY"),
                "identifier": json_string(&record, "SYSLOG_IDENTIFIER"),
                "message": message,
            }))
        })
        .collect())
}

async fn filesystem_check(device: &Path) -> Result<Value, String> {
    let before = device_state(device).await?;
    if is_mounted(&before) {
        return Err("filesystem check requires an unmounted device".to_string());
    }
    if is_swap(path_str(device)?) || before["fstype"].as_str() == Some("swap") {
        return Err("filesystem check does not operate on swap devices".to_string());
    }
    let fstype = before["fstype"]
        .as_str()
        .ok_or_else(|| "device has no detected filesystem".to_string())?;
    let (program, args, clean_codes): (&'static str, Vec<String>, &[i32]) = match fstype {
        "ext2" | "ext3" | "ext4" => (
            required_tool(&["/usr/sbin/e2fsck", "/usr/bin/e2fsck"], "e2fsck")?,
            vec![
                "-f".to_string(),
                "-n".to_string(),
                path_str(device)?.to_string(),
            ],
            &[0],
        ),
        "xfs" => (
            required_tool(
                &["/usr/sbin/xfs_repair", "/usr/bin/xfs_repair"],
                "xfs_repair",
            )?,
            vec!["-n".to_string(), path_str(device)?.to_string()],
            &[0],
        ),
        "btrfs" => (
            required_tool(&["/usr/bin/btrfs", "/usr/sbin/btrfs"], "btrfs")?,
            vec![
                "check".to_string(),
                "--readonly".to_string(),
                path_str(device)?.to_string(),
            ],
            &[0],
        ),
        "vfat" | "fat" | "msdos" => (
            required_tool(&["/usr/sbin/fsck.vfat", "/usr/bin/fsck.vfat"], "fsck.vfat")?,
            vec!["-n".to_string(), path_str(device)?.to_string()],
            &[0],
        ),
        "exfat" => (
            required_tool(
                &["/usr/sbin/fsck.exfat", "/usr/bin/fsck.exfat"],
                "fsck.exfat",
            )?,
            vec!["-n".to_string(), path_str(device)?.to_string()],
            &[0],
        ),
        other => {
            return Err(format!(
                "read-only filesystem check is unsupported for {other}"
            ))
        }
    };
    let output = run_command(program, args, CHECK_TIMEOUT, ChildPolicy::default()).await?;
    let (after, post_state_error) = match device_state_optional(device).await {
        Ok(after) => (after, None),
        Err(error) => (
            json!({
                "present": device.exists(),
                "canonical_path": device,
            }),
            Some(error),
        ),
    };
    let mounted_during_check = is_mounted(&after);
    let device_present = after["present"].as_bool() == Some(true);
    let exit_code = output.status.code();
    Ok(json!({
        "device": device,
        "fstype": fstype,
        "read_only": true,
        "exit_code": exit_code,
        "clean": exit_code.is_some_and(|code| clean_codes.contains(&code)),
        "issues_found": exit_code.is_some_and(|code| !clean_codes.contains(&code)),
        "result_valid": device_present && !mounted_during_check && post_state_error.is_none(),
        "device_present_after": device_present,
        "mounted_during_check": mounted_during_check,
        "post_state": after,
        "post_state_error": post_state_error,
        "stdout": output.stdout,
        "stderr": output.stderr,
        "stdout_truncated": output.stdout_truncated,
        "stderr_truncated": output.stderr_truncated,
    }))
}

async fn eject_optical(device: &Path) -> Result<CommandOutput, String> {
    let udisksctl = udisksctl_path()?;
    let info = run_checked(
        udisksctl,
        vec![
            "info".to_string(),
            "--block-device".to_string(),
            path_str(device)?.to_string(),
        ],
        TOOL_TIMEOUT,
        ChildPolicy::default(),
    )
    .await?;
    let block_object = info
        .stdout
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("/org/freedesktop/UDisks2/") && line.ends_with(':'))
        .map(|line| line.trim_end_matches(':').to_string())
        .ok_or_else(|| "udisksctl did not report a block object path".to_string())?;
    let busctl = busctl_path()?;
    let drive = run_checked(
        busctl,
        vec![
            "get-property".to_string(),
            "org.freedesktop.UDisks2".to_string(),
            block_object,
            "org.freedesktop.UDisks2.Block".to_string(),
            "Drive".to_string(),
        ],
        TOOL_TIMEOUT,
        ChildPolicy::default(),
    )
    .await?;
    let drive_object = drive
        .stdout
        .split('"')
        .nth(1)
        .filter(|value| value.starts_with("/org/freedesktop/UDisks2/drives/"))
        .ok_or_else(|| "UDisks2 did not report a drive object path".to_string())?
        .to_string();
    run_checked(
        busctl,
        vec![
            "call".to_string(),
            "org.freedesktop.UDisks2".to_string(),
            drive_object,
            "org.freedesktop.UDisks2.Drive".to_string(),
            "Eject".to_string(),
            "a{sv}".to_string(),
            "1".to_string(),
            "auth.no_user_interaction".to_string(),
            "b".to_string(),
            "true".to_string(),
        ],
        UDISKS_TIMEOUT,
        ChildPolicy::default(),
    )
    .await
}

async fn run_udisks(
    args: &[&str],
    identity: Option<(u32, u32)>,
    home: Option<PathBuf>,
    session_cgroup: Option<PathBuf>,
) -> Result<CommandOutput, String> {
    run_checked(
        udisksctl_path()?,
        args.iter().map(|value| value.to_string()).collect(),
        UDISKS_TIMEOUT,
        ChildPolicy {
            identity,
            home,
            session_cgroup,
        },
    )
    .await
}

fn canonical_block_device(raw: &str) -> Result<PathBuf, String> {
    if raw.len() > 4096
        || !raw.starts_with("/dev/")
        || raw.chars().any(|character| character.is_control())
    {
        return Err(format!("invalid block-device path: {raw:?}"));
    }

    let path =
        fs::canonicalize(raw).map_err(|error| format!("resolve block device {raw:?}: {error}"))?;
    if !path.starts_with("/dev") {
        return Err(format!(
            "block device resolves outside /dev: {}",
            path.display()
        ));
    }
    let metadata =
        fs::metadata(&path).map_err(|error| format!("inspect {}: {error}", path.display()))?;
    if !metadata.file_type().is_block_device() {
        return Err(format!("not a block device: {}", path.display()));
    }
    Ok(path)
}

fn active_session_cgroup(uid: u32) -> Result<PathBuf, String> {
    let preferred = fs::read_to_string(format!("/run/systemd/users/{uid}"))
        .ok()
        .and_then(|data| {
            data.lines()
                .find_map(|line| line.strip_prefix("DISPLAY="))
                .map(str::to_string)
        });
    let preferred_scope = preferred
        .as_deref()
        .map(|session_id| format!("session-{session_id}.scope"));
    let mut session_ids = Vec::new();
    if let Some(preferred) = preferred.as_ref() {
        session_ids.push(preferred.clone());
    }
    if let Ok(entries) = fs::read_dir("/run/systemd/sessions") {
        let mut discovered = entries
            .flatten()
            .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
            .collect::<Vec<_>>();
        discovered.sort();
        session_ids.extend(discovered);
    }
    let mut scopes = BTreeSet::new();
    for session_id in session_ids {
        if session_id.is_empty() || !session_id.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            continue;
        }
        let Ok(session_data) = fs::read_to_string(format!("/run/systemd/sessions/{session_id}"))
        else {
            continue;
        };
        let fields = session_data
            .lines()
            .filter_map(|line| line.split_once('='))
            .collect::<BTreeMap<_, _>>();
        let expected_scope = format!("session-{session_id}.scope");
        if fields
            .get("UID")
            .and_then(|value| value.parse::<u32>().ok())
            == Some(uid)
            && fields.get("ACTIVE") == Some(&"1")
            && fields.get("REMOTE") != Some(&"1")
            && fields.get("SCOPE") == Some(&expected_scope.as_str())
        {
            scopes.insert(expected_scope);
        }
    }
    if let Some(preferred_scope) = preferred_scope {
        if scopes.contains(&preferred_scope) {
            return canonical_session_cgroup(uid, &preferred_scope);
        }
    }
    let session_scope = match scopes.into_iter().collect::<Vec<_>>().as_slice() {
        [scope] => scope.clone(),
        [] => {
            return Err(
                "mount requires an active local logind session for the requesting user".to_string(),
            )
        }
        _ => {
            return Err(
                "mount is ambiguous because the requesting user has multiple active local sessions"
                    .to_string(),
            )
        }
    };
    canonical_session_cgroup(uid, &session_scope)
}

fn canonical_session_cgroup(uid: u32, session_scope: &str) -> Result<PathBuf, String> {
    let cgroup = Path::new("/sys/fs/cgroup")
        .join("user.slice")
        .join(format!("user-{uid}.slice"))
        .join(session_scope)
        .join("cgroup.procs");
    let cgroup = fs::canonicalize(cgroup)
        .map_err(|error| format!("resolve active session cgroup: {error}"))?;
    if !cgroup.starts_with("/sys/fs/cgroup") {
        return Err("active session cgroup resolves outside /sys/fs/cgroup".to_string());
    }
    Ok(cgroup)
}

fn validate_action(action: &str, device: Option<&str>) -> Result<(), String> {
    match action {
        "status" if device.is_none() => Ok(()),
        "health" | "check" | "mount" | "unmount" | "eject"
            if device.is_some_and(|value| !value.is_empty()) =>
        {
            Ok(())
        }
        "status" => Err("status does not accept a device".to_string()),
        "health" | "check" | "mount" | "unmount" | "eject" => {
            Err(format!("{action} requires a block-device path"))
        }
        _ => Err(format!("unknown storage action: {action}")),
    }
}

fn is_mounted(state: &Value) -> bool {
    state["mountpoints"]
        .as_array()
        .is_some_and(|mountpoints| !mountpoints.is_empty())
}

fn is_swap(device: &str) -> bool {
    let Ok(data) = fs::read_to_string("/proc/swaps") else {
        return false;
    };
    data.lines().skip(1).any(|line| {
        line.split_whitespace().next().is_some_and(|path| {
            fs::canonicalize(path)
                .ok()
                .and_then(|value| value.to_str().map(str::to_string))
                .as_deref()
                == Some(device)
        })
    })
}

fn parse_colon_fields(output: &str) -> BTreeMap<String, String> {
    output
        .lines()
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
        .collect()
}

fn parse_json_records(output: &str) -> Vec<Value> {
    output
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .collect()
}

fn json_string(value: &Value, key: &str) -> Option<String> {
    match value.get(key) {
        Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
        Some(Value::Number(value)) => Some(value.to_string()),
        _ => None,
    }
}

fn json_u64(value: &Value, key: &str) -> Option<u64> {
    match value.get(key) {
        Some(Value::Number(value)) => value.as_u64(),
        Some(Value::String(value)) => value.parse().ok(),
        _ => None,
    }
}

fn json_bool(value: &Value, key: &str) -> Option<bool> {
    match value.get(key) {
        Some(Value::Bool(value)) => Some(*value),
        Some(Value::Number(value)) => value.as_u64().map(|value| value != 0),
        Some(Value::String(value)) => match value.as_str() {
            "0" | "false" | "no" => Some(false),
            "1" | "true" | "yes" => Some(true),
            _ => None,
        },
        _ => None,
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

fn path_str(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| format!("path is not UTF-8: {}", path.display()))
}

fn required_tool(candidates: &[&'static str], name: &str) -> Result<&'static str, String> {
    tool_path(candidates).ok_or_else(|| format!("{name} is not installed"))
}

fn tool_path(candidates: &[&'static str]) -> Option<&'static str> {
    candidates
        .iter()
        .copied()
        .find(|candidate| Path::new(candidate).is_file())
}

fn lsblk_path() -> Result<&'static str, String> {
    required_tool(&["/usr/bin/lsblk", "/bin/lsblk"], "lsblk")
}

fn udisksctl_path() -> Result<&'static str, String> {
    required_tool(&["/usr/bin/udisksctl", "/bin/udisksctl"], "udisksctl")
}

fn busctl_path() -> Result<&'static str, String> {
    required_tool(&["/usr/bin/busctl", "/bin/busctl"], "busctl")
}

fn journalctl_path() -> Result<&'static str, String> {
    required_tool(&["/usr/bin/journalctl", "/bin/journalctl"], "journalctl")
}

#[derive(Clone, Default)]
struct ChildPolicy {
    identity: Option<(u32, u32)>,
    home: Option<PathBuf>,
    session_cgroup: Option<PathBuf>,
}

struct CommandOutput {
    status: ExitStatus,
    stdout: String,
    stderr: String,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

async fn run_checked(
    program: &'static str,
    args: Vec<String>,
    timeout: Duration,
    policy: ChildPolicy,
) -> Result<CommandOutput, String> {
    let output = run_command(program, args, timeout, policy).await?;
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

async fn run_command(
    program: &'static str,
    args: Vec<String>,
    timeout: Duration,
    policy: ChildPolicy,
) -> Result<CommandOutput, String> {
    tokio::task::spawn_blocking(move || run_command_sync(program, args, timeout, policy))
        .await
        .map_err(|error| format!("{program} worker failed: {error}"))?
}

fn run_command_sync(
    program: &str,
    args: Vec<String>,
    timeout: Duration,
    policy: ChildPolicy,
) -> Result<CommandOutput, String> {
    let mut command = Command::new(program);
    command
        .args(&args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env(
            "HOME",
            policy.home.as_deref().unwrap_or(Path::new("/nonexistent")),
        )
        .env("LC_ALL", "C.UTF-8")
        .env("SYSTEMD_PAGER", "cat")
        .env("PAGER", "cat")
        .env(
            "DBUS_SYSTEM_BUS_ADDRESS",
            "unix:path=/run/dbus/system_bus_socket",
        )
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some((uid, _)) = policy.identity {
        let runtime_dir = PathBuf::from(format!("/run/user/{uid}"));
        if runtime_dir.is_dir() {
            command.env("XDG_RUNTIME_DIR", runtime_dir);
        }
    }
    let identity = policy.identity;
    let session_cgroup = policy
        .session_cgroup
        .as_deref()
        .map(|path| {
            CString::new(path.as_os_str().as_bytes())
                .map_err(|_| format!("session cgroup path contains NUL: {}", path.display()))
        })
        .transpose()?;
    unsafe {
        command.pre_exec(move || apply_child_policy(identity, session_cgroup.as_ref()));
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

fn apply_child_policy(
    identity: Option<(u32, u32)>,
    session_cgroup: Option<&CString>,
) -> std::io::Result<()> {
    if let Some(path) = session_cgroup {
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let moved = unsafe { libc::write(fd, b"0".as_ptr().cast(), 1) };
        let move_error = (moved != 1).then(std::io::Error::last_os_error);
        let close_result = unsafe { libc::close(fd) };
        if let Some(error) = move_error {
            return Err(error);
        }
        if close_result != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    let limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::setrlimit(libc::RLIMIT_CORE as _, &limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if let Some((uid, gid)) = identity {
        if unsafe { libc::setgroups(0, std::ptr::null()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        if unsafe { libc::setgid(gid) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        if unsafe { libc::setuid(uid) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

fn read_bounded(mut reader: impl Read) -> Result<(Vec<u8>, bool), String> {
    let mut kept = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    let mut truncated = false;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("read child output: {error}"))?;
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
    fn smart_exit_status_is_decoded() {
        assert_eq!(
            smart_exit_flags((1 << 3) | (1 << 6)),
            vec!["disk-failing", "error-log"]
        );
    }

    #[test]
    fn lsblk_tree_is_flattened_with_mountpoints() {
        let values = parse_lsblk(
            r#"{"blockdevices":[{"path":"/dev/example","name":"/dev/example","kname":"example","type":"disk","mountpoints":[null],"children":[{"path":"/dev/example1","name":"/dev/example1","kname":"example1","type":"part","pkname":"example","fstype":"ext4","mountpoints":["/mnt/example"]}]}]}"#,
        )
        .unwrap();
        assert_eq!(values.len(), 2);
        assert_eq!(values[1]["mountpoints"][0], "/mnt/example");
    }

    #[test]
    fn storage_actions_require_expected_device_shape() {
        assert!(validate_action("status", None).is_ok());
        assert!(validate_action("status", Some("/dev/sda")).is_err());
        assert!(validate_action("mount", Some("/dev/sda1")).is_ok());
        assert!(validate_action("eject", None).is_err());
    }
}
