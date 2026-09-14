//! Owner-scoped Activity monetary policy controls.

use serde_json::{json, Value};

use crate::activities::{self, ActivityService};

use super::activities::{decode, encode, owner, service_error};
use super::client_identity::ClientIdentity;
use super::protocol::BrokerError;
use super::wire::requests as body;

pub fn get(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let owner = owner(client)?;
    let request: body::ActivityMonetaryBudgetGet = decode(params)?;
    activities::validate_id(request.id.as_str()).map_err(service_error)?;
    let service = activities::open_default().map_err(service_error)?;
    let activity = service
        .get(owner, request.id.as_str())
        .map_err(service_error)?;
    let budget = service
        .monetary_budget(owner, &activity.id)
        .map_err(service_error)?;
    Ok(json!({"schema":1,"activity_id":activity.id,"monetary_budget":budget}))
}

pub fn set(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let owner = owner(client)?;
    let request: body::ActivityMonetaryBudgetSet = decode(params)?;
    activities::validate_id(request.id.as_str()).map_err(service_error)?;
    request.budget.0.validate().map_err(service_error)?;
    let service = activities::open_default().map_err(service_error)?;
    encode(
        service
            .set_monetary_budget(
                owner,
                request.id.as_str(),
                request.expected_revision,
                request.budget.0,
            )
            .map_err(service_error)?,
    )
}

pub fn enabled(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let owner = owner(client)?;
    let request: body::ActivityMonetaryBudgetEnabled = decode(params)?;
    activities::validate_id(request.id.as_str()).map_err(service_error)?;
    let service = activities::open_default().map_err(service_error)?;
    encode(
        service
            .set_monetary_budget_enabled(
                owner,
                request.id.as_str(),
                request.expected_revision,
                request.enabled,
            )
            .map_err(service_error)?,
    )
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/activity_monetary_budget.rs"
    ));
}
