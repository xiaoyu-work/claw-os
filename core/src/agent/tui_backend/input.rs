use super::backend::BackendInfo;
use super::protocol::{Params, RpcError, MAX_PROMPT_BYTES};

pub(super) const THREAD_FIELDS: &[&str] = &[
    "threadId",
    "model",
    "modelProvider",
    "allowProviderModelFallback",
    "serviceTier",
    "cwd",
    "runtimeWorkspaceRoots",
    "approvalPolicy",
    "approvalsReviewer",
    "sandbox",
    "permissions",
    "config",
    "serviceName",
    "baseInstructions",
    "developerInstructions",
    "personality",
    "multiAgentMode",
    "ephemeral",
    "historyMode",
    "sessionStartSource",
    "threadSource",
    "projectId",
    "environments",
    "dynamicTools",
    "selectedCapabilityRoots",
    "mockExperimentalField",
    "experimentalRawEvents",
    "history",
    "path",
    "excludeTurns",
    "initialTurnsPage",
    "lastTurnId",
    "beforeTurnId",
    "deferGoalContinuation",
];

pub(super) const TURN_FIELDS: &[&str] = &[
    "threadId",
    "input",
    "clientUserMessageId",
    "disabledPluginIds",
    "turnTrigger",
    "toolOutput",
    "responsesapiClientMetadata",
    "additionalContext",
    "environments",
    "cwd",
    "runtimeWorkspaceRoots",
    "approvalPolicy",
    "approvalsReviewer",
    "sandboxPolicy",
    "permissions",
    "model",
    "serviceTier",
    "serviceTierForTurn",
    "effort",
    "summary",
    "personality",
    "outputSchema",
    "collaborationMode",
    "multiAgentMode",
    "cyberAccessProgram",
];

pub(super) fn settings(params: &Params, info: &BackendInfo) -> Result<(), RpcError> {
    for (field, expected) in [
        ("modelProvider", "claw"),
        ("approvalPolicy", "on-request"),
        ("approvalsReviewer", "user"),
    ] {
        if params.string(field)?.is_some_and(|value| value != expected) {
            return Err(RpcError::option(field));
        }
    }
    if let Some(model) = params.string("model")? {
        model_choice(model, info)?;
    }
    if let Some(cwd) = params.string("cwd")? {
        if !std::path::Path::new(cwd).is_absolute() || cwd.contains('\0') {
            return Err(RpcError::params("cwd must be an absolute path"));
        }
        if std::path::Path::new(cwd) != info.home {
            return Err(RpcError::option(
                "cwd (Claw tasks currently run from the owner's home)",
            ));
        }
    }
    if let Some(roots) = params.array("runtimeWorkspaceRoots")? {
        if !roots.is_empty()
            && !(roots.len() == 1
                && roots[0]
                    .as_str()
                    .is_some_and(|root| std::path::Path::new(root) == info.home))
        {
            return Err(RpcError::option("runtimeWorkspaceRoots"));
        }
    }
    if params
        .string("sandbox")?
        .is_some_and(|value| value != "danger-full-access")
    {
        return Err(RpcError::option(
            "sandbox (only external Claw enforcement is available)",
        ));
    }
    if let Some(policy) = params.value("sandboxPolicy") {
        let policy = Params::new(policy.clone(), &["type", "networkAccess"])?;
        if policy.required_string("type")? != "externalSandbox"
            || policy
                .string("networkAccess")?
                .is_some_and(|value| value != "restricted")
        {
            return Err(RpcError::option("sandboxPolicy"));
        }
    }
    // "none"/"auto" are the adapter's advertised no-override presentation
    // defaults, not permission to change the worker's provider configuration.
    for (field, baseline) in [
        ("effort", "none"),
        ("summary", "auto"),
        ("personality", "none"),
        ("serviceTier", "default"),
        ("serviceTierForTurn", "default"),
    ] {
        if params.string(field)?.is_some_and(|value| value != baseline) {
            return Err(RpcError::option(field));
        }
    }
    params.reject_present(&[
        "permissions",
        "baseInstructions",
        "developerInstructions",
        "serviceName",
        "history",
        "path",
        "projectId",
        "selectedCapabilityRoots",
        "mockExperimentalField",
        "multiAgentMode",
        "toolOutput",
        "responsesapiClientMetadata",
        "additionalContext",
        "outputSchema",
        "collaborationMode",
        "cyberAccessProgram",
        "turnTrigger",
    ])?;
    for field in ["environments", "dynamicTools", "disabledPluginIds"] {
        params.reject_nonempty_array(field)?;
    }
    if let Some(config) = params.value("config") {
        let config = Params::new(
            config.clone(),
            &[
                "web_search",
                "model_reasoning_effort",
                "model_reasoning_summary",
                "personality",
            ],
        )?;
        for (field, baseline) in [
            ("web_search", "disabled"),
            ("model_reasoning_effort", "none"),
            ("model_reasoning_summary", "auto"),
            ("personality", "none"),
        ] {
            if config.string(field)?.is_some_and(|value| value != baseline) {
                return Err(RpcError::option(field));
            }
        }
    }
    for field in [
        "ephemeral",
        "experimentalRawEvents",
        "allowProviderModelFallback",
        "deferGoalContinuation",
    ] {
        if params.boolean(field, false)? {
            return Err(RpcError::option(field));
        }
    }
    if params
        .string("historyMode")?
        .is_some_and(|value| !matches!(value, "legacy" | "paginated"))
    {
        return Err(RpcError::params("Invalid historyMode"));
    }
    if params
        .string("threadSource")?
        .is_some_and(|value| value != "user")
    {
        return Err(RpcError::option("threadSource"));
    }
    if params
        .string("sessionStartSource")?
        .is_some_and(|value| !matches!(value, "startup" | "clear"))
    {
        return Err(RpcError::params("Invalid sessionStartSource"));
    }
    Ok(())
}

