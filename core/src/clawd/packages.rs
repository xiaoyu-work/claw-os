use serde_json::{json, Value};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use crate::caps::{Cap, CapSet, Scope, Verb};

use super::client_identity::ClientIdentity;

const INSTALL_TIMEOUT: Duration = Duration::from_secs(15 * 60);

static APT_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

pub async fn install(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client);
        return Err("system package installation requires Linux".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err("system package installation requires root clawd".to_string());
        }

        let uid = client.require_uid()?;
        let home = client.require_home_dir()?;
        let peer_pid = client
            .pid
            .ok_or_else(|| "clawd peer pid is unavailable".to_string())?;
        let session_id = required_string(&params, "session")?;
        let package = required_string(&params, "package")?;
        validate_package_name(&package)?;

        crate::paths::with_user_override(uid, home, async {
            authorize_package_session(&session_id, peer_pid, &package)
        })
        .await?;

        let _guard = APT_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let mut command = tokio::process::Command::new(apt_get_path());
        command
            .args(["install", "-y", "--no-install-recommends", package.as_str()])
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .env("HOME", "/root")
            .env("DEBIAN_FRONTEND", "noninteractive")
            .env("APT_LISTCHANGES_FRONTEND", "none")
            .env("LC_ALL", "C.UTF-8")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = tokio::time::timeout(INSTALL_TIMEOUT, command.output())
            .await
            .map_err(|_| {
                format!(
                    "apt-get install timed out after {}s",
                    INSTALL_TIMEOUT.as_secs()
                )
            })?
            .map_err(|error| format!("failed to launch apt-get: {error}"))?;
        let installed = output.status.success();

        Ok(json!({
            "package": package,
            "installed": installed,
            "exit_code": output.status.code(),
            "stdout_tail": output_tail(&output.stdout),
            "stderr_tail": output_tail(&output.stderr),
        }))
    }
}

fn authorize_package_session(session_id: &str, peer_pid: u32, package: &str) -> Result<(), String> {
    let session = crate::proc::session_info_by_id(session_id)
        .ok_or_else(|| format!("App session not found: {session_id}"))?;
    if session.app_id.as_deref() != Some("pkg") {
        return Err("system package installation is restricted to the pkg App".to_string());
    }
    if session.pending_bind || session.pid == 0 {
        return Err("pkg App session is not bound to a process".to_string());
    }
    let expected_start = session
        .start_time_ticks
        .ok_or_else(|| "pkg App session has no process identity".to_string())?;
    if crate::proc::read_start_time_ticks_pub(session.pid) != Some(expected_start) {
        return Err("pkg App session process identity is stale".to_string());
    }
    if !crate::proc::process_descends_from(peer_pid, session.pid) {
        return Err("package request did not originate from the pkg App process".to_string());
    }

    let mut caps = session.caps.unwrap_or_else(CapSet::new);
    if let Some(transient) = session.transient_caps {
        caps.extend(transient.iter().cloned());
    }
    let requested = Cap::new(Verb::SYS_PACKAGE, Scope::name(package));
    if !caps.covers(&requested) {
        return Err(format!(
            "pkg App session lacks sys.package permission for `{package}`"
        ));
    }
    Ok(())
}

pub(crate) fn validate_package_name(package: &str) -> Result<(), String> {
    if package.is_empty() || package.len() > 255 || package.starts_with('-') {
        return Err(format!("invalid Debian package name: {package:?}"));
    }
    let (name, architecture) = package
        .split_once(':')
        .map(|(name, arch)| (name, Some(arch)))
        .unwrap_or((package, None));
    if !valid_name_component(name, true)
        || architecture.is_some_and(|arch| !valid_name_component(arch, false))
    {
        return Err(format!("invalid Debian package name: {package:?}"));
    }
    Ok(())
}

fn valid_name_component(value: &str, allow_plus_dot: bool) -> bool {
    !value.is_empty()
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (index > 0 && (byte == b'-' || (allow_plus_dot && matches!(byte, b'+' | b'.'))))
        })
}

fn output_tail(bytes: &[u8]) -> String {
    const MAX: usize = 8 * 1024;
    let start = bytes.len().saturating_sub(MAX);
    String::from_utf8_lossy(&bytes[start..]).trim().to_string()
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

fn apt_get_path() -> &'static str {
    "/usr/bin/apt-get"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proc::SessionInfo;

    #[test]
    fn accepts_debian_package_names_and_arch_qualifiers() {
        for name in ["bash", "libssl3", "python3-venv", "g++", "curl:amd64"] {
            validate_package_name(name).unwrap();
        }
    }

    #[test]
    fn rejects_options_paths_versions_and_empty_architectures() {
        for name in [
            "",
            "-oDpkg::Pre-Invoke::=id",
            "../bash",
            "bash=1.0",
            "Bash",
            "curl:",
            "curl:amd64!",
        ] {
            assert!(validate_package_name(name).is_err(), "{name:?} should fail");
        }
    }

    #[test]
    fn package_authorization_is_bound_to_pkg_session_and_scope() {
        let _lock = crate::test_env::lock_env();
        let temp = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("COS_PROC_DATA_DIR");
        std::env::set_var("COS_PROC_DATA_DIR", temp.path());

        let mut caps = CapSet::new();
        caps.insert(Cap::new(Verb::SYS_PACKAGE, Scope::name("curl")));
        let session_id = format!("app-package-test-{}", std::process::id());
        crate::proc::register_session(SessionInfo {
            session_id: session_id.clone(),
            pid: std::process::id(),
            command: vec!["pkg test".to_string()],
            started_at: chrono::Utc::now().to_rfc3339(),
            stdout_path: String::new(),
            stderr_path: String::new(),
            group: Some("app".to_string()),
            parent: None,
            workdir: None,
            exit_code: None,
            ended_at: None,
            tier: None,
            scope: None,
            priority: None,
            caps: Some(caps),
            transient_caps: None,
            role: None,
            app_id: Some("pkg".to_string()),
            pending_bind: false,
            start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
        })
        .unwrap();

        assert!(authorize_package_session(&session_id, std::process::id(), "curl").is_ok());
        assert!(authorize_package_session(&session_id, std::process::id(), "bash").is_err());

        crate::proc::deregister_session(&session_id);
        match previous {
            Some(value) => std::env::set_var("COS_PROC_DATA_DIR", value),
            None => std::env::remove_var("COS_PROC_DATA_DIR"),
        }
    }
}
