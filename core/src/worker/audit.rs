//! Typed, safe audit facts for worker launches.
//!
//! Records answer "what isolation actually ran, and how did it end" —
//! never "what did the worker do". Nothing here carries a host path,
//! an argument, an environment value, worker output or a secret:
//!
//! * the policy is identified by its content digest;
//! * mounts are counted per class, not listed;
//! * the network is a mode plus an endpoint count;
//! * limits are the numbers the kernel enforced;
//! * outcomes are exit code, timeout and cancel flags.
//!
//! Mutation and event authority stay with `clawd`: this appends to the
//! capability audit log the rest of the caps subsystem already owns.

use serde_json::Value;

/// Event names. Stable strings so an audit projection can filter on
/// them without parsing.
const LAUNCH: &str = "worker.sandbox.launch";
const REFUSED: &str = "worker.sandbox.refused";
const EXEMPT: &str = "worker.sandbox.exempt";
const OUTCOME: &str = "worker.sandbox.outcome";

fn emit(event: &str, mut facts: Value) {
    if let Some(object) = facts.as_object_mut() {
        object.insert("event".to_string(), Value::String(event.to_string()));
        object.insert(
            "at".to_string(),
            Value::String(chrono::Utc::now().to_rfc3339()),
        );
    }
    crate::audit::log_cap_decision(facts);
}

/// One worker started under the policy described by `facts`.
pub fn launched(facts: &Value, session_id: Option<&str>) {
    let mut facts = facts.clone();
    if let (Some(object), Some(session)) = (facts.as_object_mut(), session_id) {
        object.insert("session".to_string(), Value::String(session.to_string()));
    }
    emit(LAUNCH, facts);
}

/// A launch was refused before any untrusted code ran.
pub fn refused(label: &str, tier: &str, reason: &str) {
    emit(
        REFUSED,
        serde_json::json!({
            "label": label,
            "tier": tier,
            // `reason` is kernel-authored text: a missing facility, a
            // policy validation failure, a forbidden mount root.
            "reason": reason,
        }),
    );
}

/// A trusted, kernel-allowlisted process ran outside the sandbox.
pub fn exempt(label: &str, tier: &str, reason: &str) {
    emit(
        EXEMPT,
        serde_json::json!({
            "label": label,
            "tier": tier,
            "reason": reason,
        }),
    );
}

/// A worker finished, timed out or was cancelled.
pub fn outcome(policy_digest: &str, label: &str, facts: Value) {
    let mut record = serde_json::json!({
        "policy": policy_digest,
        "label": label,
    });
    if let (Some(target), Some(source)) = (record.as_object_mut(), facts.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
    emit(OUTCOME, record);
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/worker/audit.rs"
    ));
}
