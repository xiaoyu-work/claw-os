use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::caps::{Cap, Scope, Verb};

use super::authority::{Authorized, Decision};
use super::client_identity::ClientIdentity;

const TOOL_TIMEOUT: Duration = Duration::from_secs(30);
const PAIR_TIMEOUT: Duration = Duration::from_secs(120);
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const STREAM_CAP_BYTES: usize = 1024 * 1024;
const MAX_SCAN_SECONDS: u64 = 60;
const MAX_DEVICES: usize = 100;
const PAIR_SESSION_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_PAIR_SESSIONS: usize = 8;
static BLUETOOTH_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
static PAIR_SESSIONS: OnceLock<StdMutex<BTreeMap<String, PairingSession>>> = OnceLock::new();

pub async fn control(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client, authority);
        return Err("Bluetooth Manager requires Linux BlueZ".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err("Bluetooth Manager requires root clawd".to_string());
        }
        let uid = client.require_uid()?;
        let action = required_string(&params, "action")?;
        let adapter = optional_string(&params, "adapter")?;
        let device = optional_string(&params, "device")?;
        let state = optional_string(&params, "state")?;
        let pairing_id = optional_string(&params, "pairing_id")?;
        let response = optional_string(&params, "response")?;
        let seconds = optional_u64(&params, "seconds")?;
        validate_action(
            &action,
            adapter.as_deref(),
            device.as_deref(),
            state.as_deref(),
            pairing_id.as_deref(),
            response.as_deref(),
            seconds,
        )?;
        let adapter = adapter.as_deref().map(normalize_address).transpose()?;
        let device = device.as_deref().map(normalize_address).transpose()?;
        let requested = if action == "status" {
            Cap::new(Verb::SYS_OBSERVE, Scope::name("bluetooth"))
        } else {
            Cap::new(Verb::DEVICE_BLUETOOTH, Scope::name("control"))
        };
        let _authorized = authorize_session(authority, requested)?;

        if action == "status" {
            return bluetooth_status().await;
        }
        if matches!(
            action.as_str(),
            "pair-status" | "pair-respond" | "pair-cancel"
        ) {
            return mutate(
                &action,
                adapter.as_deref(),
                device.as_deref(),
                state.as_deref(),
                pairing_id.as_deref(),
                response.as_deref(),
                seconds,
                uid,
            )
            .await;
        }
        let _guard = tokio::time::timeout(
            LOCK_TIMEOUT,
            BLUETOOTH_LOCK
                .get_or_init(|| tokio::sync::Mutex::new(()))
                .lock(),
        )
        .await
        .map_err(|_| "Bluetooth Manager is busy with another operation".to_string())?;
        mutate(
            &action,
            adapter.as_deref(),
            device.as_deref(),
            state.as_deref(),
            pairing_id.as_deref(),
            response.as_deref(),
            seconds,
            uid,
        )
        .await
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
    authority.require_app("bluetooth-manager")?;
    authority.require_all(std::slice::from_ref(&requested))
}

async fn bluetooth_status() -> Result<Value, String> {
    let adapters = adapter_records()?;
    let mut snapshots = Vec::new();
    let mut errors = Vec::new();
    for adapter in adapters {
        match adapter_snapshot(&adapter).await {
            Ok(snapshot) => snapshots.push(snapshot),
            Err(error) => errors.push(json!({
                "adapter": adapter.address,
                "error": error,
            })),
        }
    }
    let adapter_count = snapshots.len();
    Ok(json!({
        "provider": "bluez",
        "available": Path::new("/run/dbus/system_bus_socket").exists(),
        "adapters": snapshots,
        "adapter_count": adapter_count,
        "errors": errors,
    }))
}

