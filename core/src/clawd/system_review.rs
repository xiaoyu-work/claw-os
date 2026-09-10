//! Owner-bound system review requests. Only the privileged OS helper decides.

use std::path::Path;

use serde_json::{json, Value};

use crate::approvals::system_review::{self as store, ReviewKind, ReviewRecord};
use crate::provenance::runtime::PackageRef;
use crate::provenance::{PackageKind, TrustStore, VerifiedPackage, VerifyOptions};

use super::client_identity::ClientIdentity;
use super::protocol::BrokerError;

fn invalid_input(message: impl Into<String>) -> BrokerError {
    BrokerError::execution(message).classified("invalid_system_review_request")
}

fn expected_package(params: &Value) -> Result<PackageRef, String> {
    serde_json::from_value(
        params
            .get("expected_package")
            .cloned()
            .ok_or("expected_package is required")?,
    )
    .map_err(|error| format!("invalid expected App package: {error}"))
}

#[cfg(target_os = "linux")]
fn verify_source(owner: u32, source: &Path) -> Result<VerifiedPackage, String> {
    if !source.is_absolute() {
        return Err("App review source must be an absolute path".into());
    }
    let trust = TrustStore::load_roots(&TrustStore::roots_for_owner(owner));
    let _identity = super::client_identity::FsIdentityGuard::enter(owner)?;
    let source = source
        .canonicalize()
        .map_err(|error| format!("resolve App review source as its owner: {error}"))?;
    crate::provenance::verify::verify_package(
        &source,
        &VerifyOptions::new(PackageKind::App),
        &trust,
    )
    .map_err(|error| format!("verify App review source: {error}"))
}

#[cfg(not(target_os = "linux"))]
fn verify_source(_owner: u32, _source: &Path) -> Result<VerifiedPackage, String> {
    Err("protected App review requires Linux owner filesystem isolation".into())
}

fn require_expected(package: &VerifiedPackage, expected: &PackageRef) -> Result<(), String> {
    if PackageRef::of(package) != *expected {
        return Err("App package differs from the installer's verified snapshot".into());
    }
    Ok(())
}

fn view(record: &ReviewRecord) -> Result<Value, String> {
    Ok(json!({
        "id": record.id,
        "kind": record.kind,
        "state": record.effective_state()?,
        "owner_uid": record.owner_uid,
        "app_id": record.manifest.id,
        "app_version": record.manifest.version,
        "name": record.manifest.name,
        "package": record.package,
        "requester": record.requester,
        "requested_at": record.requested_at,
        "expires_at": record.expires_at,
        "decided_at": record.decided_at,
        "decided_by": record.decided_by,
        "inherited_from": record.inherited_from,
        "permission_review": record.permission_review()?,
        "permissions_granted": false,
    }))
}

pub(crate) fn require_app_review(
    owner: u32,
    app: &crate::apps::App,
    requester: &str,
) -> Result<(), BrokerError> {
    let package = app.require_verified().map_err(BrokerError::authorization)?;
    if store::has_accepted(owner, package).map_err(BrokerError::unavailable)? {
        return Ok(());
    }
    let record = store::submit(
        owner,
        ReviewKind::AppActivation,
        package,
        requester.to_string(),
    )
    .map_err(BrokerError::unavailable)?;
    if record.effective_state().map_err(BrokerError::unavailable)? == store::ReviewState::Approved {
        store::consume(owner, &record.id, package).map_err(BrokerError::authorization)?;
        return Ok(());
    }
    Err(BrokerError::authorization_required(
        "App activation is waiting for the owner's OS permission review",
        json!({
            "status": "system_review_required",
            "system_review_id": record.id,
            "app_id": app.manifest.id,
        }),
    )
    .classified("system_review_required"))
}

pub async fn prepare(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    super::app_sessions::require_non_app_caller(client)
        .await
        .map_err(BrokerError::authorization)?;
    let owner = client.require_uid().map_err(BrokerError::authorization)?;
    let source = super::permissions::required_string(&params, "source").map_err(invalid_input)?;
    let expected = expected_package(&params).map_err(invalid_input)?;
    let pid = client
        .pid
        .ok_or_else(|| BrokerError::authorization("review requester pid is unavailable"))?;
    let requester = format!("uid:{owner} pid:{pid}");
    tokio::task::spawn_blocking(move || {
        let package = verify_source(owner, Path::new(&source))?;
        require_expected(&package, &expected)?;
        let record = store::submit(owner, ReviewKind::AppInstall, &package, requester)?;
        view(&record)
    })
    .await
    .map_err(|error| BrokerError::unavailable(format!("App review preparation failed: {error}")))?
    .map_err(BrokerError::authorization)
}

