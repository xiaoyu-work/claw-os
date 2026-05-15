//! Bridge to `apps/net` — HTTP fetch / download routed through the
//! kernel so allow-lists and the `net.outbound` capability apply.

use serde::Deserialize;

use super::{call_typed, BridgeError};

/// Response from `apps/net fetch`.
#[derive(Debug, Clone, Deserialize)]
pub struct FetchResult {
    pub url: String,
    pub status: u16,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub headers: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Response from `apps/net download`.
#[derive(Debug, Clone, Deserialize)]
pub struct DownloadResult {
    pub url: String,
    pub path: String,
    pub bytes: u64,
}

/// HTTP GET into memory.
pub fn fetch(url: &str) -> Result<FetchResult, BridgeError> {
    call_typed("net", "fetch", [url], None)
}

/// HTTP GET into a file at `dest`.
pub fn download(url: &str, dest: &str) -> Result<DownloadResult, BridgeError> {
    call_typed("net", "download", [url, dest], None)
}