async fn mutate(
    action: &str,
    adapter_address: Option<&str>,
    device_address: Option<&str>,
    state: Option<&str>,
    pairing_id: Option<&str>,
    response: Option<&str>,
    seconds: Option<u64>,
    owner_uid: u32,
) -> Result<Value, String> {
    cleanup_pairing_sessions().await?;
    match action {
        "pair-status" => {
            return poll_pairing(owner_uid, pairing_id.unwrap(), Duration::ZERO).await;
        }
        "pair-respond" => {
            return respond_pairing(owner_uid, pairing_id.unwrap(), response.unwrap()).await;
        }
        "pair-cancel" => {
            return cancel_pairing(owner_uid, pairing_id.unwrap()).await;
        }
        _ => {}
    }
    let adapter_address = adapter_address.expect("validated Bluetooth adapter address");
    let adapter = resolve_adapter(adapter_address)?;
    match action {
        "power" => {
            let before = adapter_state(&adapter).await?;
            let enabled = state == Some("on");
            run_busctl(
                &[
                    "set-property",
                    "org.bluez",
                    &adapter.object_path,
                    "org.bluez.Adapter1",
                    "Powered",
                    "b",
                    if enabled { "true" } else { "false" },
                ],
                TOOL_TIMEOUT,
            )
            .await?;
            let after = adapter_state(&adapter).await?;
            Ok(change_result(action, adapter_address, None, before, after))
        }
        "scan" => {
            let before = adapter_state(&adapter).await?;
            let duration = seconds.unwrap_or(10);
            let scan_output = run_scan(&adapter, duration).await?;
            let after = adapter_snapshot(&adapter).await?;
            Ok(json!({
                "action": action,
                "adapter": adapter_address,
                "duration_seconds": duration,
                "action_applied": true,
                "before": before,
                "after": after,
                "stdout_tail": tail(&scan_output.stdout),
                "stderr_tail": tail(&scan_output.stderr),
            }))
        }
        "pair" => {
            let device_address = device_address.expect("validated Bluetooth device address");
            let before = device_state(&adapter, device_address).await?;
            let pairing = start_pairing(owner_uid, &adapter, device_address).await?;
            Ok(json!({
                "action": action,
                "adapter": adapter_address,
                "device": device_address,
                "before": before,
                "pairing": pairing,
            }))
        }
        "connect" | "disconnect" | "trust" | "untrust" | "forget" => {
            let device_address = device_address.expect("validated Bluetooth device address");
            let path = device_object_path(&adapter, device_address);
            let before = device_state(&adapter, device_address).await?;
            let operation = match action {
                "connect" => {
                    run_busctl(
                        &["call", "org.bluez", &path, "org.bluez.Device1", "Connect"],
                        TOOL_TIMEOUT,
                    )
                    .await?
                }
                "disconnect" => {
                    run_busctl(
                        &[
                            "call",
                            "org.bluez",
                            &path,
                            "org.bluez.Device1",
                            "Disconnect",
                        ],
                        TOOL_TIMEOUT,
                    )
                    .await?
                }
                "trust" | "untrust" => {
                    run_busctl(
                        &[
                            "set-property",
                            "org.bluez",
                            &path,
                            "org.bluez.Device1",
                            "Trusted",
                            "b",
                            if action == "trust" { "true" } else { "false" },
                        ],
                        TOOL_TIMEOUT,
                    )
                    .await?
                }
                "forget" => {
                    run_busctl(
                        &[
                            "call",
                            "org.bluez",
                            &adapter.object_path,
                            "org.bluez.Adapter1",
                            "RemoveDevice",
                            "o",
                            &path,
                        ],
                        TOOL_TIMEOUT,
                    )
                    .await?
                }
                _ => unreachable!("validated Bluetooth action"),
            };
            let after = device_state(&adapter, device_address).await?;
            Ok(json!({
                "action": action,
                "adapter": adapter_address,
                "device": device_address,
                "changed": before != after,
                "action_applied": true,
                "before": before,
                "after": after,
                "stdout_tail": tail(&operation.stdout),
                "stderr_tail": tail(&operation.stderr),
            }))
        }
        _ => unreachable!("validated Bluetooth action"),
    }
}

fn change_result(
    action: &str,
    adapter: &str,
    device: Option<&str>,
    before: Value,
    after: Value,
) -> Value {
    json!({
        "action": action,
        "adapter": adapter,
        "device": device,
        "changed": before != after,
        "action_applied": true,
        "before": before,
        "after": after,
    })
}

#[derive(Clone)]
struct AdapterRecord {
    name: String,
    address: String,
    object_path: String,
}

fn adapter_records() -> Result<Vec<AdapterRecord>, String> {
    let mut adapters = Vec::new();
    let entries = match fs::read_dir("/sys/class/bluetooth") {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(adapters),
        Err(error) => return Err(format!("list Bluetooth adapters: {error}")),
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("hci") || !name[3..].bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let address = fs::read_to_string(entry.path().join("address"))
            .map_err(|error| format!("read Bluetooth adapter {name} address: {error}"))?;
        let address = normalize_address(address.trim())?;
        adapters.push(AdapterRecord {
            object_path: format!("/org/bluez/{name}"),
            name,
            address,
        });
    }
    adapters.sort_by(|left, right| left.name.cmp(&right.name));
    adapters.truncate(16);
    Ok(adapters)
}

fn resolve_adapter(address: &str) -> Result<AdapterRecord, String> {
    adapter_records()?
        .into_iter()
        .find(|adapter| adapter.address == address)
        .ok_or_else(|| format!("Bluetooth adapter not found: {address}"))
}

