//! Authenticated owner-scoped Activity continuity import and export.

use serde_json::Value;

use crate::activities::{self, ActivityContinuityDocument, ActivityService};

use super::activities::{decode, encode, owner, service_error};
use super::client_identity::ClientIdentity;
use super::protocol::BrokerError;
use super::wire::requests as body;

pub fn export(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let owner_uid = owner(client)?;
    let request: body::ActivityContinuityExport = decode(params)?;
    activities::validate_id(request.id.as_str()).map_err(service_error)?;
    let result = activities::open_default()
        .map_err(service_error)?
        .export_continuity(owner_uid, request.id.as_str());
    match result {
        Ok(document) => {
            super::audit::record_activity_continuity(
                "export",
                &document.lineage.id,
                document.schema_version,
                document.references.len(),
                None,
                "exported",
                client,
            );
            encode(document)
        }
        Err(error) => Err(service_error(error)),
    }
}

pub fn import(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let owner_uid = owner(client)?;
    let request: body::ActivityContinuityImport = decode(params)?;
    let document = ActivityContinuityDocument::from_json(request.document.as_str().as_bytes())
        .map_err(service_error)?;
    let result = activities::open_default()
        .map_err(service_error)?
        .import_continuity(owner_uid, request.placement, document.clone());
    match result {
        Ok(imported) => {
            super::audit::record_activity_continuity(
                "import",
                &document.lineage.id,
                document.schema_version,
                document.references.len(),
                Some(request.placement.as_str()),
                "imported",
                client,
            );
            encode(imported)
        }
        Err(error) => {
            super::audit::record_activity_continuity(
                "import",
                &document.lineage.id,
                document.schema_version,
                document.references.len(),
                Some(request.placement.as_str()),
                "rejected",
                client,
            );
            Err(service_error(error))
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/activity_continuity.rs"
    ));
}
