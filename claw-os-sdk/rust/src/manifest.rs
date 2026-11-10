//! Pure owner-path validation and lexical previews, never authority or mounts.
use serde_json::{json, Value};

pub fn validate_owner_path(verb: &str, path: &str) -> Result<(), String> {
    super::generated::validate_owner_path_need(&json!({
        "verb": verb, "scope": {"kind": "owner-path", "path": path},
    })).map_err(|error| error.to_string())?;
    if path.len() > 4096 || path.split('/').any(|part| part.len() > 255) {
        return Err("owner-path exceeds its UTF-8 path or segment bound".into());
    }
    Ok(())
}

/// Read-only lexical preview. Root independently supplies and verifies home.
pub fn resolve_owner_path(verb: &str, path: &str, owner_home: &str) -> Result<String, String> {
    validate_owner_path(verb, path)?;
    let home = owner_home.strip_prefix('/').ok_or("owner home must be absolute")?;
    if owner_home.contains('*') {
        return Err("owner home must not contain a pattern".into());
    }
    validate_owner_path("fs.read", home)?;
    let resolved = format!("{owner_home}/{path}");
    if resolved.len() > 4096 {
        return Err("resolved owner path exceeds 4096 UTF-8 bytes".into());
    }
    Ok(resolved)
}

pub(crate) fn validate_owner_path_bindings(manifest: &Value) -> Result<(), String> {
    let operations = manifest.get("operations").and_then(Value::as_object).into_iter().flat_map(|ops| ops.values());
    let tools = manifest.get("mcp").and_then(|mcp| mcp.get("tools")).and_then(Value::as_array).into_iter().flatten();
    let desktop = manifest.get("desktop").into_iter();
    let entries = manifest.get("desktop").and_then(|desktop| desktop.get("entries"))
        .and_then(Value::as_object).into_iter().flat_map(|entries| entries.values());
    for entry in operations.chain(tools).chain(desktop.clone()).chain(entries.clone()) {
        validate_owner_path_needs(entry.get("needs").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default())?;
    }
    for gui in desktop.chain(entries) {
        if let Some(optional) = gui.get("optional_needs") {
            let optional = optional.as_array().ok_or("GUI optional_needs must be an array")?;
            validate_owner_path_needs(optional)?;
        }
    }
    Ok(())
}

fn validate_owner_path_needs(needs: &[Value]) -> Result<(), String> {
    for need in needs {
        if need.get("scope").and_then(|scope| scope.get("kind")).and_then(Value::as_str) == Some("owner-path") {
            super::generated::validate_owner_path_need(need).map_err(|error| error.to_string())?;
            validate_owner_path(need["verb"].as_str().ok_or("invalid owner-path verb")?,
                need["scope"]["path"].as_str().ok_or("invalid owner-path")?)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/manifest.rs"));
}
