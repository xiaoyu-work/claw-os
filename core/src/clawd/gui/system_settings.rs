//! Closed machine settings adapter for the OS-started Settings instance.

use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;

use crate::clawd::protocol::BrokerError;
use crate::clawd::wire::requests::{SystemSettings, SystemSettingsAction};

static CHANGE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(super) async fn request(params: Value, owner: u32) -> Result<Value, BrokerError> {
    let request: SystemSettings = serde_json::from_value(params)
        .map_err(|_| BrokerError::execution("Invalid system settings action"))?;
    let action = request.action;
    let _change = if matches!(action, SystemSettingsAction::Status) {
        None
    } else {
        Some(CHANGE.try_lock().map_err(|_| {
            BrokerError::unavailable("Another system settings change is in progress")
        })?)
    };
    let name = serde_json::to_value(action).expect("system settings action is serializable");
    let name = name.as_str().expect("system settings action is a string");
    let output = tokio::time::timeout(
        Duration::from_secs(90),
        tokio::process::Command::new("/usr/libexec/claw-system-settings")
            .arg(name)
            .arg(owner.to_string())
            .env_clear()
            .env("LANG", "C.UTF-8")
            .env("PATH", "/usr/sbin:/usr/bin")
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| {
        BrokerError::unavailable("System settings result is unconfirmed; refresh the page")
    })?
    .map_err(|_| BrokerError::unavailable("Install the matching Claw OS Desktop package"))?;
    if output.status.code() == Some(3) {
        return Err(BrokerError::authorization(
            "Administrator authentication was not granted",
        ));
    }
    if !output.status.success() {
        return Err(BrokerError::execution(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    let state = serde_json::from_slice(&output.stdout)
        .map_err(|_| BrokerError::unavailable("Could not read the confirmed system settings"))?;
    if !matches!(action, SystemSettingsAction::Status) {
        crate::provenance::audit(
            "desktop.system_settings",
            serde_json::json!({"owner_uid": owner, "action": name}),
        );
    }
    Ok(state)
}
