use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::caps::{Cap, CapSet, Scope, Verb};

use super::client_identity::ClientIdentity;

const TABLE_FAMILY: &str = "inet";
const TABLE_NAME: &str = "claw_agent";
const TOOL_TIMEOUT: Duration = Duration::from_secs(60);
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const STREAM_CAP_BYTES: usize = 2 * 1024 * 1024;
static FIREWALL_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

#[derive(Clone, Serialize, Deserialize)]
struct FirewallState {
    schema: u32,
    revision: String,
    rules: Vec<FirewallRule>,
}

impl Default for FirewallState {
    fn default() -> Self {
        Self {
            schema: 1,
            revision: "initial".to_string(),
            rules: Vec::new(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FirewallRule {
    id: String,
    action: String,
    direction: String,
    protocol: String,
    port: u16,
    remote: Option<String>,
    interface: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct FirewallBackup {
    token: String,
    owner_uid: u32,
    created_at: String,
    applied_revision: String,
    previous: FirewallState,
    status: String,
}

pub async fn reconcile_on_start() -> Result<(), String> {
    if !state_path().exists() {
        return Ok(());
    }
    let state = load_state()?;
    apply_live_state(&state).await
}

pub async fn control(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client);
        return Err("Firewall Manager requires Linux nftables".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err("Firewall Manager requires root clawd".to_string());
        }
        let uid = client.require_uid()?;
        let home = client.require_home_dir()?;
        let peer_pid = client
            .pid
            .ok_or_else(|| "clawd peer pid is unavailable".to_string())?;
        let session_id = required_string(&params, "session")?;
        let action = required_string(&params, "action")?;
        let rule_action = optional_string(&params, "rule_action")?;
        let direction = optional_string(&params, "direction")?;
        let protocol = optional_string(&params, "protocol")?;
        let port = optional_u64(&params, "port")?;
        let remote = optional_string(&params, "remote")?;
        let interface = optional_string(&params, "interface")?;
        let rule_id = optional_string(&params, "rule_id")?;
        let token = optional_string(&params, "token")?;
        let confirm = params
            .get("confirm")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        validate_action(
            &action,
            rule_action.as_deref(),
            direction.as_deref(),
            protocol.as_deref(),
            port,
            remote.as_deref(),
            interface.as_deref(),
            rule_id.as_deref(),
            token.as_deref(),
            confirm,
        )?;
        let requested = if action == "status" {
            Cap::new(Verb::SYS_OBSERVE, Scope::name("firewall"))
        } else {
            Cap::new(Verb::NET_FIREWALL, Scope::name("manage"))
        };
        crate::paths::with_user_override(uid, home, async {
            authorize_session(&session_id, peer_pid, requested)
        })
        .await?;

        if action == "status" {
            return firewall_status().await;
        }
        let _guard = tokio::time::timeout(
            LOCK_TIMEOUT,
            FIREWALL_LOCK
                .get_or_init(|| tokio::sync::Mutex::new(()))
                .lock(),
        )
        .await
        .map_err(|_| "Firewall Manager is busy with another mutation".to_string())?;
        match action.as_str() {
            "add" => {
                add_rule(
                    uid,
                    rule_action.as_deref().unwrap(),
                    direction.as_deref().unwrap(),
                    protocol.as_deref().unwrap(),
                    port.unwrap() as u16,
                    remote.as_deref(),
                    interface.as_deref(),
                )
                .await
            }
            "delete" => delete_rule(uid, rule_id.as_deref().unwrap()).await,
            "clear" => clear_rules(uid).await,
            "restore" => restore_rules(uid, token.as_deref().unwrap()).await,
            _ => unreachable!("validated firewall action"),
        }
    }
}

fn authorize_session(session_id: &str, peer_pid: u32, requested: Cap) -> Result<(), String> {
    let session = crate::proc::session_info_by_id(session_id)
        .ok_or_else(|| format!("firewall-manager session not found: {session_id}"))?;
    if session.app_id.as_deref() != Some("firewall-manager") {
        return Err("firewall control is restricted to the firewall-manager App".to_string());
    }
    if session.pending_bind || session.pid == 0 {
        return Err("firewall-manager session is not bound to a process".to_string());
    }
    let expected_start = session
        .start_time_ticks
        .ok_or_else(|| "firewall-manager session has no process identity".to_string())?;
    if crate::proc::read_start_time_ticks_pub(session.pid) != Some(expected_start) {
        return Err("firewall-manager session process identity is stale".to_string());
    }
    if !crate::proc::process_descends_from(peer_pid, session.pid) {
        return Err("firewall request did not originate from the authorized session".to_string());
    }
    let mut caps = session.caps.unwrap_or_else(CapSet::new);
    if let Some(transient) = session.transient_caps {
        caps.extend(transient.iter().cloned());
    }
    if !caps.covers(&requested) {
        return Err(format!(
            "firewall-manager session lacks {}:{}",
            requested.verb.as_str(),
            requested.scope
        ));
    }
    Ok(())
}

async fn firewall_status() -> Result<Value, String> {
    let state = load_state()?;
    let live = list_live_table().await;
    let desired_ids = state
        .rules
        .iter()
        .map(|rule| rule.id.clone())
        .collect::<BTreeSet<_>>();
    let live_ids = live
        .as_ref()
        .ok()
        .map(extract_live_rule_ids)
        .unwrap_or_default();
    Ok(json!({
        "available": nft_path().is_ok(),
        "table": format!("{TABLE_FAMILY} {TABLE_NAME}"),
        "state": state,
        "live": live.unwrap_or_else(|error| json!({"available": false, "error": error})),
        "drift": {
            "missing_live_rules": desired_ids.difference(&live_ids).collect::<Vec<_>>(),
            "unexpected_live_rules": live_ids.difference(&desired_ids).collect::<Vec<_>>(),
        },
    }))
}

async fn add_rule(
    owner_uid: u32,
    action: &str,
    direction: &str,
    protocol: &str,
    port: u16,
    remote: Option<&str>,
    interface: Option<&str>,
) -> Result<Value, String> {
    let mut state = load_state()?;
    let rule = FirewallRule {
        id: uuid::Uuid::new_v4().simple().to_string(),
        action: action.to_string(),
        direction: direction.to_string(),
        protocol: protocol.to_string(),
        port,
        remote: remote.map(normalize_cidr).transpose()?,
        interface: interface.map(validate_interface).transpose()?,
    };
    if state.rules.iter().any(|existing| {
        existing.action == rule.action
            && existing.direction == rule.direction
            && existing.protocol == rule.protocol
            && existing.port == rule.port
            && existing.remote == rule.remote
            && existing.interface == rule.interface
    }) {
        return Err("an identical managed firewall rule already exists".to_string());
    }
    state.rules.push(rule.clone());
    apply_mutation(owner_uid, state, "add", Some(rule)).await
}

async fn delete_rule(owner_uid: u32, id: &str) -> Result<Value, String> {
    validate_id(id, "rule id")?;
    let mut state = load_state()?;
    let index = state
        .rules
        .iter()
        .position(|rule| rule.id == id)
        .ok_or_else(|| format!("managed firewall rule not found: {id}"))?;
    let rule = state.rules.remove(index);
    apply_mutation(owner_uid, state, "delete", Some(rule)).await
}

async fn clear_rules(owner_uid: u32) -> Result<Value, String> {
    let mut state = load_state()?;
    state.rules.clear();
    apply_mutation(owner_uid, state, "clear", None).await
}

async fn apply_mutation(
    owner_uid: u32,
    mut next: FirewallState,
    action: &str,
    rule: Option<FirewallRule>,
) -> Result<Value, String> {
    let previous = load_state()?;
    next.revision = uuid::Uuid::new_v4().simple().to_string();
    let backup = create_backup(owner_uid, previous.clone(), &next.revision)?;
    apply_live_state(&next).await?;
    if let Err(error) = save_state(&next) {
        let rollback = apply_live_state(&previous).await;
        return match rollback {
            Ok(()) => Err(format!(
                "firewall state persistence failed and live rules were restored: {error}"
            )),
            Err(rollback_error) => Err(format!(
                "firewall state persistence failed ({error}) and live rollback failed ({rollback_error}); backup token: {}",
                backup.token
            )),
        };
    }
    let live = list_live_table().await?;
    let expected = next
        .rules
        .iter()
        .map(|rule| rule.id.clone())
        .collect::<BTreeSet<_>>();
    let actual = extract_live_rule_ids(&live);
    if expected != actual {
        let live_rollback = apply_live_state(&previous).await;
        let state_rollback = save_state(&previous);
        return match (live_rollback, state_rollback) {
            (Ok(()), Ok(())) => Err(
                "live nftables rules did not match the requested state; previous rules were restored"
                    .to_string(),
            ),
            (live, state) => Err(format!(
                "live nftables drift was detected and rollback was incomplete (live: {}; state: {}); backup token: {}",
                live.err().unwrap_or_else(|| "ok".to_string()),
                state.err().unwrap_or_else(|| "ok".to_string()),
                backup.token
            )),
        };
    }
    let mut applied_backup = backup.clone();
    applied_backup.status = "applied".to_string();
    save_backup(&applied_backup)?;
    Ok(json!({
        "action": action,
        "rule": rule,
        "revision": next.revision,
        "backup_token": backup.token,
        "rules": next.rules,
        "live": live,
    }))
}

async fn restore_rules(owner_uid: u32, token: &str) -> Result<Value, String> {
    validate_id(token, "backup token")?;
    let mut backup = load_backup(token)?;
    if backup.owner_uid != owner_uid {
        return Err("firewall backup belongs to another user".to_string());
    }
    let current = load_state()?;
    if current.revision != backup.applied_revision {
        return Err("managed firewall state changed after this backup was created".to_string());
    }
    apply_live_state(&backup.previous).await?;
    if let Err(error) = save_state(&backup.previous) {
        let rollback = apply_live_state(&current).await;
        return match rollback {
            Ok(()) => Err(format!(
                "firewall restore persistence failed and current live rules were restored: {error}"
            )),
            Err(rollback_error) => Err(format!(
                "firewall restore persistence failed ({error}) and live rollback failed ({rollback_error})"
            )),
        };
    }
    backup.status = "restored".to_string();
    save_backup(&backup)?;
    Ok(json!({
        "restored": true,
        "backup_token": token,
        "revision": backup.previous.revision,
        "rules": backup.previous.rules,
    }))
}

async fn apply_live_state(state: &FirewallState) -> Result<(), String> {
    ensure_table().await?;
    let script = render_ruleset(state)?;
    let nft = nft_path()?;
    let check = run_nft(nft, &["--check", "--file", "-"], Some(script.as_bytes())).await?;
    if !check.status.success() {
        return Err(format!("nft check failed: {}", tail(&check.stderr)));
    }
    let apply = run_nft(nft, &["--file", "-"], Some(script.as_bytes())).await?;
    if !apply.status.success() {
        return Err(format!("nft apply failed: {}", tail(&apply.stderr)));
    }
    Ok(())
}

async fn ensure_table() -> Result<(), String> {
    let nft = nft_path()?;
    let listed = run_nft(nft, &["list", "table", TABLE_FAMILY, TABLE_NAME], None).await?;
    if listed.status.success() {
        return Ok(());
    }
    let added = run_nft(nft, &["add", "table", TABLE_FAMILY, TABLE_NAME], None).await?;
    if !added.status.success() {
        return Err(format!(
            "create managed nftables table: {}",
            tail(&added.stderr)
        ));
    }
    Ok(())
}

fn render_ruleset(state: &FirewallState) -> Result<String, String> {
    let mut script = format!(
        "delete table {TABLE_FAMILY} {TABLE_NAME}\n\
         add table {TABLE_FAMILY} {TABLE_NAME}\n\
         add chain {TABLE_FAMILY} {TABLE_NAME} input {{ type filter hook input priority -5; policy accept; }}\n\
         add chain {TABLE_FAMILY} {TABLE_NAME} output {{ type filter hook output priority -5; policy accept; }}\n"
    );
    for rule in &state.rules {
        validate_rule(rule)?;
        let mut expressions = Vec::new();
        if let Some(interface) = &rule.interface {
            expressions.push(format!(
                "{} \"{}\"",
                if rule.direction == "input" {
                    "iifname"
                } else {
                    "oifname"
                },
                interface
            ));
        }
        if let Some(remote) = &rule.remote {
            let family = if remote.contains(':') { "ip6" } else { "ip" };
            expressions.push(format!(
                "{family} {} {remote}",
                if rule.direction == "input" {
                    "saddr"
                } else {
                    "daddr"
                }
            ));
        }
        expressions.push(format!("{} dport {}", rule.protocol, rule.port));
        expressions.push(if rule.action == "allow" {
            "accept".to_string()
        } else {
            "drop".to_string()
        });
        expressions.push(format!("comment \"claw:{}\"", rule.id));
        script.push_str(&format!(
            "add rule {TABLE_FAMILY} {TABLE_NAME} {} {}\n",
            rule.direction,
            expressions.join(" ")
        ));
    }
    Ok(script)
}

fn validate_rule(rule: &FirewallRule) -> Result<(), String> {
    if !matches!(rule.action.as_str(), "allow" | "deny")
        || !matches!(rule.direction.as_str(), "input" | "output")
        || !matches!(rule.protocol.as_str(), "tcp" | "udp")
        || rule.port == 0
    {
        return Err("persisted firewall rule is invalid".to_string());
    }
    validate_id(&rule.id, "rule id")?;
    if let Some(remote) = &rule.remote {
        normalize_cidr(remote)?;
    }
    if let Some(interface) = &rule.interface {
        validate_interface(interface)?;
    }
    Ok(())
}

async fn list_live_table() -> Result<Value, String> {
    let nft = nft_path()?;
    let output = run_nft(
        nft,
        &["--json", "list", "table", TABLE_FAMILY, TABLE_NAME],
        None,
    )
    .await?;
    if !output.status.success() {
        return Err(format!(
            "managed nftables table is unavailable: {}",
            tail(&output.stderr)
        ));
    }
    serde_json::from_str(&output.stdout).map_err(|error| format!("parse nftables JSON: {error}"))
}

fn extract_live_rule_ids(value: &Value) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    collect_comments(value, &mut ids);
    ids
}

fn collect_comments(value: &Value, ids: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            if let Some(comment) = map.get("comment").and_then(Value::as_str) {
                if let Some(id) = comment.strip_prefix("claw:") {
                    if validate_id(id, "rule id").is_ok() {
                        ids.insert(id.to_string());
                    }
                }
            }
            for value in map.values() {
                collect_comments(value, ids);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_comments(value, ids);
            }
        }
        _ => {}
    }
}

