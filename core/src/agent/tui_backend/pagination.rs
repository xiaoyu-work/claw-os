use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::protocol::{Params, RpcError};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Anchor {
    pub scope: String,
    pub id: String,
    pub inclusive: bool,
}

pub(super) fn decode(params: &Params, scope: &str) -> Result<Option<Anchor>, RpcError> {
    let Some(cursor) = params.string("cursor")? else {
        return Ok(None);
    };
    if cursor.len() > 2048 {
        return Err(RpcError::params("Pagination cursor is too long"));
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| RpcError::params("Invalid pagination cursor"))?;
    let anchor: Anchor = serde_json::from_slice(&bytes)
        .map_err(|_| RpcError::params("Invalid pagination cursor"))?;
    if anchor.scope != scope || anchor.id.is_empty() || anchor.id.len() > 768 {
        return Err(RpcError::params(
            "Pagination cursor belongs to another query",
        ));
    }
    Ok(Some(anchor))
}

pub(super) fn encode(scope: &str, id: &str, inclusive: bool) -> Value {
    let value = serde_json::json!({ "scope": scope, "id": id, "inclusive": inclusive });
    Value::String(URL_SAFE_NO_PAD.encode(value.to_string()))
}

pub(super) fn start(params: &Params, scope: &str, ids: &[String]) -> Result<usize, RpcError> {
    let Some(anchor) = decode(params, scope)? else {
        return Ok(0);
    };
    let index = ids
        .iter()
        .position(|id| *id == anchor.id)
        .ok_or_else(|| RpcError::stale("Pagination anchor no longer exists"))?;
    Ok(index + usize::from(!anchor.inclusive))
}

pub(super) fn scope(prefix: &str, params: &Value) -> String {
    use sha2::{Digest, Sha256};

    let mut filters = params.clone();
    if let Some(filters) = filters.as_object_mut() {
        for key in ["cursor", "limit", "sortDirection", "itemsView"] {
            filters.remove(key);
        }
        filters.retain(|_, value| !value.is_null());
    }
    let digest = Sha256::digest(filters.to_string().as_bytes());
    format!("{prefix}:{}", hex::encode(&digest[..12]))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tui_backend/pagination.rs"
    ));
}
