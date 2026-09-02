//! Opaque, event-scoped references to extension-requested capabilities.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::agent_extensions::manifest::ExtensionActionPolicy;
use crate::caps::Cap;
use crate::extension_host::abi::MonotonicDeadlineNs;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityReference {
    pub requested_index: usize,
    pub handle: String,
}

#[derive(Debug, Clone)]
struct Record {
    context: OwnedReferenceContext,
    requested_index: usize,
    allowed_tool: String,
    policy_id: String,
    cap: Cap,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OwnedReferenceContext {
    owner_uid: u32,
    session_id: String,
    task_id: String,
    extension_id: String,
    manifest_digest: String,
    capability_generation: String,
    event_id: String,
    deadline: MonotonicDeadlineNs,
}

pub struct CapabilityReferenceStore {
    max_records: usize,
    records: Mutex<HashMap<[u8; 32], Record>>,
}

impl Default for CapabilityReferenceStore {
    fn default() -> Self {
        Self::new(512)
    }
}

#[derive(Debug, Clone)]
pub struct ReferenceContext<'a> {
    pub owner_uid: u32,
    pub session_id: &'a str,
    pub task_id: &'a str,
    pub extension_id: &'a str,
    pub manifest_digest: &'a str,
    pub capability_generation: &'a str,
    pub event_id: &'a str,
    pub deadline: MonotonicDeadlineNs,
}

impl ReferenceContext<'_> {
    fn owned(&self) -> OwnedReferenceContext {
        OwnedReferenceContext {
            owner_uid: self.owner_uid,
            session_id: self.session_id.to_string(),
            task_id: self.task_id.to_string(),
            extension_id: self.extension_id.to_string(),
            manifest_digest: self.manifest_digest.to_string(),
            capability_generation: self.capability_generation.to_string(),
            event_id: self.event_id.to_string(),
            deadline: self.deadline,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ActionReferenceBinding {
    pub reference: CapabilityReference,
    pub action_id: String,
    pub tool: String,
    pub policy_id: String,
    pub input_digest: String,
    pub capability: Cap,
    pub operation_digest: String,
}

pub struct IssuedReferenceLease {
    store: Arc<CapabilityReferenceStore>,
    context: OwnedReferenceContext,
    references: Vec<CapabilityReference>,
    keys: Vec<[u8; 32]>,
    resolved: bool,
}

impl CapabilityReferenceStore {
    pub fn new(max_records: usize) -> Self {
        Self {
            max_records,
            records: Mutex::new(HashMap::new()),
        }
    }

    pub fn issue_event(
        self: &Arc<Self>,
        context: &ReferenceContext<'_>,
        requested: &[Cap],
        policies: &[ExtensionActionPolicy],
    ) -> Result<IssuedReferenceLease, String> {
        let remaining = context.deadline.remaining()?;
        if remaining > std::time::Duration::from_secs(10) {
            return Err("capability reference expiry is outside the event deadline".to_string());
        }
        let owned = context.owned();
        let mut pending = Vec::with_capacity(policies.len());
        let mut unique = HashSet::new();
        for policy in policies {
            let cap = requested
                .get(policy.requested_index)
                .cloned()
                .ok_or_else(|| "extension action policy named an unknown capability".to_string())?;
            let handle = random_handle()?;
            let key = crate::crypto::sha256_bytes(handle.as_bytes());
            if !unique.insert(key) {
                return Err("capability reference generation collided".to_string());
            }
            pending.push((
                key,
                CapabilityReference {
                    requested_index: policy.requested_index,
                    handle,
                },
                Record {
                    context: owned.clone(),
                    requested_index: policy.requested_index,
                    allowed_tool: policy.tool.clone(),
                    policy_id: policy.policy_id.clone(),
                    cap,
                },
            ));
        }

        let mut records = self
            .records
            .lock()
            .map_err(|_| "capability reference store is unavailable".to_string())?;
        records.retain(|_, record| record.context.deadline.remaining().is_ok());
        if records.len().saturating_add(pending.len()) > self.max_records {
            return Err("capability reference store is full".to_string());
        }
        if pending.iter().any(|(key, _, _)| records.contains_key(key)) {
            return Err("capability reference generation collided".to_string());
        }
        let mut references = Vec::with_capacity(pending.len());
        let mut keys = Vec::with_capacity(pending.len());
        for (key, reference, record) in pending {
            records.insert(key, record);
            keys.push(key);
            references.push(reference);
        }
        drop(records);
        Ok(IssuedReferenceLease {
            store: Arc::clone(self),
            context: owned,
            references,
            keys,
            resolved: false,
        })
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.records
            .lock()
            .map(|records| records.len())
            .unwrap_or(0)
    }
}

impl IssuedReferenceLease {
    pub fn references(&self) -> &[CapabilityReference] {
        &self.references
    }

    /// Validate and consume every proposed-action reference atomically.
    ///
    /// Success and failure both retire the complete event lease, so an
    /// extension cannot execute a valid prefix or replay an unused reference.
    pub fn consume_all(mut self, bindings: &[ActionReferenceBinding]) -> Result<(), String> {
        let mut records = self
            .store
            .records
            .lock()
            .map_err(|_| "capability reference store is unavailable".to_string())?;
        let mut seen = HashSet::new();
        let valid = self.context.deadline.remaining().is_ok()
            && bindings.iter().all(|binding| {
                valid_handle(&binding.reference.handle)
                    && seen.insert(binding.reference.handle.clone())
                    && records
                        .get(&crate::crypto::sha256_bytes(
                            binding.reference.handle.as_bytes(),
                        ))
                        .is_some_and(|record| {
                            record.context == self.context
                                && record.requested_index == binding.reference.requested_index
                                && record.allowed_tool == binding.tool
                                && record.policy_id == binding.policy_id
                                && record.cap == binding.capability
                                && valid_digest(&binding.input_digest)
                                && valid_digest(&binding.operation_digest)
                                && !binding.action_id.is_empty()
                                && binding.action_id.len() <= 128
                        })
            });
        for key in &self.keys {
            records.remove(key);
        }
        self.resolved = true;
        if valid {
            Ok(())
        } else {
            Err("capability reference is invalid or expired".to_string())
        }
    }
}

impl Drop for IssuedReferenceLease {
    fn drop(&mut self) {
        if self.resolved {
            return;
        }
        if let Ok(mut records) = self.store.records.lock() {
            for key in &self.keys {
                records.remove(key);
            }
        }
    }
}

fn valid_handle(handle: &str) -> bool {
    handle.len() == 64
        && handle
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn valid_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn random_handle() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    crate::credential::os_random_bytes(&mut bytes)
        .map_err(|error| format!("generate capability reference: {error}"))?;
    Ok(hex::encode(bytes))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent_extensions/capability_ref.rs"
    ));
}
