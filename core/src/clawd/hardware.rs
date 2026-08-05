use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use crate::caps::{Cap, CapSet, Scope, Verb};

use super::client_identity::ClientIdentity;

const TOOL_TIMEOUT: Duration = Duration::from_secs(30);
const STREAM_CAP_BYTES: usize = 2 * 1024 * 1024;

pub async fn inspect(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client);
        return Err("Hardware Center requires Linux".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err("Hardware Center requires root clawd".to_string());
        }
        let uid = client.require_uid()?;
        let home = client.require_home_dir()?;
        let peer_pid = client
            .pid
            .ok_or_else(|| "clawd peer pid is unavailable".to_string())?;
        let session_id = required_string(&params, "session")?;
        let action = required_string(&params, "action")?;
        validate_action(&action)?;
        crate::paths::with_user_override(uid, home, async {
            authorize_session(&session_id, peer_pid)
        })
        .await?;

        match action.as_str() {
            "summary" => summary().await,
            "cpu" => cpu_inventory().await,
            "gpu" => gpu_inventory().await,
            "pci" => pci_inventory().await,
            "usb" => usb_inventory(),
            "memory" => memory_inventory().await,
            "storage" => storage_inventory().await,
            "drivers" => driver_inventory(),
            "thermal" => thermal_inventory(),
            _ => unreachable!("validated hardware action"),
        }
    }
}

fn authorize_session(session_id: &str, peer_pid: u32) -> Result<(), String> {
    let session = crate::proc::session_info_by_id(session_id)
        .ok_or_else(|| format!("hardware-center session not found: {session_id}"))?;
    if session.app_id.as_deref() != Some("hardware-center") {
        return Err("hardware inventory is restricted to the hardware-center App".to_string());
    }
    if session.pending_bind || session.pid == 0 {
        return Err("hardware-center session is not bound to a process".to_string());
    }
    let expected_start = session
        .start_time_ticks
        .ok_or_else(|| "hardware-center session has no process identity".to_string())?;
    if crate::proc::read_start_time_ticks_pub(session.pid) != Some(expected_start) {
        return Err("hardware-center session process identity is stale".to_string());
    }
    if !crate::proc::process_descends_from(peer_pid, session.pid) {
        return Err("hardware request did not originate from the authorized session".to_string());
    }
    let mut caps = session.caps.unwrap_or_else(CapSet::new);
    if let Some(transient) = session.transient_caps {
        caps.extend(transient.iter().cloned());
    }
    let requested = Cap::new(Verb::SYS_OBSERVE, Scope::name("hardware"));
    if !caps.covers(&requested) {
        return Err("hardware-center session lacks sys.observe:hardware".to_string());
    }
    Ok(())
}

async fn summary() -> Result<Value, String> {
    let (cpu, gpu, pci, memory, storage) = tokio::join!(
        cpu_inventory(),
        gpu_inventory(),
        pci_inventory(),
        memory_inventory(),
        storage_inventory(),
    );
    Ok(json!({
        "schema": 1,
        "system": dmi_identity(),
        "cpu": result_value(cpu),
        "gpu": result_value(gpu),
        "pci": result_value(pci),
        "usb": result_value(usb_inventory()),
        "memory": result_value(memory),
        "storage": result_value(storage),
        "drivers": result_value(driver_inventory()),
        "thermal": result_value(thermal_inventory()),
    }))
}

fn result_value(result: Result<Value, String>) -> Value {
    result.unwrap_or_else(|error| json!({"available": false, "error": error}))
}

