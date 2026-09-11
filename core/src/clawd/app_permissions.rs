//! OS App-permission service; capability and owner binding are independent of UI identity.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{authority::Decision, client_identity::ClientIdentity};
use crate::approvals::{self, app_policy};
use crate::caps::{Cap, Need, Scope, ScopeBinding, Verb};

pub async fn control(
    params: Value,
    client: &ClientIdentity,
    authority: Option<&Decision>,
) -> Result<Value, String> {
    let uid = client.require_uid()?;
    let action = super::permissions::required_string(&params, "action")?;
    if !matches!(action.as_str(), "list" | "show" | "request" | "revoke") {
        return Err("supported App permission actions: list, show, request, revoke; approval requires the privileged helper".into());
    }
    if action != "list" {
        let app = super::permissions::required_string(&params, "app_id")?;
        if app.len() > 128
            || !app.starts_with(|c: char| c.is_ascii_lowercase())
            || !app
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_')
        {
            return Err("invalid installed App id".into());
        }
    }
    if matches!(action.as_str(), "request" | "revoke") {
        let key = super::permissions::required_string(&params, "permission_id")?;
        if key.len() != 64 || !key.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err("permission_id must be the exact key returned by permissions_show".into());
        }
    }
    if action == "request" {
        super::permissions::required_string(&params, "reason")?;
    }
    if let Some(authority) = authority {
        if authority.owner_uid() != uid {
            return Err("App permission authority belongs to another owner".into());
        }
        let _authorized =
            authority.require(Cap::new(Verb::SYS_PERMISSIONS, Scope::name("manage")))?;
    } else {
        super::app_sessions::require_non_app_caller(client).await?;
    }
    let home = client.require_home_dir()?;
    crate::paths::with_user_override(uid, home, async move {
        if action == "list" {
            let root = std::path::PathBuf::from(std::env::var("COS_APPS_DIR")
                .unwrap_or_else(|_| "/usr/lib/cos/apps".into()));
            let discovered = crate::apps::discover_all(&root);
            let mut apps = Vec::new();
            for id in discovered.verified.keys().take(256) {
                let app = super::app_sessions::installed_app(id)?;
                apps.push(json!({"app_id": id, "name": app.manifest.name,
                    "trust": app.trust_label(), "version": app.manifest.version}));
            }
            let quarantined: Vec<_> = discovered.quarantined.iter().take(256)
                .map(|(id, app)| json!({"app_id":id, "error":app.quarantine_reason()})).collect();
            return Ok(json!({"apps": apps, "quarantined": quarantined,
                "truncated": discovered.verified.len() > 256 || discovered.quarantined.len() > 256}));
        }
        let app_id = super::permissions::required_string(&params, "app_id")?;
        let app = super::app_sessions::installed_app(&app_id)?;
        let ceiling = app.require_verified()?.ceiling();
        let needs: Vec<&Need> = app.manifest.operations.values().flat_map(|operation| operation.needs.iter())
            .chain(app.manifest.mcp.iter().flat_map(|mcp| mcp.tools.iter()).flat_map(|tool| tool.needs.iter()))
            .collect();
        let blocks = app_policy::blocks(uid, &app_id)?;
        if action == "show" {
            let live = super::authority::authority().app_caps(uid, &app_id);
            let permissions = needs.iter().map(|need| {
                let cap = fixed_cap(need);
                let within_ceiling = cap.as_ref().is_some_and(|cap|
                    ceiling.clamp(&crate::caps::CapSet::from_caps([cap.clone()])).1.is_empty());
                let manageable = within_ceiling && cap.as_ref().is_some_and(|cap| app_policy::supported(cap.verb));
                let enabled = cap.as_ref().map(|cap| {
                    let mut enabled = within_ceiling;
                    for block in blocks.iter().filter(|block| block.cap.covers(cap) || cap.covers(&block.cap)) {
                        enabled &= block.enabled(uid, &app_id)?;
                    }
                    Ok::<_, String>(enabled)
                }).transpose()?;
                Ok(json!({
                    "permission_id": permission_id(need), "declared": need,
                    "capability": cap, "manageable": manageable, "enabled": enabled,
                    "within_package_ceiling": within_ceiling,
                    "live_granted": cap.as_ref().map(|cap| live.covers(cap) && enabled == Some(true)),
                    "limitation": if manageable { None } else { Some("Argument-bound or direct-resource permission: the OS cannot safely revoke its worker mounts/native authority yet.") },
                }))
            }).collect::<Result<Vec<_>, String>>()?;
            let prefix = format!("{}{uid}:{app_id}:", app_policy::SESSION_PREFIX);
            let pending: Vec<_> = approvals::list_pending_for_owner(Some(uid)).into_iter()
                .filter(|request| request.session.starts_with(&prefix)).collect();
            let recent: Vec<_> = approvals::list_recent_for_owner(1000, Some(uid)).into_iter()
                .filter(|resolved| resolved.request.session.starts_with(&prefix)).collect();
            return Ok(json!({"app_id": app_id, "trust": app.trust_label(),
                "permissions": permissions, "pending": pending, "recent": recent,
                "semantics": "enabled removes only an owner/App deny gate; actual calls still require verified manifest, caller grants and exact arguments. live_granted is a current daemon grant, not a promise of launch."}));
        }
        let key = super::permissions::required_string(&params, "permission_id")?;
        let need = needs.iter().find(|need| permission_id(need) == key)
            .ok_or("permission is not declared by the currently verified App; refresh permission details")?;
        let cap = fixed_cap(need).ok_or("argument-bound permissions cannot yet be managed")?;
        if !app_policy::supported(cap.verb) {
            return Err("direct-resource permissions cannot yet be managed".into());
        }
        if !ceiling.clamp(&crate::caps::CapSet::from_caps([cap.clone()])).1.is_empty() {
            return Err("permission exceeds the verified package's trust ceiling".into());
        }
        if action == "revoke" {
            let block = app_policy::revoke(uid, &app_id, cap)?;
            #[cfg(target_os = "linux")]
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let retired = super::authority::authority().revoke_app(uid, &app_id);
            let audit = super::authority::audit::record_app_permission_revoked(uid, &app_id, &block.cap, block.generation, retired)
                .map_err(|error| format!("App permission was disabled but revocation audit failed: {error}; refresh status"));
            #[cfg(target_os = "linux")]
            let retirement = {
                let app = app_id.clone();
                super::gui::retirement::wait(deadline, move |deadline| super::gui::retire_app(
                    uid, &app, deadline,
                )).await
                    .map_err(|error| format!("App permission is disabled but GUI retirement did not complete: {error}"))
            };
            #[cfg(not(target_os = "linux"))]
            let retirement = Ok(());
            finish_revocation(audit, retirement)?;
            return Ok(json!({"app_id":app_id, "permission_id":key, "enabled":false,
                "revoked":true, "generation":block.generation, "retired_grants":retired,
                "restart_required":true}));
        }
        let reason = super::permissions::required_string(&params, "reason")?;
        let block = blocks.iter().find(|block| block.cap == cap)
            .ok_or("permission is already enabled; permission management does not expand the manifest or launcher ceiling")?;
        if block.enabled(uid, &app_id)? {
            return Err("permission is already enabled".into());
        }
        let session = block.session(uid, &app_id);
        let id = match approvals::find_pending_exact(&session, &cap, Some(uid), None) {
            Some(request) => request.id,
            None => approvals::submit_owned(cap.verb, cap.scope, session, reason,
                Some(format!("App permission restore {app_id}")), Some(uid))?,
        };
        Ok(json!({"id":id, "status":"pending", "enabled":false, "app_id":app_id,
            "required_duration":"forever", "approval":"trusted human helper; never an MCP tool"}))
    }).await
}