pub(super) fn model_choice(model: &str, info: &BackendInfo) -> Result<(), RpcError> {
    crate::agent::service::validate_requested_model(Some(model)).map_err(RpcError::params)?;
    if !info.models.iter().any(|candidate| candidate == model) {
        return Err(RpcError::option(
            "model (select from the configured Claw provider's catalogue)",
        ));
    }
    Ok(())
}

pub(super) fn prompt(params: &Params) -> Result<String, RpcError> {
    let input = params
        .array("input")?
        .filter(|input| !input.is_empty() && input.len() <= 64)
        .ok_or_else(|| RpcError::params("input must contain 1..=64 text items"))?;
    let mut parts = Vec::with_capacity(input.len());
    let mut bytes = 0usize;
    for item in input {
        let item = Params::new(item.clone(), &["type", "text", "text_elements"])?;
        if item.required_string("type")? != "text" {
            return Err(RpcError::option(
                "non-text input (attachments, audio, Skill and mention inputs)",
            ));
        }
        item.reject_nonempty_array("text_elements")?;
        let text = item
            .string("text")?
            .ok_or_else(|| RpcError::params("text is required"))?;
        if text.contains('\0') {
            return Err(RpcError::params("Text input contains a NUL character"));
        }
        bytes = bytes.saturating_add(text.len()).saturating_add(1);
        if bytes > MAX_PROMPT_BYTES {
            return Err(RpcError::params("Text input exceeds the Claw prompt limit"));
        }
        parts.push(text.to_string());
    }
    let prompt = parts.join("\n");
    if prompt.trim().is_empty() {
        return Err(RpcError::params("Text input must not be empty"));
    }
    Ok(prompt)
}

pub(super) fn client_message_id(params: &Params) -> Result<Option<String>, RpcError> {
    params
        .string("clientUserMessageId")?
        .map(|id| {
            super::protocol::token(id, "clientUserMessageId")?;
            Ok(id.to_string())
        })
        .transpose()
}

pub(super) fn items_view(params: &Params, default: &str) -> Result<String, RpcError> {
    let view = params.string("itemsView")?.unwrap_or(default);
    if !matches!(view, "full" | "summary" | "notLoaded") {
        return Err(RpcError::params("Invalid itemsView"));
    }
    Ok(view.to_string())
}

pub(super) fn descending(params: &Params, default: bool) -> Result<bool, RpcError> {
    match params.string("sortDirection")? {
        Some("asc") => Ok(false),
        Some("desc") => Ok(true),
        None => Ok(default),
        _ => Err(RpcError::params("Invalid sortDirection")),
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tui_backend/input.rs"
    ));
}
