//! Owner desktop preferences, without exposing the owner's configuration tree.

mod files;
mod request;

pub use request::Request;
use request::{Action, Entry, Namespace};

use serde_json::{json, Value};

use super::authority::{Audience, Authorized, Decision};
use super::client_identity::ClientIdentity;
use super::protocol::BrokerError;

trait PreferenceStore {
    fn read(&self, namespace: Namespace, proof: &Authorized) -> Result<Vec<Entry>, BrokerError>;
    fn write(
        &self,
        namespace: Namespace,
        entries: &[Entry],
        proof: &Authorized,
    ) -> Result<(), BrokerError>;
}

pub fn cli_request(args: &[String]) -> Result<Value, BrokerError> {
    if args.len() != 1 || args[0].len() > request::MAX_REQUEST {
        return Err(BrokerError::execution(
            "desktop preferences require one bounded JSON request",
        ));
    }
    let mut value: Value = serde_json::from_str(&args[0])
        .map_err(|error| BrokerError::execution(format!("invalid preferences JSON: {error}")))?;
    let body = value
        .as_object_mut()
        .ok_or_else(|| BrokerError::execution("desktop preferences require an object"))?;
    if body.contains_key("session") {
        return Err(BrokerError::execution(
            "desktop preferences do not accept a caller-supplied session",
        ));
    }
    body.insert(
        "session".into(),
        Value::String(std::env::var("COS_SESSION").map_err(|_| {
            BrokerError::authorization("desktop preferences require an authenticated COS_SESSION")
        })?),
    );
    serde_json::to_value(Request::decode(value).map_err(BrokerError::execution)?)
        .map_err(|error| BrokerError::execution(error.to_string()))
}

pub async fn control(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, BrokerError> {
    let request = Request::decode(params).map_err(BrokerError::execution)?;
    let owner = client.require_uid().map_err(BrokerError::authorization)?;
    if authority.owner_uid() != owner
        || authority.session_id() != Some(request.session.as_str())
        || authority.audience() != Audience::SystemService
    {
        return Err(BrokerError::authorization(
            "desktop preferences owner/session mismatch",
        ));
    }
    authority
        .ensure_current()
        .map_err(BrokerError::authorization)?;
    let proof = authority
        .require_all(&request.required_caps())
        .map_err(BrokerError::authorization)?;
    let home = client
        .require_home_dir()
        .map_err(BrokerError::authorization)?;
    apply(&request, &files::OwnerFiles::new(owner, home), &proof)
}

fn apply(
    request: &Request,
    store: &impl PreferenceStore,
    proof: &Authorized,
) -> Result<Value, BrokerError> {
    match &request.action {
        Action::Read => Ok(json!({
            "action":"read",
            "namespace":request.namespace,
            "version":1,
            "entries":store.read(request.namespace, proof)?,
        })),
        Action::Write { entries } => {
            store.write(request.namespace, entries, proof)?;
            Ok(json!({
                "action":"write",
                "namespace":request.namespace,
                "version":1,
                "status":"applied",
                "entries":entries,
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/desktop_preferences/mod.rs"
    ));
}
