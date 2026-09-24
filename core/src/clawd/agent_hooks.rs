//! @agent-file
//! Responsibility: expose owner-scoped built-in Agent hook settings through the broker.
//! Key dependencies: hooks_config persistence and authenticated owner filesystem identity.
//! Constraints: only closed built-in kinds are accepted and changes apply to future tasks.

use std::path::Path;
use std::sync::mpsc;

use serde_json::{json, Value};

use crate::agent::runtime::hooks_config::{self, HookKind, HooksConfig};
use crate::agentd::spawn::ROOT_OWNER_REFUSAL;

use super::client_identity::{ClientIdentity, FsIdentityGuard};

pub fn get(_params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let owner_uid = owner_uid(client)?;
    run_as_owner(owner_uid, move |path| get_at(&path))
}

pub fn set(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let request: super::wire::requests::AgentHooksSet = serde_json::from_value(params)
        .map_err(|error| format!("invalid Agent hook setting: {error}"))?;
    let kind = HookKind::parse(request.kind.as_str()).ok_or_else(|| {
        format!(
            "unknown Agent hook kind {}; expected logging, audit, or checkpoint",
            request.kind.as_str()
        )
    })?;
    let owner_uid = owner_uid(client)?;
    run_as_owner(owner_uid, move |path| set_at(&path, kind, request.enabled))
}

fn owner_uid(client: &ClientIdentity) -> Result<u32, String> {
    let owner_uid = client.require_uid()?;
    if owner_uid == 0 {
        return Err(ROOT_OWNER_REFUSAL.to_string());
    }
    Ok(owner_uid)
}

fn run_as_owner<T, F>(owner_uid: u32, operation: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(std::path::PathBuf) -> Result<T, String> + Send + 'static,
{
    let path = crate::paths::clawd_user_agent_state_dir(owner_uid).join("hooks.json");
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("clawd-owner-agent-hooks".to_string())
        .spawn(move || {
            let result = FsIdentityGuard::enter(owner_uid).and_then(|_identity| operation(path));
            let _ = tx.send(result);
        })
        .map_err(|error| format!("start owner Agent hooks operation: {error}"))?;
    rx.recv()
        .map_err(|_| "owner Agent hooks operation stopped without a result".to_string())?
}

fn project(config: &HooksConfig, changed: Option<(HookKind, bool)>) -> Value {
    let hooks = HookKind::ALL
        .into_iter()
        .map(|kind| {
            json!({
                "kind": kind.canonical(),
                "enabled": config.is_enabled(kind),
            })
        })
        .collect::<Vec<_>>();
    let mut value = json!({
        "applies_to": "future_tasks",
        "hooks": hooks,
    });
    if let Some((kind, changed)) = changed {
        value["updated_kind"] = json!(kind.canonical());
        value["changed"] = json!(changed);
    }
    value
}

fn get_at(path: &Path) -> Result<Value, String> {
    let config = hooks_config::load(path).map_err(|error| error.to_string())?;
    Ok(project(&config, None))
}

fn set_at(path: &Path, kind: HookKind, enabled: bool) -> Result<Value, String> {
    let mut config = hooks_config::load(path).map_err(|error| error.to_string())?;
    let changed = if enabled {
        config.enable(kind)
    } else {
        config.disable(kind)
    };
    if changed {
        hooks_config::save(path, &config).map_err(|error| error.to_string())?;
    }
    Ok(project(&config, Some((kind, changed))))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/agent_hooks.rs"
    ));
}