pub async fn pending(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(100);
    if limit == 0 || limit > 100 {
        return Err(invalid_input("system review limit must be 1..100"));
    }
    super::app_sessions::require_non_app_caller(client)
        .await
        .map_err(BrokerError::authorization)?;
    let owner = client.require_uid().map_err(BrokerError::authorization)?;
    tokio::task::spawn_blocking(move || {
        let reviews = store::pending(owner, limit as usize)?
            .iter()
            .map(view)
            .collect::<Result<Vec<_>, String>>()?;
        Ok::<_, String>(json!({"reviews": reviews}))
    })
    .await
    .map_err(|error| BrokerError::unavailable(format!("read pending system reviews: {error}")))?
    .map_err(BrokerError::unavailable)
}

pub async fn show(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    super::app_sessions::require_non_app_caller(client)
        .await
        .map_err(BrokerError::authorization)?;
    let owner = client.require_uid().map_err(BrokerError::authorization)?;
    let id = super::permissions::required_string(&params, "id").map_err(invalid_input)?;
    tokio::task::spawn_blocking(move || view(&store::get(owner, &id)?))
        .await
        .map_err(|error| BrokerError::unavailable(format!("read system review: {error}")))?
        .map_err(BrokerError::authorization)
}

pub async fn decide(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    if client.require_uid().map_err(BrokerError::authorization)? != 0 {
        return Err(BrokerError::authorization(
            "system review decisions require the privileged OS approval helper",
        ));
    }
    let owner = params
        .get("owner_uid")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| invalid_input("owner_uid is required"))?;
    let id = super::permissions::required_string(&params, "id").map_err(invalid_input)?;
    let decision =
        super::permissions::required_string(&params, "decision").map_err(invalid_input)?;
    let approve = match decision.as_str() {
        "approve" => true,
        "deny" => false,
        _ => {
            return Err(invalid_input(
                "system review decision must be approve or deny",
            ))
        }
    };
    tokio::task::spawn_blocking(move || {
        let current = store::get(owner, &id)?;
        let verified = if approve {
            match verify_source(owner, &current.source_dir) {
                Ok(package) => {
                    if PackageRef::of(&package) != current.package {
                        store::invalidate(owner, &id)?;
                        return Err("App package changed; refresh the system review".into());
                    }
                    Some(package)
                }
                Err(error) => {
                    store::invalidate(owner, &id)?;
                    return Err(error);
                }
            }
        } else {
            None
        };
        let record = store::decide(owner, &id, approve, verified.as_ref())?;
        view(&record)
    })
    .await
    .map_err(|error| BrokerError::unavailable(format!("system review decision failed: {error}")))?
    .map_err(BrokerError::authorization)
}

pub async fn consume(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    super::app_sessions::require_non_app_caller(client)
        .await
        .map_err(BrokerError::authorization)?;
    let owner = client.require_uid().map_err(BrokerError::authorization)?;
    let id = super::permissions::required_string(&params, "id").map_err(invalid_input)?;
    let source = super::permissions::required_string(&params, "source").map_err(invalid_input)?;
    let expected = expected_package(&params).map_err(invalid_input)?;
    tokio::task::spawn_blocking(move || {
        let package = verify_source(owner, Path::new(&source))?;
        require_expected(&package, &expected)?;
        view(&store::consume(owner, &id, &package)?)
    })
    .await
    .map_err(|error| BrokerError::unavailable(format!("consume system review: {error}")))?
    .map_err(BrokerError::authorization)
}

pub async fn cancel(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    super::app_sessions::require_non_app_caller(client)
        .await
        .map_err(BrokerError::authorization)?;
    let owner = client.require_uid().map_err(BrokerError::authorization)?;
    let id = super::permissions::required_string(&params, "id").map_err(invalid_input)?;
    tokio::task::spawn_blocking(move || view(&store::decide(owner, &id, false, None)?))
        .await
        .map_err(|error| BrokerError::unavailable(format!("cancel system review: {error}")))?
        .map_err(BrokerError::authorization)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/system_review.rs"
    ));
}
