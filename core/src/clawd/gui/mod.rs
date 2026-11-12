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
mod selected_read;
#[cfg(target_os = "linux")]
pub(crate) use selected_read::{
    file_read_status, request_file_read, request_file_write, file_write_status, reserve_file_save,
    request_file_create, file_create_status, reserve_file_create,
};
#[cfg(target_os = "linux")]
pub(crate) use supervision::validate_optional_mounts;
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

#[cfg(not(target_os = "linux"))]
pub(crate) async fn request_file_read(
    _params: serde_json::Value, _client: &crate::clawd::client_identity::ClientIdentity,
    _authority: &crate::clawd::authority::Decision,
) -> Result<serde_json::Value, crate::clawd::protocol::BrokerError> {
    Err(crate::clawd::protocol::BrokerError::unavailable("selected native file read requires Linux"))
}

#[cfg(not(target_os = "linux"))]
pub(crate) use request_file_read as file_read_status;
#[cfg(not(target_os = "linux"))]
pub(crate) use request_file_read as request_file_write;
#[cfg(not(target_os = "linux"))]
pub(crate) use request_file_read as file_write_status;
#[cfg(not(target_os = "linux"))]
pub(crate) use request_file_read as reserve_file_save;
#[cfg(not(target_os = "linux"))]
pub(crate) use request_file_read as request_file_create;
#[cfg(not(target_os = "linux"))]
pub(crate) use request_file_read as file_create_status;
#[cfg(not(target_os = "linux"))]
pub(crate) use request_file_read as reserve_file_create;

#[cfg(not(target_os = "linux"))]
pub(crate) fn validate_optional_mounts(
    _app: &crate::apps::App, _entry: Option<&str>, _home: &std::path::Path, caps: &crate::caps::CapSet,
) -> Result<(), String> {
    crate::worker::derive::granted_path_mounts(caps).map(|_| ())
}