fn normalize_cidr(value: &str) -> Result<String, String> {
    let (address, prefix) = value
        .split_once('/')
        .ok_or_else(|| "remote CIDR must use address/prefix form".to_string())?;
    let address = address
        .parse::<std::net::IpAddr>()
        .map_err(|_| format!("invalid remote CIDR address: {address:?}"))?;
    let prefix = prefix
        .parse::<u8>()
        .map_err(|_| "invalid remote CIDR prefix".to_string())?;
    let maximum = if address.is_ipv4() { 32 } else { 128 };
    if prefix > maximum {
        return Err(format!("remote CIDR prefix must be 0..{maximum}"));
    }
    Ok(format!("{address}/{prefix}"))
}

fn validate_interface(value: &str) -> Result<String, String> {
    if value.is_empty()
        || value.len() > 15
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
    {
        return Err(format!("invalid interface name: {value:?}"));
    }
    Ok(value.to_string())
}

fn validate_id(value: &str, kind: &str) -> Result<(), String> {
    if value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(format!("invalid {kind}"))
    }
}

fn validate_action(
    action: &str,
    rule_action: Option<&str>,
    direction: Option<&str>,
    protocol: Option<&str>,
    port: Option<u64>,
    remote: Option<&str>,
    interface: Option<&str>,
    rule_id: Option<&str>,
    token: Option<&str>,
    confirm: bool,
) -> Result<(), String> {
    if let Some(remote) = remote {
        normalize_cidr(remote)?;
    }
    if let Some(interface) = interface {
        validate_interface(interface)?;
    }
    match action {
        "status"
            if rule_action.is_none()
                && direction.is_none()
                && protocol.is_none()
                && port.is_none()
                && remote.is_none()
                && interface.is_none()
                && rule_id.is_none()
                && token.is_none()
                && !confirm =>
        {
            Ok(())
        }
        "add"
            if matches!(rule_action, Some("allow" | "deny"))
                && matches!(direction, Some("input" | "output"))
                && matches!(protocol, Some("tcp" | "udp"))
                && port.is_some_and(|port| (1..=65535).contains(&port))
                && rule_id.is_none()
                && token.is_none()
                && !confirm =>
        {
            Ok(())
        }
        "delete"
            if rule_action.is_none()
                && direction.is_none()
                && protocol.is_none()
                && port.is_none()
                && remote.is_none()
                && interface.is_none()
                && rule_id.is_some_and(|id| validate_id(id, "rule id").is_ok())
                && token.is_none()
                && !confirm =>
        {
            Ok(())
        }
        "clear"
            if rule_action.is_none()
                && direction.is_none()
                && protocol.is_none()
                && port.is_none()
                && remote.is_none()
                && interface.is_none()
                && rule_id.is_none()
                && token.is_none()
                && confirm =>
        {
            Ok(())
        }
        "restore"
            if rule_action.is_none()
                && direction.is_none()
                && protocol.is_none()
                && port.is_none()
                && remote.is_none()
                && interface.is_none()
                && rule_id.is_none()
                && token.is_some_and(|token| validate_id(token, "backup token").is_ok())
                && confirm =>
        {
            Ok(())
        }
        _ => Err(format!("invalid arguments for firewall action {action:?}")),
    }
}

