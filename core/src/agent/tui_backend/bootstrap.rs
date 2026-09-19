use serde_json::{json, Value};

use super::backend::BackendInfo;
use super::protocol::{safe_text, Params, RpcError};

pub(super) fn read(
    method: &str,
    params: Value,
    info: &BackendInfo,
) -> Option<Result<Value, RpcError>> {
    Some(match method {
        "account/read" => account(params, info),
        "model/list" => models(params, info),
        "config/read" => config(params, info),
        "configRequirements/read" => requirements(params, info),
        "modelProvider/capabilities/read" => Params::new(params, &[]).map(|_| {
            json!({
                "namespaceTools": false,
                "imageGeneration": false,
                "webSearch": false,
            })
        }),
        _ => return None,
    })
}

fn account(value: Value, info: &BackendInfo) -> Result<Value, RpcError> {
    let params = Params::new(value, &["refreshToken"])?;
    if params.boolean("refreshToken", false)? {
        return Err(RpcError::option("refreshToken (use `cos agent setup`)"));
    }
    // A Claw credential is not a Codex/ChatGPT account or an OpenAI API key.
    Ok(json!({
        "account": null,
        "requiresOpenaiAuth": false,
        "claw": {
            "provider": safe_text(&info.provider),
            "model": safe_text(&info.model),
            "ready": info.ready,
            "authenticationManagedBy": "cos agent setup",
        },
    }))
}

fn models(value: Value, info: &BackendInfo) -> Result<Value, RpcError> {
    let params = Params::new(value, &["cursor", "limit", "includeHidden"])?;
    let limit = params.limit(100)?;
    params.boolean("includeHidden", false)?;
    if info.provider.is_empty()
        || info.model.is_empty()
        || info.models.is_empty()
        || info.provider == "mock"
    {
        return Err(RpcError::unavailable(
            "No real Claw model is configured. Run `cos agent setup text` before starting a conversation",
        ));
    }
    let scope = format!("models:{}", info.provider);
    let start = super::pagination::start(&params, &scope, &info.models)?;
    let end = start.saturating_add(limit).min(info.models.len());
    let data: Vec<Value> = info.models[start..end].iter().map(|model| json!({
            "id": model,
            "model": model,
            "displayName": format!("{} (Claw: {})", model, safe_text(&info.provider)),
            "description": "Claw configured-provider catalogue. Selection applies to the task; credentials, provider, fallback and reasoning settings remain owned by Claw.",
            "hidden": false,
            "upgrade": null,
            "upgradeInfo": null,
            "availabilityNux": null,
            "supportedReasoningEfforts": [],
            "defaultReasoningEffort": "none",
            "inputModalities": ["text"],
            "supportsPersonality": false,
            "multiAgentVersion": "disabled",
            "additionalSpeedTiers": [],
            "serviceTiers": [],
            "defaultServiceTier": null,
            "availableAccessPrograms": null,
            "isDefault": *model == info.model,
        })).collect();
    let next = if end < info.models.len() && end > start {
        super::pagination::encode(&scope, &info.models[end - 1], false)
    } else {
        Value::Null
    };
    Ok(json!({ "data": data, "nextCursor": next }))
}

fn effective_config(info: &BackendInfo) -> Value {
    json!({
        "model": safe_text(&info.model),
        "model_provider": "claw",
        "model_providers": {
            "claw": {
                "name": "Claw Agent (broker-owned)",
                "requires_openai_auth": false,
            },
        },
        "approval_policy": "on-request",
        "approvals_reviewer": "user",
        // Codex's sandbox is not the executor. The thread response describes the
        // external Claw boundary; this mode does not change any Claw capability.
        "sandbox_mode": "danger-full-access",
        "web_search": "disabled",
        "projects": {
            (info.home.to_string_lossy().into_owned()): {
                "trust_level": "untrusted",
            },
        },
        "model_reasoning_effort": null,
        "model_reasoning_summary": null,
        "service_tier": null,
        "analytics": { "enabled": false },
        "feedback": { "enabled": false },
        "check_for_update_on_startup": false,
        "claw": {
            "provider": safe_text(&info.provider),
            "ready": info.ready,
            "executionAuthority": "clawd",
            "configurationManagedBy": "cos agent setup",
        },
    })
}

fn config(value: Value, info: &BackendInfo) -> Result<Value, RpcError> {
    let params = Params::new(value, &["includeLayers", "cwd"])?;
    if let Some(cwd) = params.string("cwd")? {
        if cwd.contains('\0') || !std::path::Path::new(cwd).is_absolute() {
            return Err(RpcError::params("cwd must be an absolute path"));
        }
    }
    let include_layers = params.boolean("includeLayers", false)?;
    let config = effective_config(info);
    let metadata = json!({
        "name": { "type": "sessionFlags" },
        "version": env!("CARGO_PKG_VERSION"),
    });
    let origins: serde_json::Map<String, Value> = config
        .as_object()
        .into_iter()
        .flat_map(|config| config.keys())
        .map(|key| (key.clone(), metadata.clone()))
        .collect();
    let mut response = json!({ "config": config, "origins": origins });
    if include_layers {
        response["layers"] = json!([{
            "name": { "type": "sessionFlags" },
            "version": env!("CARGO_PKG_VERSION"),
            "config": response["config"],
        }]);
    }
    Ok(response)
}

fn requirements(value: Value, info: &BackendInfo) -> Result<Value, RpcError> {
    Params::new(value, &[])?;
    Ok(json!({
        "requirements": {
            "modelProvider": "claw",
            "modelProviders": {
                "claw": {
                    "name": "Claw Agent (broker-owned)",
                    "requires_openai_auth": false,
                },
            },
            "allowedApprovalPolicies": ["on-request"],
            "allowedApprovalsReviewers": ["user"],
            "allowedSandboxModes": ["danger-full-access"],
            "allowRemoteControl": false,
            "allowBrowserAndComputerUse": false,
            "allowAppshots": false,
            "checkForUpdateOnStartup": false,
            "feedback": { "enabled": false },
            "models": {
                "newThread": {
                    "model": safe_text(&info.model),
                    "modelReasoningEffort": null,
                    "serviceTier": null,
                },
            },
        },
    }))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tui_backend/bootstrap.rs"
    ));
}