fn finish_revocation(audit: Result<(), String>, retirement: Result<(), String>) -> Result<(), String> {
    match (audit, retirement) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(audit), Err(retirement)) => Err(format!("{audit}; {retirement}")),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
    }
}

fn fixed_cap(need: &Need) -> Option<Cap> {
    let scope = match &need.scope {
        ScopeBinding::Fixed { scope } => scope.clone(),
        ScopeBinding::Wild => Scope::Wild,
        _ => return None,
    };
    Some(Cap::new(need.verb, scope))
}

pub(crate) fn validate_approval(request: &approvals::Request) -> Result<(), String> {
    approval_app(request).map(|_| ())
}

pub(crate) fn approval_app(request: &approvals::Request) -> Result<crate::apps::App, String> {
    let (_, app_id) = app_policy::validate_request(request)?;
    let app = super::app_sessions::installed_app(&app_id)?;
    let cap = Cap::new(
        Verb::parse(&request.verb).ok_or("unknown permission verb")?,
        request.scope.clone(),
    );
    let declared = app
        .manifest
        .operations
        .values()
        .flat_map(|operation| operation.needs.iter())
        .chain(
            app.manifest
                .mcp
                .iter()
                .flat_map(|mcp| mcp.tools.iter())
                .flat_map(|tool| tool.needs.iter()),
        )
        .any(|need| fixed_cap(need).as_ref() == Some(&cap));
    if !declared {
        return Err("permission is no longer declared by the verified App".into());
    }
    if !app
        .require_verified()?
        .ceiling()
        .clamp(&crate::caps::CapSet::from_caps([cap]))
        .1
        .is_empty()
    {
        return Err("permission exceeds the current verified package ceiling".into());
    }
    Ok(app)
}

fn permission_id(need: &Need) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(need).expect("manifest need serialization"))
    )
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/app_permissions.rs"
    ));
}