fn device_object_path(adapter: &AdapterRecord, address: &str) -> String {
    format!("{}/dev_{}", adapter.object_path, address.replace(':', "_"))
}

async fn adapter_snapshot(adapter: &AdapterRecord) -> Result<Value, String> {
    let discovery = run_bluetoothctl_script(
        &[
            format!("select {}", adapter.address),
            "devices".to_string(),
            "quit".to_string(),
        ],
        TOOL_TIMEOUT,
    )
    .await?;
    let addresses = parse_device_addresses(&discovery.stdout);
    let mut commands = vec![format!("select {}", adapter.address), "show".to_string()];
    for address in addresses.iter().take(MAX_DEVICES) {
        commands.push(format!("info {address}"));
    }
    commands.push("quit".to_string());
    let output = run_bluetoothctl_script(&commands, TOOL_TIMEOUT).await?;
    let (adapter_properties, devices) = parse_snapshot(&output.stdout, &adapter.address);
    let device_count = devices.len();
    Ok(json!({
        "name": adapter.name,
        "address": adapter.address,
        "object_path": adapter.object_path,
        "properties": adapter_properties,
        "devices": devices,
        "device_count": device_count,
        "stdout_truncated": output.stdout_truncated,
        "stderr_tail": tail(&output.stderr),
    }))
}

async fn adapter_state(adapter: &AdapterRecord) -> Result<Value, String> {
    let output = run_bluetoothctl_script(
        &[
            format!("select {}", adapter.address),
            "show".to_string(),
            "quit".to_string(),
        ],
        TOOL_TIMEOUT,
    )
    .await?;
    let (properties, _) = parse_snapshot(&output.stdout, &adapter.address);
    Ok(json!({
        "name": adapter.name,
        "address": adapter.address,
        "object_path": adapter.object_path,
        "properties": properties,
    }))
}

async fn device_state(adapter: &AdapterRecord, address: &str) -> Result<Value, String> {
    let output = run_bluetoothctl_script(
        &[
            format!("select {}", adapter.address),
            format!("info {address}"),
            "quit".to_string(),
        ],
        TOOL_TIMEOUT,
    )
    .await?;
    let (_, devices) = parse_snapshot(&output.stdout, &adapter.address);
    Ok(devices
        .into_iter()
        .find(|device| device["address"].as_str() == Some(address))
        .unwrap_or_else(|| {
            json!({
                "address": address,
                "present": false,
            })
        }))
}