fn load_state() -> Result<FirewallState, String> {
    let path = state_path();
    let data = match fs::read(&path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(FirewallState::default())
        }
        Err(error) => return Err(format!("read firewall state {}: {error}", path.display())),
    };
    let state: FirewallState =
        serde_json::from_slice(&data).map_err(|error| format!("parse firewall state: {error}"))?;
    if state.schema != 1 {
        return Err(format!(
            "unsupported firewall state schema: {}",
            state.schema
        ));
    }
    for rule in &state.rules {
        validate_rule(rule)?;
    }
    Ok(state)
}

fn save_state(state: &FirewallState) -> Result<(), String> {
    let data = serde_json::to_vec_pretty(state)
        .map_err(|error| format!("serialize firewall state: {error}"))?;
    crate::agent::util::atomic_write_with_fsync(&state_path(), &data)
        .map_err(|error| format!("write firewall state: {error}"))
}

fn create_backup(
    owner_uid: u32,
    previous: FirewallState,
    applied_revision: &str,
) -> Result<FirewallBackup, String> {
    let backup = FirewallBackup {
        token: uuid::Uuid::new_v4().simple().to_string(),
        owner_uid,
        created_at: chrono::Utc::now().to_rfc3339(),
        applied_revision: applied_revision.to_string(),
        previous,
        status: "prepared".to_string(),
    };
    save_backup(&backup)?;
    Ok(backup)
}