async fn cpu_inventory() -> Result<Value, String> {
    let mut fields = Map::new();
    if let Some(lscpu) = tool_path(&["/usr/bin/lscpu", "/bin/lscpu"]) {
        let output = run_checked(lscpu, &["--json", "--bytes"], TOOL_TIMEOUT).await?;
        if let Ok(value) = serde_json::from_str::<Value>(&output.stdout) {
            if let Some(entries) = value["lscpu"].as_array() {
                for entry in entries {
                    let Some(field) = entry["field"].as_str() else {
                        continue;
                    };
                    let key = normalize_key(field.trim_end_matches(':'));
                    fields.insert(key, entry["data"].clone());
                }
            }
        }
    }
    let cpuinfo = fs::read_to_string("/proc/cpuinfo")
        .map_err(|error| format!("read /proc/cpuinfo: {error}"))?;
    let processors = cpuinfo
        .lines()
        .filter(|line| line.starts_with("processor"))
        .count();
    let first = parse_colon_fields(cpuinfo.split("\n\n").next().unwrap_or_default());
    let vulnerabilities = read_directory_values("/sys/devices/system/cpu/vulnerabilities");
    Ok(json!({
        "available": true,
        "logical_processors": processors,
        "model_name": first.get("model name"),
        "vendor_id": first.get("vendor_id"),
        "microcode": first.get("microcode"),
        "flags": first.get("flags").map(|flags| flags.split_whitespace().collect::<Vec<_>>()),
        "lscpu": fields,
        "vulnerabilities": vulnerabilities,
        "loadavg": fs::read_to_string("/proc/loadavg").ok().map(|value| value.trim().to_string()),
    }))
}

async fn gpu_inventory() -> Result<Value, String> {
    let pci = pci_devices();
    let mut gpus = pci
        .into_iter()
        .filter(|device| {
            device["class"]
                .as_str()
                .is_some_and(|class| class.trim_start_matches("0x").starts_with("03"))
        })
        .collect::<Vec<_>>();
    if let Some(lspci) = tool_path(&["/usr/bin/lspci", "/bin/lspci"]) {
        if let Ok(output) = run_checked(lspci, &["-D", "-vmm", "-nn", "-k"], TOOL_TIMEOUT).await {
            let descriptions = parse_lspci_blocks(&output.stdout);
            for gpu in &mut gpus {
                if let Some(slot) = gpu["slot"].as_str() {
                    if let Some(description) = descriptions.get(slot) {
                        gpu["description"] = description.clone();
                    }
                }
            }
        }
    }
    let drm = fs::read_dir("/sys/class/drm")
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_string();
            if !name.starts_with("card") || name.contains('-') {
                return None;
            }
            let device = fs::canonicalize(entry.path().join("device")).ok()?;
            Some(json!({
                "card": name,
                "device_path": device,
                "driver": driver_name(&device),
            }))
        })
        .collect::<Vec<_>>();
    let count = gpus.len();
    Ok(json!({
        "available": true,
        "gpus": gpus,
        "count": count,
        "drm": drm,
    }))
}

async fn pci_inventory() -> Result<Value, String> {
    let mut devices = pci_devices();
    if let Some(lspci) = tool_path(&["/usr/bin/lspci", "/bin/lspci"]) {
        if let Ok(output) = run_checked(lspci, &["-D", "-vmm", "-nn", "-k"], TOOL_TIMEOUT).await {
            let descriptions = parse_lspci_blocks(&output.stdout);
            for device in &mut devices {
                if let Some(slot) = device["slot"].as_str() {
                    if let Some(description) = descriptions.get(slot) {
                        device["description"] = description.clone();
                    }
                }
            }
        }
    }
    let count = devices.len();
    Ok(json!({
        "available": true,
        "devices": devices,
        "count": count,
    }))
}

fn pci_devices() -> Vec<Value> {
    let Ok(entries) = fs::read_dir("/sys/bus/pci/devices") else {
        return Vec::new();
    };
    let mut devices = entries
        .flatten()
        .filter_map(|entry| {
            let slot = entry.file_name().to_str()?.to_string();
            let path = entry.path();
            Some(json!({
                "slot": slot,
                "vendor_id": read_trim(path.join("vendor")),
                "device_id": read_trim(path.join("device")),
                "class": read_trim(path.join("class")),
                "subsystem_vendor_id": read_trim(path.join("subsystem_vendor")),
                "subsystem_device_id": read_trim(path.join("subsystem_device")),
                "revision": read_trim(path.join("revision")),
                "numa_node": read_i64(path.join("numa_node")),
                "driver": driver_name(&path),
                "iommu_group": symlink_name(path.join("iommu_group")),
            }))
        })
        .collect::<Vec<_>>();
    devices.sort_by(|left, right| left["slot"].as_str().cmp(&right["slot"].as_str()));
    devices
}

