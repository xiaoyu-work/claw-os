use serde_json::{json, Map, Value};
use std::fs;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use crate::caps::{Cap, Scope, Verb};

use super::authority::{Authorized, Decision};

const TOOL_TIMEOUT: Duration = Duration::from_secs(30);
const STREAM_CAP_BYTES: usize = 2 * 1024 * 1024;
const JOURNAL_LIMIT: usize = 1000;

pub async fn inspect(params: Value, authority: &Decision) -> Result<Value, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, authority);
        return Err("Security Center requires Linux".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err("Security Center requires root clawd".to_string());
        }
        let action = required_string(&params, "action")?;
        validate_action(&action)?;
        let _authorized = authorize_session(authority)?;

        match action.as_str() {
            "summary" => summary().await,
            "auth" => auth_report().await,
            "ssh" => ssh_report().await,
            "sudo" => sudo_report().await,
            "mac" => mac_report(),
            "ports" => port_report().await,
            "events" => security_events().await,
            _ => unreachable!("validated security action"),
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
fn authorize_session(authority: &Decision) -> Result<Authorized, String> {
    authority.require_app("security-center")?;
    authority.require(Cap::new(Verb::SYS_SECURITY, Scope::name("audit")))
}

async fn summary() -> Result<Value, String> {
    let (auth, ssh, sudo, ports, events) = tokio::join!(
        auth_report(),
        ssh_report(),
        sudo_report(),
        port_report(),
        security_events(),
    );
    let mac = mac_report();
    let sections = [
        ("auth", result_value(auth)),
        ("ssh", result_value(ssh)),
        ("sudo", result_value(sudo)),
        ("mac", result_value(mac)),
        ("ports", result_value(ports)),
        ("events", result_value(events)),
    ];
    let mut findings = sections
        .iter()
        .flat_map(|(section, value)| {
            value["findings"]
                .as_array()
                .into_iter()
                .flatten()
                .cloned()
                .map(|mut finding| {
                    finding["section"] = Value::String((*section).to_string());
                    finding
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    for (section, value) in &sections {
        if value["available"].as_bool() == Some(false) || value.get("error").is_some() {
            findings.push(finding(
                "security-provider-unavailable",
                "warning",
                format!("{section} security evidence could not be collected."),
                "Restore the missing provider or inspect the section error before concluding the system is healthy.",
            ));
        }
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
        "schema": 1,
        "status": status,
        "auth": sections[0].1.clone(),
        "ssh": sections[1].1.clone(),
        "sudo": sections[2].1.clone(),
        "mac": sections[3].1.clone(),
        "ports": sections[4].1.clone(),
        "events": sections[5].1.clone(),
        "findings": findings,
    }))
}

fn result_value(result: Result<Value, String>) -> Value {
    result.unwrap_or_else(|error| json!({"available": false, "error": error, "findings": []}))
}

async fn auth_report() -> Result<Value, String> {
    let entries = journal_entries(false).await?;
    let mut events = Vec::new();
    let mut failed_logins = 0_u64;
    let mut successful_logins = 0_u64;
    let mut sudo_failures = 0_u64;
    let mut root_sessions = 0_u64;
    for entry in entries {
        let message = entry["message"].as_str().unwrap_or_default();
        let lower = message.to_ascii_lowercase();
        let identifier = entry["identifier"]
            .as_str()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let auth_source = ["sshd", "sudo", "su", "login", "polkit", "gdm", "cosmic"]
            .iter()
            .any(|source| identifier.contains(source))
            || lower.contains("pam_unix");
        if !auth_source {
            continue;
        }
        let kind = if lower.contains("sudo")
            && ["incorrect password", "authentication failure"]
                .iter()
                .any(|needle| lower.contains(needle))
        {
            sudo_failures += 1;
            "sudo-failure"
        } else if [
            "failed password",
            "authentication failure",
            "invalid user",
            "failed publickey",
            "maximum authentication attempts",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
        {
            failed_logins += 1;
            "authentication-failure"
        } else if lower.contains("accepted password") || lower.contains("accepted publickey") {
            successful_logins += 1;
            "login-success"
        } else if lower.contains("session opened for user root") {
            root_sessions += 1;
            "root-session"
        } else {
            continue;
        };
        let mut event = entry;
        event["kind"] = Value::String(kind.to_string());
        events.push(event);
    }
    let mut findings = Vec::new();
    if failed_logins >= 10 {
        findings.push(finding(
            "repeated-auth-failures",
            "critical",
            format!("{failed_logins} authentication failures occurred in the last 24 hours."),
            "Identify source addresses/users and verify whether rate limiting or account lockout is active.",
        ));
    } else if failed_logins > 0 {
        findings.push(finding(
            "auth-failures",
            "warning",
            format!("{failed_logins} authentication failure(s) occurred in the last 24 hours."),
            "Correlate failures by account, source, and service.",
        ));
    }
    if sudo_failures > 0 {
        findings.push(finding(
            "sudo-auth-failures",
            "warning",
            format!("{sudo_failures} sudo authentication failure(s) were found."),
            "Confirm the attempts were expected and review the initiating sessions.",
        ));
    }
    if root_sessions > 0 {
        findings.push(finding(
            "root-sessions",
            "warning",
            format!("{root_sessions} root session-open event(s) were found."),
            "Confirm direct root sessions are intentional and attributable.",
        ));
    }
    Ok(json!({
        "available": true,
        "window": "24h",
        "failed_logins": failed_logins,
        "successful_logins": successful_logins,
        "sudo_failures": sudo_failures,
        "root_sessions": root_sessions,
        "events": events,
        "findings": findings,
    }))
}

async fn ssh_report() -> Result<Value, String> {
    let service = ssh_service_state().await;
    let config = if let Some(sshd) = tool_path(&["/usr/sbin/sshd", "/usr/bin/sshd"]) {
        match run_command(sshd, &["-T"], TOOL_TIMEOUT).await {
            Ok(output) if output.status.success() => parse_space_fields(&output.stdout),
            Ok(output) => {
                let mut map = Map::new();
                map.insert(
                    "error".to_string(),
                    Value::String(format!(
                        "sshd -T exited {}: {}",
                        output.status.code().unwrap_or(-1),
                        tail(&output.stderr)
                    )),
                );
                map
            }
            Err(error) => {
                let mut map = Map::new();
                map.insert("error".to_string(), Value::String(error));
                map
            }
        }
    } else {
        let mut map = Map::new();
        map.insert(
            "error".to_string(),
            Value::String("sshd is not installed".to_string()),
        );
        map
    };
    let mut findings = Vec::new();
    if config.get("permitemptypasswords").and_then(Value::as_str) == Some("yes") {
        findings.push(finding(
            "ssh-empty-passwords",
            "critical",
            "sshd permits empty passwords.".to_string(),
            "Set PermitEmptyPasswords no and validate the effective configuration.",
        ));
    }
    if config.get("permitrootlogin").and_then(Value::as_str) == Some("yes") {
        findings.push(finding(
            "ssh-root-login",
            "critical",
            "sshd permits direct root login.".to_string(),
            "Disable direct root login and use accountable sudo escalation.",
        ));
    }
    if config.get("passwordauthentication").and_then(Value::as_str) == Some("yes") {
        findings.push(finding(
            "ssh-password-auth",
            "warning",
            "sshd password authentication is enabled.".to_string(),
            "Prefer public-key authentication where operationally possible.",
        ));
    }
    Ok(json!({
        "available": true,
        "service": service,
        "effective_config": config,
        "findings": findings,
    }))
}

fn parse_space_fields(output: &str) -> Map<String, Value> {
    output
        .lines()
        .filter_map(|line| line.split_once(' '))
        .map(|(key, value)| (normalize_key(key), Value::String(value.trim().to_string())))
        .collect()
}

async fn ssh_service_state() -> Value {
    let Some(systemctl) = tool_path(&["/usr/bin/systemctl", "/bin/systemctl"]) else {
        return json!({"available": false, "error": "systemctl is not installed"});
    };
    for unit in ["ssh.service", "sshd.service"] {
        if let Ok(output) = run_command(systemctl, &["is-active", unit], TOOL_TIMEOUT).await {
            let state = output.stdout.trim();
            if output.status.success() || !state.is_empty() {
                return json!({
                    "unit": unit,
                    "active_state": state,
                    "exit_code": output.status.code(),
                });
            }
        }
    }
    json!({"available": true, "active_state": "not-found"})
}

async fn sudo_report() -> Result<Value, String> {
    let mut files = Vec::new();
    if Path::new("/etc/sudoers").exists() {
        files.push(PathBuf::from("/etc/sudoers"));
    }
    if let Ok(entries) = fs::read_dir("/etc/sudoers.d") {
        files.extend(
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.is_file()),
        );
    }
    files.sort();
    files.truncate(256);
    let mut rules = Vec::new();
    let mut file_state = Vec::new();
    let mut findings = Vec::new();
    for path in files {
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("inspect {}: {error}", path.display()))?;
        let mode = metadata.mode() & 0o7777;
        if metadata.file_type().is_symlink() {
            findings.push(finding(
                "sudoers-symlink",
                "critical",
                format!("{} is a symbolic link.", path.display()),
                "Replace sudoers symlinks with root-owned regular files managed through visudo.",
            ));
            continue;
        }
        if metadata.uid() != 0 {
            findings.push(finding(
                "sudoers-owner",
                "critical",
                format!(
                    "{} belongs to uid {} instead of root.",
                    path.display(),
                    metadata.uid()
                ),
                "Restore root ownership before relying on this sudoers policy.",
            ));
        }
        if mode & 0o022 != 0 {
            findings.push(finding(
                "sudoers-writable",
                "critical",
                format!("{} is group/world writable ({mode:o}).", path.display()),
                "Restore root ownership and non-writable sudoers permissions.",
            ));
        }
        file_state.push(json!({
            "path": path,
            "uid": metadata.uid(),
            "gid": metadata.gid(),
            "mode": format!("{mode:04o}"),
        }));
        if let Ok(data) = fs::read_to_string(&path) {
            for (index, line) in data.lines().enumerate() {
                let trimmed = line.trim();
                if trimmed.is_empty()
                    || (trimmed.starts_with('#')
                        && !trimmed.starts_with("#include")
                        && !trimmed.starts_with("#includedir"))
                {
                    continue;
                }
                rules.push(json!({
                    "path": path,
                    "line": index + 1,
                    "rule": truncate_text(trimmed, 4096),
                }));
                let upper = trimmed.to_ascii_uppercase();
                let broad_nopasswd = upper
                    .split_once("NOPASSWD:")
                    .map(|(_, commands)| commands.trim())
                    .is_some_and(|commands| commands == "ALL" || commands.starts_with("ALL,"));
                if broad_nopasswd
                    && !findings
                        .iter()
                        .any(|finding| finding["code"] == "sudoers-broad-nopasswd")
                {
                    findings.push(finding(
                        "sudoers-broad-nopasswd",
                        "warning",
                        format!("A broad NOPASSWD rule exists in {}.", path.display()),
                        "Confirm passwordless elevation is narrowly scoped to required commands and identities.",
                    ));
                }
            }
        }
    }
    let validation = if let Some(visudo) = tool_path(&["/usr/sbin/visudo", "/usr/bin/visudo"]) {
        match run_command(visudo, &["-c"], TOOL_TIMEOUT).await {
            Ok(output) => json!({
                "valid": output.status.success(),
                "exit_code": output.status.code(),
                "stdout": truncate_text(&output.stdout, 16 * 1024),
                "stderr": truncate_text(&output.stderr, 16 * 1024),
            }),
            Err(error) => json!({"valid": false, "error": error}),
        }
    } else {
        json!({"available": false, "error": "visudo is not installed"})
    };
    if validation["valid"].as_bool() == Some(false) {
        findings.push(finding(
            "sudoers-invalid",
            "critical",
            "visudo reported an invalid sudoers configuration.".to_string(),
            "Correct sudoers syntax using visudo before relying on privilege escalation.",
        ));
    }
    Ok(json!({
        "available": true,
        "files": file_state,
        "rules": rules,
        "validation": validation,
        "findings": findings,
    }))
}

fn mac_report() -> Result<Value, String> {
    let apparmor_enabled = read_trim("/sys/module/apparmor/parameters/enabled")
        .is_some_and(|value| value.eq_ignore_ascii_case("Y"));
    let apparmor_profiles = fs::read_to_string("/sys/kernel/security/apparmor/profiles")
        .ok()
        .map(|data| {
            let mut enforce = 0_u64;
            let mut complain = 0_u64;
            let mut kill = 0_u64;
            for line in data.lines() {
                if line.ends_with("(enforce)") {
                    enforce += 1;
                } else if line.ends_with("(complain)") {
                    complain += 1;
                } else if line.ends_with("(kill)") {
                    kill += 1;
                }
            }
            json!({
                "count": data.lines().count(),
                "enforce": enforce,
                "complain": complain,
                "kill": kill,
            })
        });
    let selinux_enforce = read_trim("/sys/fs/selinux/enforce").and_then(|value| value.parse().ok());
    let mut findings = Vec::new();
    if !apparmor_enabled && selinux_enforce != Some(1_u8) {
        findings.push(finding(
            "mac-disabled",
            "warning",
            "Neither AppArmor nor enforcing SELinux was detected.".to_string(),
            "Enable and maintain a mandatory access-control policy appropriate for this system.",
        ));
    }
    if apparmor_profiles
        .as_ref()
        .and_then(|profiles| profiles["complain"].as_u64())
        .unwrap_or(0)
        > 0
    {
        findings.push(finding(
            "apparmor-complain",
            "warning",
            "One or more AppArmor profiles are in complain mode.".to_string(),
            "Review complain-mode profiles and move validated policies to enforce mode.",
        ));
    }
    Ok(json!({
        "available": true,
        "apparmor": {
            "enabled": apparmor_enabled,
            "profiles": apparmor_profiles,
        },
        "selinux": {
            "mounted": Path::new("/sys/fs/selinux").exists(),
            "enforce": selinux_enforce,
        },
        "secure_boot": secure_boot_state(),
        "lockdown": read_trim("/sys/kernel/security/lockdown"),
        "findings": findings,
    }))
}

fn secure_boot_state() -> Option<bool> {
    let entries = fs::read_dir("/sys/firmware/efi/efivars").ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("SecureBoot-") {
            continue;
        }
        let data = fs::read(entry.path()).ok()?;
        return data.get(4).map(|value| *value == 1);
    }
    None
}

async fn port_report() -> Result<Value, String> {
    let ss =
        tool_path(&["/usr/bin/ss", "/bin/ss"]).ok_or_else(|| "ss is not installed".to_string())?;
    let output = run_command(ss, &["-H", "-lntup"], TOOL_TIMEOUT).await?;
    if !output.status.success() {
        return Err(format!(
            "ss exited {}: {}",
            output.status.code().unwrap_or(-1),
            tail(&output.stderr)
        ));
    }
    let listeners = output
        .stdout
        .lines()
        .filter_map(parse_listener)
        .collect::<Vec<_>>();
    let count = listeners.len();
    let mut findings = Vec::new();
    for listener in &listeners {
        let port = listener["port"].as_u64().unwrap_or(0);
        let wildcard = listener["wildcard"].as_bool() == Some(true);
        if wildcard && matches!(port, 22 | 2375 | 3306 | 5432 | 6379 | 9200 | 27017) {
            findings.push(finding(
                "sensitive-wildcard-listener",
                "warning",
                format!(
                    "{} port {port} listens on all interfaces.",
                    listener["protocol"].as_str().unwrap_or("network")
                ),
                "Confirm firewall exposure and bind sensitive services to the minimum required interfaces.",
            ));
        }
    }
    Ok(json!({
        "available": true,
        "listeners": listeners,
        "count": count,
        "findings": findings,
    }))
}

fn parse_listener(line: &str) -> Option<Value> {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 6 {
        return None;
    }
    let protocol = fields[0];
    let state = fields[1];
    let local = fields[4];
    let (address, port) = split_socket_address(local)?;
    let wildcard = matches!(address.as_str(), "*" | "0.0.0.0" | "::" | "[::]");
    Some(json!({
        "protocol": protocol,
        "state": state,
        "local": local,
        "address": address,
        "port": port,
        "wildcard": wildcard,
        "process": truncate_text(&fields[6..].join(" "), 4096),
    }))
}

fn split_socket_address(value: &str) -> Option<(String, u64)> {
    let (address, port) = value.rsplit_once(':')?;
    let port = port.trim_matches('*').parse().ok()?;
    Some((
        address
            .trim_matches(|character| matches!(character, '[' | ']'))
            .to_string(),
        port,
    ))
}

async fn security_events() -> Result<Value, String> {
    let entries = journal_entries(true).await?;
    let mut events = Vec::new();
    let mut findings = Vec::new();
    for entry in entries {
        let message = entry["message"].as_str().unwrap_or_default();
        let lower = message.to_ascii_lowercase();
        let kind = if lower.contains("apparmor=\"denied\"")
            || lower.contains("apparmor denied")
            || lower.contains("audit: type=1400")
        {
            "apparmor-denial"
        } else if lower.contains("avc:  denied")
            || lower.contains("selinux") && lower.contains("denied")
        {
            "selinux-denial"
        } else if lower.contains("lockdown:") {
            "kernel-lockdown"
        } else if lower.contains("module verification failed")
            || lower.contains("signature and/or required key missing")
        {
            "module-signature"
        } else if lower.contains("audit:") {
            "audit"
        } else if [
            "martian source",
            "possible syn flooding",
            "firewall",
            "nftables",
            "iptables",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
        {
            "network-security"
        } else {
            continue;
        };
        let mut event = entry;
        event["kind"] = Value::String(kind.to_string());
        events.push(event);
    }
    let denial_count = events
        .iter()
        .filter(|event| {
            matches!(
                event["kind"].as_str(),
                Some("apparmor-denial" | "selinux-denial")
            )
        })
        .count();
    if denial_count > 0 {
        findings.push(finding(
            "mac-denials",
            "warning",
            format!("{denial_count} mandatory-access-control denial event(s) were found."),
            "Identify the denied process/resource and distinguish attacks from policy regressions.",
        ));
    }
    let count = events.len();
    Ok(json!({
        "available": true,
        "window": "24h",
        "events": events,
        "count": count,
        "findings": findings,
    }))
}

async fn journal_entries(kernel: bool) -> Result<Vec<Value>, String> {
    let journalctl = tool_path(&["/usr/bin/journalctl", "/bin/journalctl"])
        .ok_or_else(|| "journalctl is not installed".to_string())?;
    let mut args = vec![
        "--no-pager",
        "--quiet",
        "--since=-24h",
        "--reverse",
        "-n",
        "1000",
        "--output=json",
        "--output-fields=__REALTIME_TIMESTAMP,_BOOT_ID,PRIORITY,SYSLOG_IDENTIFIER,_COMM,_EXE,_PID,_UID,_SYSTEMD_UNIT,MESSAGE",
    ];
    if kernel {
        args.push("--dmesg");
    }
    let output = run_command(journalctl, &args, TOOL_TIMEOUT).await?;
    if !output.status.success() {
        return Err(format!(
            "journalctl exited {}: {}",
            output.status.code().unwrap_or(-1),
            tail(&output.stderr)
        ));
    }
    Ok(output
        .stdout
        .lines()
        .take(JOURNAL_LIMIT)
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(normalize_journal_entry)
        .collect())
}

fn normalize_journal_entry(value: Value) -> Option<Value> {
    let message = value["MESSAGE"].as_str()?;
    Some(json!({
        "timestamp_us": value["__REALTIME_TIMESTAMP"],
        "boot_id": value["_BOOT_ID"],
        "priority": value["PRIORITY"],
        "identifier": value["SYSLOG_IDENTIFIER"].as_str().or_else(|| value["_COMM"].as_str()),
        "exe": value["_EXE"],
        "pid": value["_PID"],
        "uid": value["_UID"],
        "unit": value["_SYSTEMD_UNIT"],
        "message": truncate_text(message, 4096),
    }))
}

fn finding(code: &str, severity: &str, detail: String, recommendation: &str) -> Value {
    json!({
        "code": code,
        "severity": severity,
        "detail": detail,
        "recommendation": recommendation,
    })
}

fn normalize_key(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('-', "_")
}

fn read_trim(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn truncate_text(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_string();
    }
    let mut end = max;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

fn validate_action(action: &str) -> Result<(), String> {
    if matches!(
        action,
        "summary" | "auth" | "ssh" | "sudo" | "mac" | "ports" | "events"
    ) {
        Ok(())
    } else {
        Err(format!("unknown security action: {action}"))
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
            .map_err(|error| format!("read security command output: {error}"))?;
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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/security.rs"
    ));
}