fn save_backup(backup: &FirewallBackup) -> Result<(), String> {
    let data = serde_json::to_vec_pretty(backup)
        .map_err(|error| format!("serialize firewall backup: {error}"))?;
    crate::agent::util::atomic_write_with_fsync(&backup_path(&backup.token), &data)
        .map_err(|error| format!("write firewall backup: {error}"))
}

fn load_backup(token: &str) -> Result<FirewallBackup, String> {
    let data =
        fs::read(backup_path(token)).map_err(|error| format!("read firewall backup: {error}"))?;
    serde_json::from_slice(&data).map_err(|error| format!("parse firewall backup: {error}"))
}

fn state_path() -> PathBuf {
    crate::paths::data_dir()
        .join("clawd")
        .join("firewall-state.json")
}

fn backup_path(token: &str) -> PathBuf {
    crate::paths::data_dir()
        .join("clawd")
        .join("firewall-backups")
        .join(format!("{token}.json"))
}

async fn run_nft(
    program: &'static str,
    args: &[&str],
    stdin_data: Option<&[u8]>,
) -> Result<CommandOutput, String> {
    let args = args.iter().map(|value| value.to_string()).collect();
    let stdin_data = stdin_data.map(Vec::from);
    tokio::task::spawn_blocking(move || run_nft_sync(program, args, stdin_data))
        .await
        .map_err(|error| format!("nft worker failed: {error}"))?
}

