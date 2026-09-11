//! Authenticated GUI launch custody; display transport is not a permission grant.

#[cfg(target_os = "linux")]
mod broker;
pub mod inputs;
#[cfg(target_os = "linux")]
mod manager;
#[cfg(target_os = "linux")]
mod output;
#[cfg(target_os = "linux")]
mod proxy;
#[cfg(target_os = "linux")]
pub(crate) mod retirement;
#[cfg(target_os = "linux")]
mod supervision;
#[cfg(target_os = "linux")]
mod transport;
#[cfg(target_os = "linux")]
pub(crate) use manager::*;

#[cfg(not(target_os = "linux"))]
pub(crate) async fn launch(
    _params: serde_json::Value, _client: &crate::clawd::client_identity::ClientIdentity,
) -> Result<serde_json::Value, crate::clawd::protocol::BrokerError> {
    Err(crate::clawd::protocol::BrokerError::unavailable("authenticated GUI launch requires Linux"))
}

#[cfg(not(target_os = "linux"))]
pub(crate) use launch as wait;
#[cfg(not(target_os = "linux"))]
pub(crate) use launch as stop;
