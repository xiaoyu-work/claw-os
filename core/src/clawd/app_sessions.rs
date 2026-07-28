use serde_json::{json, Value};

use crate::caps::{ArgKind, Cap, CapSet, Manifest, Scope, ScopeBinding, Verb};
use crate::proc::SessionInfo;

use super::client_identity::ClientIdentity;

pub async fn register(
    params: Value,
    client: &ClientIdentity,
) -> Result<Value, String> {
    require_trusted_launcher(client)?;
    let uid = client.require_uid()?;
    let home = client.require_home_dir()?;
    let mut info: SessionInfo = serde_json::from_value(
        params
            .get("session")
            .cloned()
            .ok_or_else(|| "session is required".to_string())?,
    )
    .map_err(|error| format!("invalid App session: {error}"))?;
    validate_app_session(&info)?;
    info.pid = 0;
    info.pending_bind = true;
    info.start_time_ticks = None;
    info.transient_caps = None;
    info.tier = info
        .tier
        .map(|tier| tier.max(crate::caps::Role::Worker.credential_tier()));
    validate_manifest_caps(
        info.app_id.as_deref().unwrap_or_default(),
        info.caps.as_ref(),
        false,
    )?;

    let session_id = info.session_id.clone();
    let proc_dir = crate::paths::with_user_override(uid, home, async move {
        crate::proc::register_session(info)?;
        Ok::<_, String>(crate::paths::proc_data_dir())
    })
    .await?;
    Ok(json!({
        "session_id": session_id,
        "proc_data_dir": proc_dir,
    }))
}

pub async fn register_mcp(
    params: Value,
    client: &ClientIdentity,
) -> Result<Value, String> {
    require_trusted_launcher(client)?;
    let uid = client.require_uid()?;
    let home = client.require_home_dir()?;
    let peer_pid = client
        .pid
        .ok_or_else(|| "clawd peer pid is unavailable".to_string())?;
    let parent: SessionInfo = serde_json::from_value(
        params
            .get("parent")
            .cloned()
            .ok_or_else(|| "parent session is required".to_string())?,
    )
    .map_err(|error| format!("invalid parent session: {error}"))?;
    if parent.app_id.is_some()
        || parent.pid != peer_pid
        || parent.caps.is_none()
    {
        return Err("invalid MCP parent session".to_string());
    }
    let command = required_string(&params, "command")?;
    let session_id = format!("mcp-{}", uuid::Uuid::new_v4().simple());
    let info = SessionInfo {
        session_id: session_id.clone(),
        pid: 0,
        command: vec![command],
        started_at: chrono::Utc::now().to_rfc3339(),
        stdout_path: String::new(),
        stderr_path: String::new(),
        group: Some("mcp".to_string()),
        parent: Some(parent.session_id),
        workdir: parent.workdir,
        exit_code: None,
        ended_at: None,
        tier: parent.tier,
        scope: parent.scope,
        priority: parent.priority,
        caps: parent.caps,
        transient_caps: None,
        role: parent.role,
        app_id: None,
        pending_bind: true,
        start_time_ticks: None,
    };
    let proc_dir = crate::paths::with_user_override(uid, home, async move {
        crate::proc::register_session(info)?;
        Ok::<_, String>(crate::paths::proc_data_dir())
    })
    .await?;
    Ok(json!({
        "session_id": session_id,
        "proc_data_dir": proc_dir,
    }))
}

pub async fn bind(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    require_trusted_launcher(client)?;
    let uid = client.require_uid()?;
    let home = client.require_home_dir()?;
    let parent_pid = client
        .pid
        .ok_or_else(|| "clawd peer pid is unavailable".to_string())?;
    let child_pid = required_u32(&params, "pid")?;
    if child_pid == parent_pid {
        return Err("App session must bind a child process".to_string());
    }
    if !is_descendant_of(child_pid, parent_pid) {
        return Err(format!(
            "process {child_pid} is not descended from launcher {parent_pid}"
        ));
    }
    if process_uid(child_pid) != Some(uid) {
        return Err(format!("App process {child_pid} is not owned by uid {uid}"));
    }
    let session_id = required_string(&params, "session_id")?;
    crate::paths::with_user_override(uid, home, async move {
        crate::proc::bind_session_process(&session_id, child_pid)
    })
    .await?;
    Ok(json!({"bound": true}))
}