fn run_nft_sync(
    program: &str,
    args: Vec<String>,
    stdin_data: Option<Vec<u8>>,
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
        .map_err(|error| format!("failed to launch nft: {error}"))?;
    if let Some(data) = stdin_data {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "nft stdin is unavailable".to_string())?;
        stdin
            .write_all(&data)
            .map_err(|error| format!("write nft ruleset: {error}"))?;
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "nft stdout is unavailable".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "nft stderr is unavailable".to_string())?;
    let stdout_reader = std::thread::spawn(move || read_bounded(stdout));
    let stderr_reader = std::thread::spawn(move || read_bounded(stderr));
    let deadline = Instant::now() + TOOL_TIMEOUT;
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
                    .map_err(|error| format!("wait for timed-out nft: {error}"))?;
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("wait for nft: {error}"));
            }
        }
    };
    let (stdout, stdout_truncated) = stdout_reader
        .join()
        .map_err(|_| "nft stdout reader panicked".to_string())??;
    let (stderr, stderr_truncated) = stderr_reader
        .join()
        .map_err(|_| "nft stderr reader panicked".to_string())??;
    if timed_out {
        return Err(format!("nft timed out after {}s", TOOL_TIMEOUT.as_secs()));
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
            .map_err(|error| format!("read nft output: {error}"))?;
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

fn nft_path() -> Result<&'static str, String> {
    ["/usr/sbin/nft", "/usr/bin/nft"]
        .into_iter()
        .find(|path| Path::new(path).is_file())
        .ok_or_else(|| "nft is not installed".to_string())
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
    fn cidr_validation_is_bounded() {
        assert_eq!(normalize_cidr("192.0.2.1/24").unwrap(), "192.0.2.1/24");
        assert!(normalize_cidr("192.0.2.1/33").is_err());
        assert!(normalize_cidr("example.com/24").is_err());
    }

    #[test]
    fn rendered_rules_use_managed_comments() {
        let state = FirewallState {
            schema: 1,
            revision: "r".to_string(),
            rules: vec![FirewallRule {
                id: "0123456789abcdef0123456789abcdef".to_string(),
                action: "deny".to_string(),
                direction: "input".to_string(),
                protocol: "tcp".to_string(),
                port: 22,
                remote: Some("192.0.2.0/24".to_string()),
                interface: Some("eth0".to_string()),
            }],
        };
        let script = render_ruleset(&state).unwrap();
        assert!(script.contains("tcp dport 22 drop"));
        assert!(script.contains("comment \"claw:0123456789abcdef0123456789abcdef\""));
    }
}
