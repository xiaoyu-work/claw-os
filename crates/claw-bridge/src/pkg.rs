//! Bridge to `apps/pkg` — apt wrapper that records every install in
//! the audit log and respects `pkg.install` capabilities.

use serde::Deserialize;

use super::{call, call_typed, BridgeError};

/// Response from `apps/pkg has`.
#[derive(Debug, Clone, Deserialize)]
pub struct HasResult {
    pub package: String,
    pub installed: bool,
    #[serde(default)]
    pub version: Option<String>,
}

/// One row from `apps/pkg list`.
#[derive(Debug, Clone, Deserialize)]
pub struct ListedPackage {
    pub package: String,
    #[serde(default)]
    pub version: Option<String>,
}

/// Response from `apps/pkg list`.
#[derive(Debug, Clone, Deserialize)]
pub struct ListResult {
    pub packages: Vec<ListedPackage>,
}

/// Ensure `packages` are installed (no-op if already present).
pub fn need(packages: &[&str]) -> Result<serde_json::Value, BridgeError> {
    call("pkg", "need", packages.iter().copied(), None)
}

/// Check whether `package` is installed.
pub fn has(package: &str) -> Result<HasResult, BridgeError> {
    call_typed("pkg", "has", [package], None)
}

/// List currently installed packages.
pub fn list() -> Result<ListResult, BridgeError> {
    call_typed("pkg", "list", std::iter::empty::<&str>(), None)
}