fn parse_lspci_blocks(output: &str) -> BTreeMap<String, Value> {
    output
        .split("\n\n")
        .filter_map(|block| {
            let fields = parse_colon_fields(block);
            let slot = fields.get("Slot")?.trim_matches('"').to_string();
            let value = fields
                .into_iter()
                .map(|(key, value)| {
                    (
                        normalize_key(&key),
                        Value::String(value.trim_matches('"').to_string()),
                    )
                })
                .collect::<Map<_, _>>();
            Some((slot, Value::Object(value)))
        })
        .collect()
}

fn usb_inventory() -> Result<Value, String> {
    let entries = fs::read_dir("/sys/bus/usb/devices")
        .map_err(|error| format!("list USB devices: {error}"))?;
    let mut devices = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let vendor = read_trim(path.join("idVendor"))?;
            let product_id = read_trim(path.join("idProduct"))?;
            Some(json!({
                "sysfs_name": entry.file_name().to_string_lossy(),
                "vendor_id": vendor,
                "product_id": product_id,
                "manufacturer": read_trim(path.join("manufacturer")),
                "product": read_trim(path.join("product")),
                "serial": read_trim(path.join("serial")),
                "device_class": read_trim(path.join("bDeviceClass")),
                "device_subclass": read_trim(path.join("bDeviceSubClass")),
                "device_protocol": read_trim(path.join("bDeviceProtocol")),
                "speed_mbps": read_f64(path.join("speed")),
                "authorized": read_trim(path.join("authorized")).map(|value| value == "1"),
                "bus_number": read_u64(path.join("busnum")),
                "device_number": read_u64(path.join("devnum")),
                "driver": driver_name(&path),
            }))
        })
        .collect::<Vec<_>>();
    devices.sort_by(|left, right| {
        left["sysfs_name"]
            .as_str()
            .cmp(&right["sysfs_name"].as_str())
    });
    let count = devices.len();
    Ok(json!({
        "available": true,
        "devices": devices,
        "count": count,
    }))
}

async fn memory_inventory() -> Result<Value, String> {
    let meminfo = parse_meminfo()?;
    let modules = if let Some(dmidecode) = tool_path(&["/usr/sbin/dmidecode", "/usr/bin/dmidecode"])
    {
        match run_checked(dmidecode, &["--type", "17"], TOOL_TIMEOUT).await {
            Ok(output) => parse_memory_devices(&output.stdout),
            Err(error) => vec![json!({"available": false, "error": error})],
        }
    } else {
        vec![json!({"available": false, "error": "dmidecode is not installed"})]
    };
    let module_count = modules
        .iter()
        .filter(|module| {
            module["size"]
                .as_str()
                .is_some_and(|size| size != "No Module Installed")
        })
        .count();
    Ok(json!({
        "available": true,
        "meminfo_kib": meminfo,
        "modules": modules,
        "module_count": module_count,
    }))
}

fn parse_memory_devices(output: &str) -> Vec<Value> {
    output
        .split("\n\n")
        .filter(|block| block.contains("Memory Device"))
        .take(128)
        .map(|block| {
            let fields = parse_colon_fields(block);
            json!({
                "size": fields.get("Size"),
                "locator": fields.get("Locator"),
                "bank_locator": fields.get("Bank Locator"),
                "form_factor": fields.get("Form Factor"),
                "type": fields.get("Type"),
                "speed": fields.get("Speed"),
                "configured_speed": fields.get("Configured Memory Speed"),
                "manufacturer": fields.get("Manufacturer"),
                "serial": fields.get("Serial Number"),
                "part_number": fields.get("Part Number").map(|value| value.trim()),
            })
        })
        .collect()
}

