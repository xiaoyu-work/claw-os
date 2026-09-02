//! Authenticated manifest for the Agent extension ABI.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::caps::{Cap, ScopeKind};
use crate::provenance::{package_digest, SignedFile, VerifiedPackage};

pub const MANIFEST_FILE: &str = "extension.json";
pub const ABI_VERSION: u32 = 1;
pub const FEATURE_OBSERVATIONAL_EVENTS: &str = "observational-events";
pub const FEATURE_PROPOSED_ACTIONS: &str = "proposed-actions";
pub const SUPPORTED_FEATURES: &[&str] = &[FEATURE_OBSERVATIONAL_EVENTS, FEATURE_PROPOSED_ACTIONS];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionManifest {
    pub schema_version: u32,
    pub identity: ExtensionIdentity,
    pub entry: String,
    pub protocol: ProtocolRequirement,
    pub subscriptions: Vec<EventKind>,
    #[serde(default)]
    pub requested_capabilities: Vec<Cap>,
    #[serde(default)]
    pub limits: ExtensionLimits,
    #[serde(flatten)]
    pub additive: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionIdentity {
    pub id: String,
    pub version: String,
    pub content_digest: String,
    #[serde(flatten)]
    pub additive: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolRequirement {
    pub min_version: u32,
    pub max_version: u32,
    #[serde(default)]
    pub required_features: Vec<String>,
    #[serde(flatten)]
    pub additive: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventKind {
    SessionStart,
    PreModelCall,
    PostModelCall,
    PreTool,
    PostTool,
    Completion,
}

impl EventKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionStart => "session-start",
            Self::PreModelCall => "pre-model-call",
            Self::PostModelCall => "post-model-call",
            Self::PreTool => "pre-tool",
            Self::PostTool => "post-tool",
            Self::Completion => "completion",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionLimits {
    #[serde(default = "default_event_timeout_ms")]
    pub event_timeout_ms: u64,
    #[serde(default = "default_queue_capacity")]
    pub queue_capacity: usize,
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: usize,
    #[serde(default = "default_max_actions_per_event")]
    pub max_actions_per_event: usize,
    #[serde(default = "default_max_in_flight")]
    pub max_in_flight: usize,
    #[serde(flatten)]
    pub additive: BTreeMap<String, Value>,
}

impl Default for ExtensionLimits {
    fn default() -> Self {
        Self {
            event_timeout_ms: default_event_timeout_ms(),
            queue_capacity: default_queue_capacity(),
            max_output_bytes: default_max_output_bytes(),
            max_actions_per_event: default_max_actions_per_event(),
            max_in_flight: default_max_in_flight(),
            additive: BTreeMap::new(),
        }
    }
}

const fn default_event_timeout_ms() -> u64 {
    1000
}

const fn default_queue_capacity() -> usize {
    8
}

const fn default_max_output_bytes() -> usize {
    4096
}

const fn default_max_actions_per_event() -> usize {
    2
}

const fn default_max_in_flight() -> usize {
    1
}

impl ExtensionManifest {
    pub fn parse_verified(package: &VerifiedPackage) -> Result<Self, String> {
        let bytes = package
            .file_bytes(MANIFEST_FILE)
            .ok_or_else(|| format!("verified package omitted {MANIFEST_FILE}"))?;
        if bytes.is_empty() || bytes.len() > 64 * 1024 {
            return Err("extension manifest is empty or oversized".to_string());
        }
        let manifest: Self = serde_json::from_slice(bytes)
            .map_err(|error| format!("parse extension manifest: {error}"))?;
        manifest.validate(package)?;
        Ok(manifest)
    }

    pub fn validate(&self, package: &VerifiedPackage) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err(format!(
                "unsupported extension manifest schema {}",
                self.schema_version
            ));
        }
        validate_id(&self.identity.id)?;
        if self.identity.id != package.id() {
            return Err("extension manifest identity does not match package identity".to_string());
        }
        let version = semver::Version::parse(&self.identity.version)
            .map_err(|_| "extension version is not semver".to_string())?;
        if version.to_string() != self.identity.version
            || self.identity.version != package.version()
            || self.identity.version.len() > 64
        {
            return Err("extension manifest version does not match package version".to_string());
        }
        validate_digest(&self.identity.content_digest)?;
        if self.identity.content_digest != content_digest(package) {
            return Err(
                "extension content digest does not match verified package files".to_string(),
            );
        }
        validate_entry(&self.entry)?;
        if package.file_bytes(&self.entry).is_none() || !package.file_is_executable(&self.entry) {
            return Err(
                "extension entry is missing or not executable in the verified package".to_string(),
            );
        }
        if self.protocol.min_version == 0
            || self.protocol.max_version < self.protocol.min_version
            || self.protocol.min_version > ABI_VERSION
            || self.protocol.max_version < ABI_VERSION
            || self.protocol.max_version > 64
        {
            return Err(format!(
                "extension protocol range {}..={} is incompatible with ABI v{}",
                self.protocol.min_version, self.protocol.max_version, ABI_VERSION
            ));
        }
        let mut features = BTreeSet::new();
        for feature in &self.protocol.required_features {
            if feature.is_empty()
                || feature.len() > 64
                || !SUPPORTED_FEATURES.contains(&feature.as_str())
                || !features.insert(feature)
            {
                return Err(format!(
                    "extension requires unsupported or duplicate feature `{feature}`"
                ));
            }
        }
        if self.subscriptions.is_empty() || self.subscriptions.len() > 6 {
            return Err("extension must declare one to six event subscriptions".to_string());
        }
        let subscriptions = self.subscriptions.iter().copied().collect::<BTreeSet<_>>();
        if subscriptions.len() != self.subscriptions.len() {
            return Err("extension event subscriptions contain duplicates".to_string());
        }
        if self.requested_capabilities.len() > 16 {
            return Err("extension requested too many capabilities".to_string());
        }
        let mut caps = BTreeSet::new();
        for cap in &self.requested_capabilities {
            let metadata = crate::caps::catalog::lookup(cap.verb)
                .ok_or_else(|| format!("extension requested unknown verb `{}`", cap.verb))?;
            let kind = cap.scope.kind();
            let valid_kind = match metadata.scope_kind {
                ScopeKind::None | ScopeKind::Wild => kind == ScopeKind::Wild,
                expected => kind == expected,
            };
            if !valid_kind || !caps.insert(format!("{}:{}", cap.verb, cap.scope)) {
                return Err(format!(
                    "extension capability `{}` has an invalid or duplicate scope",
                    cap.verb
                ));
            }
        }
        if !(50..=5000).contains(&self.limits.event_timeout_ms)
            || !(1..=32).contains(&self.limits.queue_capacity)
            || !(1..=8192).contains(&self.limits.max_output_bytes)
            || self.limits.max_actions_per_event > 4
            || self.limits.max_in_flight != 1
        {
            return Err("extension limits are outside the ABI bounds".to_string());
        }
        Ok(())
    }

    pub fn manifest_digest(package: &VerifiedPackage) -> Result<String, String> {
        package
            .file_bytes(MANIFEST_FILE)
            .map(crate::crypto::sha256_hex)
            .ok_or_else(|| format!("verified package omitted {MANIFEST_FILE}"))
    }
}

pub fn content_digest(package: &VerifiedPackage) -> String {
    let files = package
        .signed_files()
        .iter()
        .filter(|file| file.path != MANIFEST_FILE)
        .cloned()
        .collect::<Vec<SignedFile>>();
    package_digest(&files)
}

fn validate_id(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
    {
        return Err("extension id is invalid".to_string());
    }
    Ok(())
}

fn validate_entry(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 240
        || value.starts_with('/')
        || value.split('/').any(|part| {
            part.is_empty()
                || part == "."
                || part == ".."
                || part.starts_with('.')
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
    {
        return Err("extension entry path is invalid".to_string());
    }
    Ok(())
}

fn validate_digest(value: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err("extension content digest is invalid".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent_extensions/manifest.rs"
    ));
}