pub async fn set_transient(
    params: Value,
    client: &ClientIdentity,
) -> Result<Value, String> {
    require_trusted_launcher(client)?;
    let uid = client.require_uid()?;
    let home = client.require_home_dir()?;
    let session_id = required_string(&params, "session_id")?;
    let caps = match params.get("caps") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            serde_json::from_value::<CapSet>(value.clone())
                .map_err(|error| format!("invalid transient caps: {error}"))?,
        ),
    };
    let app_id = crate::paths::with_user_override(
        uid,
        home.clone(),
        async {
            crate::proc::session_info_by_id(&session_id)
                .and_then(|session| session.app_id)
                .ok_or_else(|| "App session not found".to_string())
        },
    )
    .await?;
    validate_manifest_caps(&app_id, caps.as_ref(), true)?;
    crate::paths::with_user_override(uid, home, async move {
        crate::proc::set_app_session_transient_caps(&session_id, caps)
    })
    .await?;
    Ok(json!({"updated": true}))
}

pub async fn deregister(
    params: Value,
    client: &ClientIdentity,
) -> Result<Value, String> {
    require_trusted_launcher(client)?;
    let uid = client.require_uid()?;
    let home = client.require_home_dir()?;
    let session_id = required_string(&params, "session_id")?;
    crate::paths::with_user_override(uid, home, async move {
        crate::proc::deregister_session(&session_id);
    })
    .await;
    Ok(json!({"removed": true}))
}

fn validate_app_session(info: &SessionInfo) -> Result<(), String> {
    if !info.session_id.starts_with("app-")
        || !info
            .app_id
            .as_deref()
            .is_some_and(|value| !value.is_empty())
        || info.group.as_deref() != Some("app")
    {
        return Err("invalid App session identity".to_string());
    }
    Ok(())
}

fn validate_manifest_caps(
    app_id: &str,
    caps: Option<&CapSet>,
    session_only: bool,
) -> Result<(), String> {
        let Some(caps) = caps else {
            return Ok(());
        };
        let apps_dir = std::path::PathBuf::from(
            std::env::var("COS_APPS_DIR")
                .unwrap_or_else(|_| "/usr/lib/cos/apps".to_string()),
        );
        let manifest_path = crate::apps::find(&apps_dir, app_id)
            .map(|app| app.dir.join("app.json"))
            .ok_or_else(|| format!("App `{app_id}` is not installed"))?;
        let body = std::fs::read_to_string(&manifest_path)
            .map_err(|error| format!("read {}: {error}", manifest_path.display()))?;
        let manifest = Manifest::from_json(&body)
            .map_err(|error| format!("parse {}: {error}", manifest_path.display()))?;

        for cap in caps.iter() {
            if cap.verb == Verb::AGENT_INVOKE
                && cap.scope == Scope::name(app_id)
            {
                continue;
            }
            let allowed = if session_only {
                manifest
                    .session
                    .as_ref()
                    .into_iter()
                    .flat_map(|session| session.tools.iter())
                    .any(|tool| {
                        tool.needs
                            .iter()
                            .any(|need| need_allows_cap(need, &tool.args, cap))
                    })
            } else {
                manifest.operations.values().any(|operation| {
                    operation
                        .needs
                        .iter()
                        .any(|need| need_allows_cap(need, &operation.args, cap))
                })
            };
            if !allowed {
                return Err(format!(
                    "App `{app_id}` manifest does not declare capability {}:{}",
                    cap.verb.as_str(),
                    cap.scope
                ));
            }
        }
        Ok(())
    }