fn parse_meminfo() -> Result<Map<String, Value>, String> {
    let data = fs::read_to_string("/proc/meminfo")
        .map_err(|error| format!("read /proc/meminfo: {error}"))?;
    Ok(data
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            let number = value.split_whitespace().next()?.parse::<u64>().ok()?;
            Some((normalize_key(key), Value::from(number)))
        })
        .collect())
}

async fn storage_inventory() -> Result<Value, String> {
    let lsblk = tool_path(&["/usr/bin/lsblk", "/bin/lsblk"])
        .ok_or_else(|| "lsblk is not installed".to_string())?;
    let output = run_checked(
        lsblk,
        &[
            "--json",
            "--bytes",
            "--paths",
            "--output",
            "PATH,NAME,KNAME,TYPE,PKNAME,SIZE,RO,RM,HOTPLUG,TRAN,FSTYPE,FSVER,LABEL,UUID,MOUNTPOINTS,MODEL,SERIAL,VENDOR,STATE",
        ],
        TOOL_TIMEOUT,
    )
    .await?;
    let value: Value = serde_json::from_str(&output.stdout)
        .map_err(|error| format!("parse lsblk JSON: {error}"))?;
    Ok(json!({
        "available": true,
        "blockdevices": value["blockdevices"],
    }))
}

fn driver_inventory() -> Result<Value, String> {
    let data = fs::read_to_string("/proc/modules")
        .map_err(|error| format!("read /proc/modules: {error}"))?;
    let modules = data
        .lines()
        .take(2048)
        .filter_map(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if fields.len() < 6 {
                return None;
            }
            Some(json!({
                "name": fields[0],
                "size_bytes": fields[1].parse::<u64>().ok(),
                "ref_count": fields[2].parse::<u64>().ok(),
                "dependencies": if fields[3] == "-" {
                    Vec::new()
                } else {
                    fields[3]
                        .split(',')
                        .filter(|dependency| !dependency.is_empty())
                        .collect::<Vec<_>>()
                },
                "state": fields[4],
                "address": fields[5],
                "taint": read_trim(Path::new("/sys/module").join(fields[0]).join("taint")),
                "version": read_trim(Path::new("/sys/module").join(fields[0]).join("version")),
            }))
        })
        .collect::<Vec<_>>();
    let count = modules.len();
    Ok(json!({
        "available": true,
        "kernel_release": read_trim("/proc/sys/kernel/osrelease"),
        "modules": modules,
        "count": count,
    }))
}

fn thermal_inventory() -> Result<Value, String> {
    let thermal_zones = fs::read_dir("/sys/class/thermal")
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_string();
            if !name.starts_with("thermal_zone") {
                return None;
            }
            let path = entry.path();
            Some(json!({
                "zone": name,
                "type": read_trim(path.join("type")),
                "temperature_c": read_i64(path.join("temp")).map(|value| value as f64 / 1000.0),
                "policy": read_trim(path.join("policy")),
            }))
        })
        .collect::<Vec<_>>();
    let hwmon = fs::read_dir("/sys/class/hwmon")
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| hwmon_device(&entry.path()))
        .collect::<Vec<_>>();
    Ok(json!({
        "available": true,
        "thermal_zones": thermal_zones,
        "hwmon": hwmon,
    }))
}

fn hwmon_device(path: &Path) -> Value {
    let mut temperatures = Vec::new();
    let mut fans = Vec::new();
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(index) = name
                .strip_prefix("temp")
                .and_then(|value| value.strip_suffix("_input"))
            {
                temperatures.push(json!({
                    "index": index,
                    "label": read_trim(path.join(format!("temp{index}_label"))),
                    "temperature_c": read_i64(entry.path()).map(|value| value as f64 / 1000.0),
                    "max_c": read_i64(path.join(format!("temp{index}_max"))).map(|value| value as f64 / 1000.0),
                    "critical_c": read_i64(path.join(format!("temp{index}_crit"))).map(|value| value as f64 / 1000.0),
                }));
            } else if let Some(index) = name
                .strip_prefix("fan")
                .and_then(|value| value.strip_suffix("_input"))
            {
                fans.push(json!({
                    "index": index,
                    "label": read_trim(path.join(format!("fan{index}_label"))),
                    "rpm": read_u64(entry.path()),
                }));
            }
        }
    }
    json!({
        "name": read_trim(path.join("name")),
        "temperatures": temperatures,
        "fans": fans,
    })
}

