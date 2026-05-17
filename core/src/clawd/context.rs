use serde_json::{json, Value};

use super::state::DaemonState;

pub fn snapshot(state: &DaemonState) -> Result<Value, String> {
    let entries = state
        .context_snapshot()
        .into_iter()
        .map(|entry| {
            json!({
                "source": entry.source,
                "updated_at": entry.updated_at,
                "payload": entry.payload,
            })
        })
        .collect::<Vec<_>>();

    Ok(json!({
        "schema": 1,
        "entries": entries,
    }))
}

pub fn update(state: &DaemonState, params: Value) -> Result<Value, String> {
    let source = required_string(&params, "source")?;
    let payload = params.get("payload").cloned().unwrap_or(Value::Null);

    state.update_context(source.clone(), payload);

    Ok(json!({
        "accepted": true,
        "source": source,
    }))
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
