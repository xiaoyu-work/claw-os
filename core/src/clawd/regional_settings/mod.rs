//! Capability-gated regional settings, without App identity or polkit exemptions.

mod dbus;
mod request;

pub use request::Request;

use std::sync::OnceLock;
use std::time::Duration;

use serde_json::Value;

use super::authority::{Audience, Decision};
use super::client_identity::ClientIdentity;
use super::protocol::{BrokerError, BrokerErrorKind};

static MUTATION: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

pub fn cli_request(args: &[String]) -> Result<Value, BrokerError> {
    if args.len() != 1 || args[0].len() > 8192 {
        return Err(BrokerError::execution(
            "regional settings require one JSON request of at most 8192 bytes",
        ));
    }
    let mut params: Value = serde_json::from_str(&args[0]).map_err(|error| {
        BrokerError::execution(format!("invalid regional settings JSON: {error}"))
    })?;
    let fields = params
        .as_object_mut()
        .ok_or_else(|| BrokerError::execution("regional settings request must be an object"))?;
    if fields.contains_key("session") {
        return Err(BrokerError::execution(
            "regional settings do not accept a caller-supplied session",
        ));
    }
    let session = std::env::var("COS_SESSION")
        .map_err(|_| BrokerError::authorization("regional settings require an authenticated COS_SESSION; the GUI launcher must provide an authorized session"))?;
    fields.insert("session".into(), Value::String(session));
    serde_json::to_value(Request::decode(params).map_err(BrokerError::execution)?)
        .map_err(|error| BrokerError::execution(error.to_string()))
}

pub async fn control(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, BrokerError> {
    let (request, owner) = prepare(params, client, authority)?;
    let result = apply(&request, owner, authority).await;
    complete_attempt(&request, authority, result)
}

async fn apply(request: &Request, owner: u32, authority: &Decision) -> Result<Value, BrokerError> {
    if unsafe { libc::geteuid() } != 0 {
        return Err(BrokerError::unavailable(
            "regional settings require root clawd",
        ));
    }
    let _guard = tokio::time::timeout(
        Duration::from_secs(5),
        MUTATION.get_or_init(|| tokio::sync::Mutex::new(())).lock(),
    )
    .await
    .map_err(|_| {
        BrokerError::unavailable("regional settings are busy; no change was dispatched")
    })?;
    authority
        .ensure_current()
        .map_err(BrokerError::authorization)?;
    let backend = dbus::Backend::system().await?;
    backend.apply(request, owner, authority).await
}

fn complete_attempt(
    request: &Request,
    authority: &Decision,
    result: Result<Value, BrokerError>,
) -> Result<Value, BrokerError> {
    if matches!(&result, Err(error) if error.kind == BrokerErrorKind::Unavailable)
        && !super::authority::obligation_met(Some(authority))
    {
        // Failed discovery still owes the broker an audited request spend.
        // Delay it until failure so successful discovery retains a last-use grant.
        let _authorized = authority
            .require(request.required_cap())
            .map_err(BrokerError::authorization)?;
    }
    result
}

fn prepare(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<(Request, u32), BrokerError> {
    let request = Request::decode(params).map_err(BrokerError::execution)?;
    let owner = preflight(&request, client, authority)?;
    Ok((request, owner))
}

fn preflight(
    request: &Request,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<u32, BrokerError> {
    let owner = client.require_uid().map_err(BrokerError::authorization)?;
    if authority.owner_uid() != owner
        || authority.session_id() != Some(request.session())
        || authority.audience() != Audience::SystemService
    {
        return Err(BrokerError::authorization(
            "regional settings owner/session mismatch",
        ));
    }
    authority
        .ensure_current()
        .map_err(BrokerError::authorization)?;
    if !authority.caps().covers(&request.required_cap()) {
        return Err(BrokerError::authorization(
            "regional settings require the exact declared capability",
        ));
    }
    // Only a preflight refusal check. The backend spends live authority directly
    // before mutation, so a last-use grant is not retired during discovery.
    Ok(owner)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/regional_settings/mod.rs"
    ));
}
