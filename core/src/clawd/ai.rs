//! App AI runs in the broker's owner-scoped gate, never in an App sandbox.

use serde_json::Value;

use super::{authority::Decision, client_identity::ClientIdentity, protocol::BrokerError};
use crate::ai::gate;

const CHAT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
static IN_FLIGHT: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

pub async fn chat(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, BrokerError> {
    let body: super::wire::requests::AiChat =
        serde_json::from_value(params).map_err(|_| BrokerError::execution("invalid AI request"))?;
    authority
        .require_app(body.app_id.as_str())
        .map_err(BrokerError::authorization)?;
    let session = authority.session().map_err(BrokerError::authorization)?;
    if session.pending_bind || session.app_id.as_deref() != authority.app_id() {
        return Err(BrokerError::authorization(
            "AI requires the registered, bound App session",
        ));
    }
    // Worker relays call handlers directly, so enforce bounds here too.
    let _permit = IN_FLIGHT
        .try_acquire()
        .map_err(|_| BrokerError::unavailable("AI gate is busy"))?;
    let home = client.require_home_dir()?;
    let req = gate::ChatRequest {
        app_id: body.app_id.as_str().into(),
        origin: body.origin.as_str().into(),
        prompt: Some(body.prompt.as_str().into()),
        system: body.system.map(|s| s.as_str().into()),
        max_units: body.max_units,
        tools: body.tools.as_slice().to_vec(),
        ..Default::default()
    };
    crate::paths::with_user_override(authority.owner_uid(), home, async {
        // Only daemon configuration is consulted. Client environment fields
        // are neither transported nor installed in this request scope.
        crate::config::with_snapshot(crate::config::load_user_config(), async {
            tokio::time::timeout(CHAT_TIMEOUT, gate::chat_authorized(req, authority))
                .await
                .map_err(|_| BrokerError::unavailable("AI request deadline exceeded"))?
                .map_err(|error| {
                    BrokerError::with_data(
                        error.to_string(),
                        serde_json::json!({
                            "ai_error": {
                                "error": error.to_string(),
                                "code": error.wire_code(),
                                "detail": match &error {
                                    gate::AiError::Denied(detail) => Some(detail.clone()),
                                    _ => None,
                                },
                            }
                        }),
                    )
                })
                .and_then(|result| {
                    serde_json::to_value(result)
                        .map_err(|error| BrokerError::execution(error.to_string()))
                })
        })
        .await
    })
    .await
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/ai.rs"
    ));
}
