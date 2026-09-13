//! Owner-scoped annotations and history, never App data or execution authority.

use serde_json::{json, Value};

use crate::activities::{self, ActivityService};

use super::activities::{decode, encode, owner, service_error};
use super::client_identity::ClientIdentity;
use super::protocol::BrokerError;
use super::wire::requests as body;

pub fn list(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let owner = owner(client)?;
    let request: body::ActivityObjectState = decode(params)?;
    activities::validate_id(request.id.as_str()).map_err(service_error)?;
    let reference = request.reference.as_ref().map(|value| value.as_str());
    if let Some(reference) = reference {
        crate::objects::parse_reference(reference)
            .map_err(|error| BrokerError::execution(error.to_string()))?;
    }
    let limit = match request.limit {
        None => activities::DEFAULT_LIST_LIMIT,
        Some(limit) if (1..=activities::MAX_LIST_LIMIT as u64).contains(&limit) => limit as usize,
        Some(_) => {
            return Err(BrokerError::execution(
                "object-state limit must be between 1 and 100",
            ))
        }
    };
    let service = activities::open_default().map_err(service_error)?;
    let activity = service
        .get(owner, request.id.as_str())
        .map_err(service_error)?;
    let entries = service
        .object_state(owner, &activity.id, reference, limit)
        .map_err(service_error)?;
    Ok(json!({"schema":1,"activity_id":activity.id,"entries":entries}))
}

pub fn record(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let owner = owner(client)?;
    let request: body::ActivityObjectStateRecord = decode(params)?;
    activities::validate_id(request.id.as_str()).map_err(service_error)?;
    request.entry.0.validate().map_err(service_error)?;
    let service = activities::open_default().map_err(service_error)?;
    encode(
        service
            .record_object_state(owner, request.id.as_str(), request.entry.0)
            .map_err(service_error)?,
    )
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/activity_object_state.rs"
    ));
}