fn need_allows_cap(
    need: &crate::caps::Need,
    args: &[crate::caps::Arg],
    cap: &Cap,
) -> bool {
        if need.verb != cap.verb {
            return false;
        }
        match &need.scope {
            ScopeBinding::Fixed { scope } => scope == &cap.scope,
            ScopeBinding::Wild => true,
            ScopeBinding::FromArg { arg } => args
                .iter()
                .find(|decl| decl.name == *arg)
                .is_some_and(|decl| match decl.kind {
                    ArgKind::Path => matches!(&cap.scope, Scope::Path(_)),
                    ArgKind::Host => matches!(&cap.scope, Scope::Host(_)),
                    ArgKind::Name => matches!(&cap.scope, Scope::Name(_)),
                    ArgKind::Text | ArgKind::Number | ArgKind::Bool => false,
                }),
            ScopeBinding::FromArgMap { values, .. } => {
                values.values().any(|scope| scope == &cap.scope)
            }
            ScopeBinding::FromArgOrWild { arg, .. } => {
                matches!(&cap.scope, Scope::Wild)
                    || args
                        .iter()
                        .find(|decl| decl.name == *arg)
                        .is_some_and(|decl| match decl.kind {
                            ArgKind::Path => matches!(&cap.scope, Scope::Path(_)),
                            ArgKind::Host => matches!(&cap.scope, Scope::Host(_)),
                            ArgKind::Name => matches!(&cap.scope, Scope::Name(_)),
                            ArgKind::Text | ArgKind::Number | ArgKind::Bool => false,
                        })
            }
        }
    }

fn require_trusted_launcher(client: &ClientIdentity) -> Result<(), String> {
    let pid = client
        .pid
        .ok_or_else(|| "clawd peer pid is unavailable".to_string())?;
    if process_no_new_privs(pid) != Some(false) {
        return Err("App processes cannot manage App sessions".to_string());
    }
    Ok(())
}

fn required_string(params: &Value, key: &str) -> Result<String, String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("{key} is required"))
}

fn required_u32(params: &Value, key: &str) -> Result<u32, String> {
    params
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| format!("{key} is required"))
}

#[cfg(target_os = "linux")]
fn process_no_new_privs(pid: u32) -> Option<bool> {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .and_then(|status| {
            status.lines().find_map(|line| {
                line.strip_prefix("NoNewPrivs:")
                    .and_then(|value| value.trim().parse::<u32>().ok())
            })
        })
        .map(|value| value != 0)
}

#[cfg(not(target_os = "linux"))]
fn process_no_new_privs(_pid: u32) -> Option<bool> {
    None
}

#[cfg(target_os = "linux")]
fn is_descendant_of(mut child: u32, ancestor: u32) -> bool {
    for _ in 0..64 {
        if child == ancestor {
            return true;
        }
        if child <= 1 {
            return false;
        }
        let Some(parent) = process_parent(child) else {
            return false;
        };
        child = parent;
    }
    false
}

#[cfg(not(target_os = "linux"))]
fn is_descendant_of(_child: u32, _ancestor: u32) -> bool {
    false
}

#[cfg(target_os = "linux")]
fn process_parent(pid: u32) -> Option<u32> {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()?
        .lines()
        .find_map(|line| {
            line.strip_prefix("PPid:")
                .and_then(|value| value.trim().parse().ok())
        })
}

#[cfg(target_os = "linux")]
fn process_uid(pid: u32) -> Option<u32> {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()?
        .lines()
        .find_map(|line| {
            line.strip_prefix("Uid:")
                .and_then(|value| value.split_whitespace().next())
                .and_then(|value| value.parse().ok())
        })
}

#[cfg(not(target_os = "linux"))]
fn process_uid(_pid: u32) -> Option<u32> {
    None
}
