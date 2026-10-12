//! Safe source references from successful built-in memory reads, never their contents.

use super::inspection::MemoryReference;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MemoryAccess {
    pub source: String,
    pub revision: String,
    pub reference: MemoryReference,
}

pub fn references(tool: &str, result: &str) -> Vec<MemoryAccess> {
    if !matches!(
        tool,
        "cos_memory" | "cos_recall" | "cos_app_memory" | "cos_recall_semantic"
    ) {
        return Vec::new();
    }
    let Some(envelope) = crate::agent::trust::envelope::parse(result) else {
        tracing::warn!(tool, "successful memory result has no source envelope");
        return Vec::new();
    };
    let value: Value = match serde_json::from_str(&envelope.payload) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(tool, %error, "memory source metadata is not structured");
            return Vec::new();
        }
    };
    let mut candidates = vec![&value];
    for key in ["hits", "rows", "messages"] {
        if let Some(rows) = value.get(key).and_then(Value::as_array) {
            candidates.extend(rows.iter().take(16));
        }
    }

    let mut result = Vec::new();
    for value in candidates {
        let Some(revision) = value
            .get("revision")
            .and_then(Value::as_str)
            .filter(|value| {
                value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        else {
            continue;
        };
        let reference = match tool {
            "cos_memory" => value.get("name").and_then(Value::as_str).map(|name| {
                (
                    format!("notes/{name}"),
                    MemoryReference::Note { name: name.into() },
                )
            }),
            "cos_recall" => value
                .get("id")
                .and_then(Value::as_i64)
                .filter(|id| *id > 0)
                .map(|id| {
                    (
                        format!("conversation/message/{id}"),
                        MemoryReference::Message { id: id.to_string() },
                    )
                }),
            "cos_app_memory" => value
                .get("id")
                .and_then(Value::as_i64)
                .filter(|id| *id > 0)
                .map(|id| {
                    (
                        format!(
                            "app/{}/message/{id}",
                            value
                                .get("source")
                                .and_then(Value::as_str)
                                .unwrap_or("unknown")
                        ),
                        MemoryReference::App { id: id.to_string() },
                    )
                }),
            _ => value
                .get("namespace")
                .and_then(Value::as_str)
                .zip(value.get("key").and_then(Value::as_str))
                .map(|(namespace, key)| {
                    (
                        format!("{namespace}/{key}"),
                        MemoryReference::Semantic {
                            namespace: namespace.into(),
                            key: key.into(),
                        },
                    )
                }),
        };
        if let Some((source, reference)) = reference {
            if source.len() <= 512
                && !result
                    .iter()
                    .any(|read: &MemoryAccess| read.source == source && read.revision == revision)
            {
                result.push(MemoryAccess {
                    source,
                    revision: revision.into(),
                    reference,
                });
            }
        }
    }
    result.truncate(16);
    result
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/memory/access.rs"
    ));
}
