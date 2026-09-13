//! Owner-scoped App object descriptions and reference-only Activity attachment.

use serde_json::{json, Value};

use crate::activities::{self, ActivityPatch, ActivityResource, ActivityService};
use crate::objects::{self, ObjectCatalog};

use super::activities::{decode, encode, owner, service_error};
use super::client_identity::ClientIdentity;
use super::protocol::BrokerError;
use super::wire::requests as body;

pub fn list(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let owner = owner(client)?;
    let request: body::ActivityObjects = decode(params)?;
    activities::validate_id(request.id.as_str()).map_err(service_error)?;
    let activity = activities::open_default()
        .map_err(service_error)?
        .get(owner, request.id.as_str())
        .map_err(service_error)?;
    let catalog = objects::default_catalog();
    let entries = activity
        .resources
        .iter()
        .filter(|resource| objects::is_app_reference(&resource.reference))
        .map(|resource| {
            let (status, description, error) = match objects::parse_reference(&resource.reference) {
                Err(error) => ("invalid", None, Some(error.to_string())),
                Ok(object) => match catalog.describe(&object) {
                    Ok(description) => ("declared", Some(description), None),
                    Err(error) => ("unavailable", None, Some(error.to_string())),
                },
            };
            json!({
                "label": resource.label,
                "reference": resource.reference,
                "status": status,
                "description": description,
                "error": error,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "schema": 1,
        "activity_id": activity.id,
        "objects": entries,
    }))
}

pub fn attach(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let owner = owner(client)?;
    let request: body::ActivityObjectAttach = decode(params)?;
    activities::validate_id(request.id.as_str()).map_err(service_error)?;
    let reference = objects::format_reference(&request.object.0)
        .map_err(|error| BrokerError::execution(error.to_string()))?;
    let resource = ActivityResource {
        label: request.label.as_str().to_string(),
        reference,
    };
    ActivityPatch {
        resources: Some(vec![resource.clone()]),
        ..ActivityPatch::default()
    }
    .validate()
    .map_err(service_error)?;
    let service = activities::open_default().map_err(service_error)?;
    // Authenticate Activity ownership before any App discovery. The provider
    // rechecks editability inside the same transaction that changes resources.
    let activity = service
        .get(owner, request.id.as_str())
        .map_err(service_error)?;
    objects::default_catalog()
        .describe(&request.object.0)
        .map_err(|error| {
            BrokerError::execution(error.to_string()).classified("activity_object_unavailable")
        })?;
    let updated = service
        .add_resource(owner, &activity.id, resource)
        .map_err(service_error)?;
    encode(updated)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/activity_objects.rs"
    ));
}
