use serde_json::{json, Value};

use super::state::DaemonState;

pub fn snapshot(state: &DaemonState) -> Result<Value, String> {
    refresh_builtin_sources(state);
    let entries = state
        .context_snapshot()
        .into_iter()
        .map(|entry| {
            json!({
                "source": entry.source,
                "updated_at": entry.updated_at,
                "payload": entry.payload,
                "metadata": entry.metadata,
            })
        })
        .collect::<Vec<_>>();

    Ok(json!({
        "schema": 1,
        "entries": entries,
    }))
}

pub fn sources(state: &DaemonState) -> Result<Value, String> {
    refresh_builtin_sources(state);
    let sources = state
        .context_snapshot()
        .into_iter()
        .map(|entry| {
            json!({
                "source": entry.source,
                "updated_at": entry.updated_at,
                "metadata": entry.metadata,
            })
        })
        .collect::<Vec<_>>();

    Ok(json!({
        "schema": 1,
        "sources": sources,
    }))
}

pub fn update(state: &DaemonState, params: Value) -> Result<Value, String> {
    let source = required_string(&params, "source")?;
    let payload = params.get("payload").cloned().unwrap_or(Value::Null);
    let metadata = params.get("metadata").cloned().unwrap_or_else(|| json!({}));

    state.update_context(source.clone(), payload, metadata);

    Ok(json!({
        "accepted": true,
        "source": source,
    }))
}

pub fn refresh_builtin_sources(state: &DaemonState) {
    collect_session_environment(state);
}

fn collect_session_environment(state: &DaemonState) {
    let keys = [
        "XDG_CURRENT_DESKTOP",
        "XDG_SESSION_TYPE",
        "DESKTOP_SESSION",
        "WAYLAND_DISPLAY",
        "DISPLAY",
        "XDG_RUNTIME_DIR",
        "COS_RUNTIME_DIR",
        "COS_DATA_DIR",
        "SHELL",
        "LANG",
    ];
    let mut env = serde_json::Map::new();
    for key in keys {
        if let Ok(value) = std::env::var(key) {
            if !value.is_empty() {
                env.insert(key.to_string(), Value::String(value));
            }
        }
    }
    state.update_context(
        "clawd.environment".to_string(),
        Value::Object(env),
        json!({
            "kind": "builtin",
            "collector": "session_environment",
        }),
    );
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