fn parse_snapshot(output: &str, adapter_address: &str) -> (Value, Vec<Value>) {
    enum Section {
        None,
        Adapter,
        Device(String),
    }

    let mut section = Section::None;
    let mut adapter = Map::new();
    let mut devices = BTreeMap::<String, Map<String, Value>>::new();
    for raw in output.lines() {
        let line = normalize_bluetoothctl_line(raw);
        if let Some(rest) = line.strip_prefix("Controller ") {
            let address = rest.split_whitespace().next().unwrap_or_default();
            if address.eq_ignore_ascii_case(adapter_address) {
                section = Section::Adapter;
                adapter.insert(
                    "address".to_string(),
                    Value::String(adapter_address.to_string()),
                );
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("Device ") {
            let mut parts = rest.split_whitespace();
            let address = parts.next().unwrap_or_default();
            let Ok(address) = normalize_address(address) else {
                continue;
            };
            if matches!(parts.next(), Some("not" | "not-available")) {
                section = Section::None;
                continue;
            }
            section = Section::Device(address.clone());
            devices
                .entry(address.clone())
                .or_default()
                .insert("address".to_string(), Value::String(address));
            continue;
        }
        let Some((key, value)) = line.trim().split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        match &section {
            Section::Adapter => insert_property(&mut adapter, key, value),
            Section::Device(address) => {
                insert_property(devices.entry(address.clone()).or_default(), key, value)
            }
            Section::None => {}
        }
    }
    let devices = devices
        .into_values()
        .map(|mut properties| {
            properties.insert("present".to_string(), Value::Bool(true));
            Value::Object(properties)
        })
        .collect();
    (Value::Object(adapter), devices)
}

fn insert_property(properties: &mut Map<String, Value>, key: &str, value: &str) {
    let normalized_key = key.to_ascii_lowercase().replace(' ', "_");
    let parsed = match value.to_ascii_lowercase().as_str() {
        "yes" => Value::Bool(true),
        "no" => Value::Bool(false),
        _ if matches!(normalized_key.as_str(), "rssi" | "txpower") => value
            .parse::<i64>()
            .map(Value::from)
            .unwrap_or_else(|_| Value::String(value.to_string())),
        _ => Value::String(value.to_string()),
    };
    if normalized_key == "uuid" {
        properties
            .entry("uuids".to_string())
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .expect("uuids initialized as array")
            .push(parsed);
    } else {
        properties.insert(normalized_key, parsed);
    }
}

fn parse_device_addresses(output: &str) -> BTreeSet<String> {
    output
        .lines()
        .filter_map(|line| {
            let line = normalize_bluetoothctl_line(line);
            let rest = line.strip_prefix("Device ")?;
            normalize_address(rest.split_whitespace().next()?).ok()
        })
        .collect()
}

fn normalize_bluetoothctl_line(line: &str) -> String {
    let line = strip_ansi(line).trim().to_string();
    let line = line
        .split_once("]# ")
        .map(|(_, rest)| rest.to_string())
        .unwrap_or(line);
    if line.starts_with('[') {
        if let Some((_, rest)) = line.split_once("] ") {
            return rest.trim().to_string();
        }
    }
    line
}

fn strip_ansi(value: &str) -> String {
    let mut output = String::new();
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character != '\u{1b}' {
            output.push(character);
            continue;
        }
        if chars.next() != Some('[') {
            continue;
        }
        for next in chars.by_ref() {
            if ('@'..='~').contains(&next) {
                break;
            }
        }
    }
    output
}

#[derive(Clone, serde::Serialize)]
struct PairPrompt {
    kind: String,
    message: String,
}

#[derive(Default)]
struct PairShared {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_truncated: bool,
    stderr_truncated: bool,
    prompt: Option<PairPrompt>,
    last_prompt: Option<String>,
}

struct PairingSession {
    owner_uid: u32,
    adapter_address: String,
    adapter_object_path: String,
    device_address: String,
    created_at: Instant,
    child: Child,
    stdin: ChildStdin,
    shared: Arc<StdMutex<PairShared>>,
    _stdout_reader: JoinHandle<Result<(), String>>,
    _stderr_reader: JoinHandle<Result<(), String>>,
    quit_sent: bool,
    canceled: bool,
}

impl Drop for PairingSession {
    fn drop(&mut self) {
        self.stop();
    }
}

impl PairingSession {
    fn stop(&mut self) {
        let _ = writeln!(self.stdin, "quit");
        let _ = self.stdin.flush();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn start_pairing(
    owner_uid: u32,
    adapter: &AdapterRecord,
    device_address: &str,
) -> Result<Value, String> {
    {
        let mut sessions = pairing_sessions()
            .lock()
            .map_err(|_| "Bluetooth pairing session lock is poisoned".to_string())?;
        if sessions.len() >= MAX_PAIR_SESSIONS {
            return Err("too many active Bluetooth pairing sessions".to_string());
        }
        if sessions.values_mut().any(|existing| {
            existing.adapter_address == adapter.address && pairing_session_active(existing)
        }) {
            return Err(format!(
                "adapter {} already has an active pairing session",
                adapter.address
            ));
        }
    }
    let adapter = adapter.clone();
    let device_address = device_address.to_string();
    let (id, session) = tokio::task::spawn_blocking(move || {
        start_pairing_sync(owner_uid, &adapter, &device_address)
    })
    .await
    .map_err(|error| format!("Bluetooth pairing worker failed: {error}"))??;
    {
        let mut sessions = pairing_sessions()
            .lock()
            .map_err(|_| "Bluetooth pairing session lock is poisoned".to_string())?;
        sessions.insert(id.clone(), session);
    }
    poll_pairing(owner_uid, &id, Duration::from_secs(5)).await
}

fn start_pairing_sync(
    owner_uid: u32,
    adapter: &AdapterRecord,
    device_address: &str,
) -> Result<(String, PairingSession), String> {
    let program = bluetoothctl_path()?;
    let mut command = command_base(program);
    command
        .arg("--timeout")
        .arg(PAIR_TIMEOUT.as_secs().to_string())
        .stdin(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to launch bluetoothctl pairing agent: {error}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "bluetoothctl pairing stdin is unavailable".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "bluetoothctl pairing stdout is unavailable".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "bluetoothctl pairing stderr is unavailable".to_string())?;
    let shared = Arc::new(StdMutex::new(PairShared::default()));
    let stdout_shared = shared.clone();
    let stderr_shared = shared.clone();
    let stdout_reader = std::thread::spawn(move || pair_reader(stdout, stdout_shared, true));
    let stderr_reader = std::thread::spawn(move || pair_reader(stderr, stderr_shared, false));
    writeln!(stdin, "select {}", adapter.address)
        .and_then(|_| writeln!(stdin, "agent KeyboardDisplay"))
        .and_then(|_| writeln!(stdin, "pair {device_address}"))
        .and_then(|_| stdin.flush())
        .map_err(|error| format!("start Bluetooth pairing: {error}"))?;
    let id = uuid::Uuid::new_v4().simple().to_string();
    Ok((
        id,
        PairingSession {
            owner_uid,
            adapter_address: adapter.address.clone(),
            adapter_object_path: adapter.object_path.clone(),
            device_address: device_address.to_string(),
            created_at: Instant::now(),
            child,
            stdin,
            shared,
            _stdout_reader: stdout_reader,
            _stderr_reader: stderr_reader,
            quit_sent: false,
            canceled: false,
        },
    ))
}

fn pair_reader(
    mut reader: impl Read,
    shared: Arc<StdMutex<PairShared>>,
    stdout: bool,
) -> Result<(), String> {
    let mut buffer = [0_u8; 4096];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("read Bluetooth pairing output: {error}"))?;
        if read == 0 {
            return Ok(());
        }
        let mut shared = shared
            .lock()
            .map_err(|_| "Bluetooth pairing output lock is poisoned".to_string())?;
        if stdout {
            let remaining = STREAM_CAP_BYTES.saturating_sub(shared.stdout.len());
            let keep = remaining.min(read);
            shared.stdout.extend_from_slice(&buffer[..keep]);
            shared.stdout_truncated |= keep < read;
        } else {
            let remaining = STREAM_CAP_BYTES.saturating_sub(shared.stderr.len());
            let keep = remaining.min(read);
            shared.stderr.extend_from_slice(&buffer[..keep]);
            shared.stderr_truncated |= keep < read;
        }
        if stdout {
            let text = strip_ansi(&String::from_utf8_lossy(&shared.stdout));
            if let Some(prompt) = detect_pair_prompt(&text) {
                if shared.last_prompt.as_deref() != Some(prompt.message.as_str()) {
                    shared.last_prompt = Some(prompt.message.clone());
                    shared.prompt = Some(prompt);
                }
            }
        }
    }
}

fn detect_pair_prompt(output: &str) -> Option<PairPrompt> {
    let (index, _, kind) = [
        ("Confirm passkey", "confirmation"),
        ("Authorize service", "authorization"),
        ("Enter PIN code", "pin-code"),
        ("Enter passkey", "passkey"),
    ]
    .into_iter()
    .filter_map(|(needle, kind)| output.rfind(needle).map(|index| (index, needle, kind)))
    .max_by_key(|(index, _, _)| *index)?;
    let message = output[index..]
        .lines()
        .next()
        .unwrap_or(&output[index..])
        .trim()
        .to_string();
    (!message.is_empty()).then(|| PairPrompt {
        kind: kind.to_string(),
        message,
    })
}

async fn poll_pairing(owner_uid: u32, id: &str, wait: Duration) -> Result<Value, String> {
    let deadline = Instant::now() + wait;
    loop {
        let value = {
            let mut sessions = pairing_sessions()
                .lock()
                .map_err(|_| "Bluetooth pairing session lock is poisoned".to_string())?;
            let session = sessions
                .get_mut(id)
                .ok_or_else(|| format!("Bluetooth pairing session not found: {id}"))?;
            if session.owner_uid != owner_uid {
                return Err("Bluetooth pairing session belongs to another user".to_string());
            }
            pairing_value(id, session)?
        };
        let terminal = matches!(
            value["status"].as_str(),
            Some("paired" | "failed" | "canceled")
        );
        if terminal || value["needs_response"].as_bool() == Some(true) || Instant::now() >= deadline
        {
            return Ok(value);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn pairing_value(id: &str, session: &mut PairingSession) -> Result<Value, String> {
    let shared = session
        .shared
        .lock()
        .map_err(|_| "Bluetooth pairing output lock is poisoned".to_string())?;
    let stdout = String::from_utf8_lossy(&shared.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&shared.stderr).into_owned();
    let prompt = shared.prompt.clone();
    let paired = stdout.contains("Pairing successful");
    let failed = [
        "Failed to pair",
        "AuthenticationFailed",
        "AuthenticationRejected",
        "AuthenticationCanceled",
    ]
    .iter()
    .any(|needle| stdout.contains(needle) || stderr.contains(needle));
    drop(shared);
    let exited = session
        .child
        .try_wait()
        .map_err(|error| format!("inspect Bluetooth pairing process: {error}"))?;
    let status = if session.canceled {
        "canceled"
    } else if paired {
        "paired"
    } else if failed || exited.is_some() {
        "failed"
    } else {
        "pending"
    };
    if status != "pending" && !session.quit_sent && exited.is_none() {
        let _ = writeln!(session.stdin, "quit");
        let _ = session.stdin.flush();
        session.quit_sent = true;
    }
    let shared = session
        .shared
        .lock()
        .map_err(|_| "Bluetooth pairing output lock is poisoned".to_string())?;
    Ok(json!({
        "pairing_id": id,
        "status": status,
        "adapter": session.adapter_address,
        "device": session.device_address,
        "needs_response": status == "pending" && prompt.is_some(),
        "prompt": prompt,
        "stdout_tail": tail(&String::from_utf8_lossy(&shared.stdout)),
        "stderr_tail": tail(&String::from_utf8_lossy(&shared.stderr)),
        "stdout_truncated": shared.stdout_truncated,
        "stderr_truncated": shared.stderr_truncated,
        "exit_code": exited.and_then(|status| status.code()),
    }))
}

async fn respond_pairing(owner_uid: u32, id: &str, response: &str) -> Result<Value, String> {
    {
        let mut sessions = pairing_sessions()
            .lock()
            .map_err(|_| "Bluetooth pairing session lock is poisoned".to_string())?;
        let session = sessions
            .get_mut(id)
            .ok_or_else(|| format!("Bluetooth pairing session not found: {id}"))?;
        if session.owner_uid != owner_uid {
            return Err("Bluetooth pairing session belongs to another user".to_string());
        }
        let prompt = session
            .shared
            .lock()
            .map_err(|_| "Bluetooth pairing output lock is poisoned".to_string())?
            .prompt
            .clone()
            .ok_or_else(|| "Bluetooth pairing session is not waiting for a response".to_string())?;
        validate_pair_response(&prompt, response)?;
        writeln!(session.stdin, "{response}")
            .and_then(|_| session.stdin.flush())
            .map_err(|error| format!("send Bluetooth pairing response: {error}"))?;
        session
            .shared
            .lock()
            .map_err(|_| "Bluetooth pairing output lock is poisoned".to_string())?
            .prompt = None;
    }
    poll_pairing(owner_uid, id, Duration::from_secs(5)).await
}

fn validate_pair_response(prompt: &PairPrompt, response: &str) -> Result<(), String> {
    if response.is_empty()
        || response.len() > 32
        || response.chars().any(|character| character.is_control())
    {
        return Err("invalid Bluetooth pairing response".to_string());
    }
    match prompt.kind.as_str() {
        "confirmation" | "authorization"
            if matches!(response.to_ascii_lowercase().as_str(), "yes" | "no") =>
        {
            Ok(())
        }
        "pin-code"
            if response.len() <= 16
                && response.bytes().all(|byte| byte.is_ascii_alphanumeric()) =>
        {
            Ok(())
        }
        "passkey"
            if response.len() <= 6
                && response.bytes().all(|byte| byte.is_ascii_digit())
                && response.parse::<u32>().is_ok_and(|value| value <= 999_999) =>
        {
            Ok(())
        }
        "confirmation" | "authorization" => Err("pairing response must be yes or no".to_string()),
        "pin-code" => Err("pairing PIN must be 1-16 ASCII letters or digits".to_string()),
        "passkey" => Err("pairing passkey must be a number from 0 to 999999".to_string()),
        _ => Err("unsupported Bluetooth pairing prompt".to_string()),
    }
}

async fn cancel_pairing(owner_uid: u32, id: &str) -> Result<Value, String> {
    let (adapter_path, device_address, adapter_address) = {
        let mut sessions = pairing_sessions()
            .lock()
            .map_err(|_| "Bluetooth pairing session lock is poisoned".to_string())?;
        let session = sessions
            .get_mut(id)
            .ok_or_else(|| format!("Bluetooth pairing session not found: {id}"))?;
        if session.owner_uid != owner_uid {
            return Err("Bluetooth pairing session belongs to another user".to_string());
        }
        session.canceled = true;
        (
            session.adapter_object_path.clone(),
            session.device_address.clone(),
            session.adapter_address.clone(),
        )
    };
    let path = format!("{adapter_path}/dev_{}", device_address.replace(':', "_"));
    let _ = run_busctl(
        &[
            "call",
            "org.bluez",
            &path,
            "org.bluez.Device1",
            "CancelPairing",
        ],
        TOOL_TIMEOUT,
    )
    .await;
    let session = pairing_sessions()
        .lock()
        .map_err(|_| "Bluetooth pairing session lock is poisoned".to_string())?
        .remove(id);
    if let Some(mut session) = session {
        session = tokio::task::spawn_blocking(move || {
            session.stop();
            session
        })
        .await
        .map_err(|error| format!("Bluetooth pairing cancel worker failed: {error}"))?;
        pairing_sessions()
            .lock()
            .map_err(|_| "Bluetooth pairing session lock is poisoned".to_string())?
            .insert(id.to_string(), session);
    }
    Ok(json!({
        "pairing_id": id,
        "status": "canceled",
        "adapter": adapter_address,
        "device": device_address,
    }))
}

async fn cleanup_pairing_sessions() -> Result<(), String> {
    let removed = {
        let mut sessions = pairing_sessions()
            .lock()
            .map_err(|_| "Bluetooth pairing session lock is poisoned".to_string())?;
        let mut expired = Vec::new();
        for (id, session) in sessions.iter_mut() {
            if session.created_at.elapsed() > PAIR_SESSION_TTL
                || (!pairing_session_active(session)
                    && session.created_at.elapsed() > Duration::from_secs(30))
            {
                expired.push(id.clone());
            }
        }
        expired
            .into_iter()
            .filter_map(|id| sessions.remove(&id))
            .collect::<Vec<_>>()
    };
    if !removed.is_empty() {
        tokio::task::spawn_blocking(move || drop(removed))
            .await
            .map_err(|error| format!("Bluetooth pairing cleanup worker failed: {error}"))?;
    }
    Ok(())
}

fn pairing_sessions() -> &'static StdMutex<BTreeMap<String, PairingSession>> {
    PAIR_SESSIONS.get_or_init(|| StdMutex::new(BTreeMap::new()))
}

fn pairing_session_active(session: &mut PairingSession) -> bool {
    if session.canceled || session.created_at.elapsed() > PAIR_SESSION_TTL {
        return false;
    }
    let terminal_output = session.shared.lock().ok().is_some_and(|shared| {
        let stdout = String::from_utf8_lossy(&shared.stdout);
        let stderr = String::from_utf8_lossy(&shared.stderr);
        stdout.contains("Pairing successful")
            || [
                "Failed to pair",
                "AuthenticationFailed",
                "AuthenticationRejected",
            ]
            .iter()
            .any(|needle| stdout.contains(needle) || stderr.contains(needle))
    });
    !terminal_output && session.child.try_wait().ok().flatten().is_none()
}

async fn run_busctl(args: &[&str], timeout: Duration) -> Result<CommandOutput, String> {
    run_command(
        busctl_path()?,
        args.iter().map(|value| value.to_string()).collect(),
        None,
        timeout,
    )
    .await
}

async fn run_bluetoothctl_script(
    commands: &[String],
    timeout: Duration,
) -> Result<CommandOutput, String> {
    let mut script = commands.join("\n");
    script.push('\n');
    run_command(
        bluetoothctl_path()?,
        vec!["--timeout".to_string(), timeout.as_secs().to_string()],
        Some(script.into_bytes()),
        timeout + Duration::from_secs(5),
    )
    .await
}

async fn run_scan(adapter: &AdapterRecord, seconds: u64) -> Result<CommandOutput, String> {
    let adapter = adapter.clone();
    tokio::task::spawn_blocking(move || run_scan_sync(&adapter, seconds))
        .await
        .map_err(|error| format!("Bluetooth scan worker failed: {error}"))?
}

fn run_scan_sync(adapter: &AdapterRecord, seconds: u64) -> Result<CommandOutput, String> {
    let program = bluetoothctl_path()?;
    let mut command = command_base(program);
    command
        .arg("--timeout")
        .arg((seconds + 10).to_string())
        .stdin(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to launch bluetoothctl: {error}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "bluetoothctl stdin is unavailable".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "bluetoothctl stdout is unavailable".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "bluetoothctl stderr is unavailable".to_string())?;
    let stdout_reader = std::thread::spawn(move || read_bounded(stdout));
    let stderr_reader = std::thread::spawn(move || read_bounded(stderr));
    writeln!(stdin, "select {}", adapter.address)
        .and_then(|_| writeln!(stdin, "scan on"))
        .and_then(|_| stdin.flush())
        .map_err(|error| format!("start Bluetooth scan: {error}"))?;
    std::thread::sleep(Duration::from_secs(seconds));
    let _ = writeln!(stdin, "scan off");
    let _ = writeln!(stdin, "quit");
    let _ = stdin.flush();
    drop(stdin);
    finish_child(
        child,
        stdout_reader,
        stderr_reader,
        Duration::from_secs(seconds + 10),
        program,
    )
}

async fn run_command(
    program: &'static str,
    args: Vec<String>,
    stdin: Option<Vec<u8>>,
    timeout: Duration,
) -> Result<CommandOutput, String> {
    tokio::task::spawn_blocking(move || run_command_sync(program, args, stdin, timeout))
        .await
        .map_err(|error| format!("{program} worker failed: {error}"))?
}

fn run_command_sync(
    program: &'static str,
    args: Vec<String>,
    stdin_data: Option<Vec<u8>>,
    timeout: Duration,
) -> Result<CommandOutput, String> {
    let mut command = command_base(program);
    command.args(args).stdin(if stdin_data.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
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
    finish_child(child, stdout_reader, stderr_reader, timeout, program)
}

fn command_base(program: &str) -> Command {
    let mut command = Command::new(program);
    command
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("HOME", "/root")
        .env("LC_ALL", "C.UTF-8")
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .env(
            "DBUS_SYSTEM_BUS_ADDRESS",
            "unix:path=/run/dbus/system_bus_socket",
        )
        .current_dir("/")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn finish_child(
    mut child: std::process::Child,
    stdout_reader: std::thread::JoinHandle<Result<(Vec<u8>, bool), String>>,
    stderr_reader: std::thread::JoinHandle<Result<(Vec<u8>, bool), String>>,
    timeout: Duration,
    program: &str,
) -> Result<CommandOutput, String> {
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
            .map_err(|error| format!("read Bluetooth command output: {error}"))?;
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

fn validate_action(
    action: &str,
    adapter: Option<&str>,
    device: Option<&str>,
    state: Option<&str>,
    pairing_id: Option<&str>,
    response: Option<&str>,
    seconds: Option<u64>,
) -> Result<(), String> {
    match action {
        "status"
            if adapter.is_none()
                && device.is_none()
                && state.is_none()
                && pairing_id.is_none()
                && response.is_none()
                && seconds.is_none() =>
        {
            Ok(())
        }
        "power"
            if adapter.is_some()
                && device.is_none()
                && matches!(state, Some("on" | "off"))
                && pairing_id.is_none()
                && response.is_none()
                && seconds.is_none() =>
        {
            Ok(())
        }
        "scan"
            if adapter.is_some()
                && device.is_none()
                && state.is_none()
                && pairing_id.is_none()
                && response.is_none()
                && seconds.unwrap_or(10) <= MAX_SCAN_SECONDS
                && seconds.unwrap_or(10) > 0 =>
        {
            Ok(())
        }
        "pair" | "connect" | "disconnect" | "trust" | "untrust" | "forget"
            if adapter.is_some()
                && device.is_some()
                && state.is_none()
                && pairing_id.is_none()
                && response.is_none()
                && seconds.is_none() =>
        {
            Ok(())
        }
        "pair-status" | "pair-cancel"
            if adapter.is_none()
                && device.is_none()
                && state.is_none()
                && pairing_id.is_some_and(valid_pairing_id)
                && response.is_none()
                && seconds.is_none() =>
        {
            Ok(())
        }
        "pair-respond"
            if adapter.is_none()
                && device.is_none()
                && state.is_none()
                && pairing_id.is_some_and(valid_pairing_id)
                && response.is_some()
                && seconds.is_none() =>
        {
            Ok(())
        }
        "status" => Err("status does not accept arguments".to_string()),
        "power" => Err("power requires <adapter> on|off".to_string()),
        "scan" => Err(format!(
            "scan requires <adapter> [seconds], where seconds is 1..{MAX_SCAN_SECONDS}"
        )),
        "pair" | "connect" | "disconnect" | "trust" | "untrust" | "forget" => {
            Err(format!("{action} requires <adapter> <device>"))
        }
        "pair-status" | "pair-cancel" => Err(format!("{action} requires <pairing-id>")),
        "pair-respond" => Err("pair-respond requires <pairing-id> and response".to_string()),
        _ => Err(format!("unknown Bluetooth action: {action}")),
    }
}

fn valid_pairing_id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn normalize_address(value: &str) -> Result<String, String> {
    let parts = value.split(':').collect::<Vec<_>>();
    if parts.len() != 6
        || parts
            .iter()
            .any(|part| part.len() != 2 || !part.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err(format!("invalid Bluetooth address: {value:?}"));
    }
    Ok(parts
        .into_iter()
        .map(str::to_ascii_uppercase)
        .collect::<Vec<_>>()
        .join(":"))
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

fn tool_path(candidates: &[&'static str], name: &str) -> Result<&'static str, String> {
    candidates
        .iter()
        .copied()
        .find(|candidate| Path::new(candidate).is_file())
        .ok_or_else(|| format!("{name} is not installed"))
}

fn busctl_path() -> Result<&'static str, String> {
    tool_path(&["/usr/bin/busctl", "/bin/busctl"], "busctl")
}

fn bluetoothctl_path() -> Result<&'static str, String> {
    tool_path(
        &["/usr/bin/bluetoothctl", "/bin/bluetoothctl"],
        "bluetoothctl",
    )
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
        "/test/unit/clawd/bluetooth.rs"
    ));
}
