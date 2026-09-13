//! Authenticated metadata previews; no App execution or inferred authorization.

use serde_json::Value;

use crate::activities::ActivityService;

use super::activities::{decode, encode, owner, service_error};
use super::client_identity::ClientIdentity;
use super::protocol::BrokerError;
use super::wire::requests as body;

pub fn preview(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    owner(client)?;
    let request: body::OperationPreview = decode(params)?;
    prepare(
        request.app_id.as_str(),
        request.operation.as_str(),
        request
            .args
            .as_ref()
            .map(|args| args.as_slice())
            .unwrap_or(&[]),
    )
}

pub fn for_activity(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let owner = owner(client)?;
    let request: body::ActivityOperationPreview = decode(params)?;
    crate::activities::validate_id(request.id.as_str()).map_err(service_error)?;
    crate::activities::open_default()
        .map_err(service_error)?
        .get(owner, request.id.as_str())
        .map_err(service_error)?;
    prepare(
        request.app_id.as_str(),
        request.operation.as_str(),
        request
            .args
            .as_ref()
            .map(|args| args.as_slice())
            .unwrap_or(&[]),
    )
}

fn prepare(app_id: &str, operation: &str, args: &[String]) -> Result<Value, BrokerError> {
    crate::operations::validate_arguments(args).map_err(BrokerError::execution)?;
    let root = std::env::var_os("COS_APPS_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "/usr/lib/cos/apps".into());
    let app = crate::apps::find_verified(&root, app_id).map_err(|error| {
        BrokerError::execution(error).classified("operation_preview_unavailable")
    })?;
    let preview = crate::operations::preview(&app, operation, args)
        .map_err(|error| BrokerError::execution(error).classified("operation_preview_invalid"))?;
    encode(preview)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/operation_previews.rs"
    ));
}
