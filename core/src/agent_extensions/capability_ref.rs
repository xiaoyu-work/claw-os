//! Opaque, event-scoped references to extension-requested capabilities.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::caps::Cap;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityReference {
    pub requested_index: usize,
    pub handle: String,
}

#[derive(Debug, Clone)]
struct Record {
    owner_uid: u32,
    session_id: String,
    task_id: String,
    extension_id: String,
    manifest_digest: String,
    capability_generation: String,
    event_id: String,
    expires_at_ms: u64,
    cap: Cap,
}

#[derive(Default)]
pub struct CapabilityReferenceStore {
    records: Mutex<HashMap<[u8; 32], Record>>,
}

pub struct ReferenceContext<'a> {
    pub owner_uid: u32,
    pub session_id: &'a str,
    pub task_id: &'a str,
    pub extension_id: &'a str,
    pub manifest_digest: &'a str,
    pub capability_generation: &'a str,
    pub event_id: &'a str,
    pub expires_at_ms: u64,
}

impl CapabilityReferenceStore {
    pub fn issue(
        &self,
        context: &ReferenceContext<'_>,
        requested: &[Cap],
    ) -> Result<Vec<CapabilityReference>, String> {
        let now = now_ms();
        if context.expires_at_ms <= now || context.expires_at_ms > now.saturating_add(10_000) {
            return Err("capability reference expiry is outside the event deadline".to_string());
        }
        let mut records = self
            .records
            .lock()
            .map_err(|_| "capability reference store is unavailable".to_string())?;
        records.retain(|_, record| record.expires_at_ms >= now);
        if records.len().saturating_add(requested.len()) > 512 {
            return Err("capability reference store is full".to_string());
        }
        requested
            .iter()
            .cloned()
            .enumerate()
            .map(|(requested_index, cap)| {
                let handle = random_handle()?;
                let key = crate::crypto::sha256_bytes(handle.as_bytes());
                records.insert(
                    key,
                    Record {
                        owner_uid: context.owner_uid,
                        session_id: context.session_id.to_string(),
                        task_id: context.task_id.to_string(),
                        extension_id: context.extension_id.to_string(),
                        manifest_digest: context.manifest_digest.to_string(),
                        capability_generation: context.capability_generation.to_string(),
                        event_id: context.event_id.to_string(),
                        expires_at_ms: context.expires_at_ms,
                        cap,
                    },
                );
                Ok(CapabilityReference {
                    requested_index,
                    handle,
                })
            })
            .collect()
    }

    /// Consume exactly once. Unknown, expired, replayed, or cross-session
    /// handles intentionally return the same error.
    pub fn consume(
        &self,
        context: &ReferenceContext<'_>,
        reference: &CapabilityReference,
    ) -> Result<Cap, String> {
        if reference.handle.len() != 64
            || !reference
                .handle
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err("capability reference is invalid or expired".to_string());
        }
        let key = crate::crypto::sha256_bytes(reference.handle.as_bytes());
        let mut records = self
            .records
            .lock()
            .map_err(|_| "capability reference store is unavailable".to_string())?;
        let Some(record) = records.remove(&key) else {
            return Err("capability reference is invalid or expired".to_string());
        };
        if record.owner_uid != context.owner_uid
            || record.session_id != context.session_id
            || record.task_id != context.task_id
            || record.extension_id != context.extension_id
            || record.manifest_digest != context.manifest_digest
            || record.capability_generation != context.capability_generation
            || record.event_id != context.event_id
            || record.expires_at_ms < now_ms()
        {
            return Err("capability reference is invalid or expired".to_string());
        }
        Ok(record.cap)
    }
}

fn random_handle() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    crate::credential::os_random_bytes(&mut bytes)
        .map_err(|error| format!("generate capability reference: {error}"))?;
    Ok(hex::encode(bytes))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent_extensions/capability_ref.rs"
    ));
}
