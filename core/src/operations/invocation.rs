//! Requested App operations and their broker-retained canonical preparation.
//! Neither value grants execution authority.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppInvocation {
    pub app_id: String,
    pub operation: String,
    pub args: Vec<String>,
    pub package_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedInvocation {
    pub id: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSessionInvocation {
    pub app_id: String,
    pub package_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedSessionCall {
    pub id: String,
    pub args: BTreeMap<String, Value>,
}