fn dmi_identity() -> Value {
    let root = Path::new("/sys/class/dmi/id");
    json!({
        "sys_vendor": read_trim(root.join("sys_vendor")),
        "product_name": read_trim(root.join("product_name")),
        "product_version": read_trim(root.join("product_version")),
        "product_serial": read_trim(root.join("product_serial")),
        "product_uuid": read_trim(root.join("product_uuid")),
        "board_vendor": read_trim(root.join("board_vendor")),
        "board_name": read_trim(root.join("board_name")),
        "board_version": read_trim(root.join("board_version")),
        "bios_vendor": read_trim(root.join("bios_vendor")),
        "bios_version": read_trim(root.join("bios_version")),
        "bios_date": read_trim(root.join("bios_date")),
    })
}

fn read_directory_values(path: &str) -> Value {
    let mut values = Map::new();
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            if let (Some(name), Some(value)) = (entry.file_name().to_str(), read_trim(entry.path()))
            {
                values.insert(name.to_string(), Value::String(value));
            }
        }
    }
    Value::Object(values)
}

fn driver_name(path: &Path) -> Option<String> {
    symlink_name(path.join("driver"))
}

fn symlink_name(path: impl AsRef<Path>) -> Option<String> {
    fs::read_link(path).ok().and_then(|path| {
        path.file_name()
            .map(|value| value.to_string_lossy().into_owned())
    })
}

fn parse_colon_fields(output: &str) -> BTreeMap<String, String> {
    output
        .lines()
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
        .collect()
}

fn normalize_key(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .replace(' ', "_")
        .replace('-', "_")
        .replace('/', "_")
        .replace('(', "_")
        .replace(')', "_")
        .replace(':', "_")
        .trim_matches('_')
        .to_string()
}

fn read_trim(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn read_u64(path: impl AsRef<Path>) -> Option<u64> {
    read_trim(path)?.parse().ok()
}

fn read_i64(path: impl AsRef<Path>) -> Option<i64> {
    read_trim(path)?.parse().ok()
}

fn read_f64(path: impl AsRef<Path>) -> Option<f64> {
    read_trim(path)?.parse().ok()
}

fn validate_action(action: &str) -> Result<(), String> {
    if matches!(
        action,
        "summary" | "cpu" | "gpu" | "pci" | "usb" | "memory" | "storage" | "drivers" | "thermal"
    ) {
        Ok(())
    } else {
        Err(format!("unknown hardware action: {action}"))
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
            .map_err(|error| format!("read hardware command output: {error}"))?;
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

fn required_string(params: &Value, key: &str) -> Result<String, String> {
    match params.get(key) {
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(value.trim().to_string()),
        Some(Value::String(_)) | None | Some(Value::Null) => {
            Err(format!("missing required string parameter: {key}"))
        }
        Some(_) => Err(format!("parameter `{key}` must be a string")),
    }
}

fn tool_path(candidates: &[&'static str]) -> Option<&'static str> {
    candidates
        .iter()
        .copied()
        .find(|candidate| Path::new(candidate).is_file())
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
    fn lspci_blocks_are_keyed_by_slot() {
        let parsed = parse_lspci_blocks(
            "Slot:\t\"0000:01:00.0\"\nClass:\t\"VGA compatible controller [0300]\"\nDriver:\t\"amdgpu\"\n",
        );
        assert_eq!(parsed["0000:01:00.0"]["driver"], "amdgpu");
    }

    #[test]
    fn hardware_keys_are_normalized() {
        assert_eq!(normalize_key("CPU max MHz:"), "cpu_max_mhz");
    }
}
