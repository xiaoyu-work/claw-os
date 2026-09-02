use super::*;

/// Validated credential namespace. Values are safe to use as one path segment
/// and as one capability-scope component.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct NamespaceId(String);

impl NamespaceId {
    pub(super) fn parse(value: &str) -> CredentialResult<Self> {
        validate_component("namespace", value)?;
        Ok(Self(value.to_string()))
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

/// Validated namespace/name pair used across policy, storage, and OAuth.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CredentialId {
    namespace: NamespaceId,
    name: String,
}

impl CredentialId {
    pub(super) fn parse(namespace: &str, name: &str) -> CredentialResult<Self> {
        let namespace = NamespaceId::parse(namespace)?;
        validate_component("credential name", name)?;
        Ok(Self {
            namespace,
            name: name.to_string(),
        })
    }

    pub(super) fn namespace(&self) -> &str {
        self.namespace.as_str()
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }
}

fn validate_component(kind: &str, value: &str) -> CredentialResult<()> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(CredentialError::invalid(
            "credential.validate",
            format!("{kind} must be alphanumeric (hyphens/underscores allowed)"),
        ));
    }
    Ok(())
}

pub(super) struct StoreRequest<'a> {
    pub(super) id: &'a CredentialId,
    pub(super) value: &'a str,
    pub(super) min_tier: u8,
    pub(super) ttl: Option<u64>,
    pub(super) refresh_cmd: Option<String>,
}

pub(super) struct StoreResult {
    pub(super) stored_at: String,
    pub(super) expires_at: Option<String>,
}

pub(super) struct CredentialMetadata {
    pub(super) name: String,
    pub(super) min_tier: u8,
    pub(super) stored_at: String,
    pub(super) stored_by: Option<String>,
    pub(super) expires_at: Option<String>,
    pub(super) refresh_cmd: Option<String>,
    pub(super) expired: bool,
}

pub(super) struct NamespaceSummary {
    pub(super) namespace: String,
    pub(super) count: usize,
}

pub(super) struct LoadedCredential {
    pub(super) name: String,
    pub(super) namespace: String,
    pub(super) min_tier: u8,
    pub(super) value: String,
    pub(super) refreshed: Option<bool>,
    pub(super) expires_at: Option<String>,
}

pub(super) struct LoadedBundle {
    pub(super) credentials: std::collections::BTreeMap<String, String>,
    pub(super) errors: std::collections::BTreeMap<String, String>,
}

/// Secret-store boundary consumed by OAuth. Implementations own persistence,
/// encryption, locking, and key material; OAuth sees only typed operations.
pub(super) trait CredentialStore {
    fn contains(&self, id: &CredentialId) -> CredentialResult<bool>;
    /// Load a value after the caller has handled capability authorization.
    ///
    /// `enforce_tier` controls record-tier enforcement only; it is not a
    /// substitute for `secret.read`. Broker-only callers may disable the tier
    /// check only after their process boundary has authorized the operation.
    fn load(&self, id: &CredentialId, enforce_tier: bool) -> CredentialResult<String>;
    fn minimum_tier(&self, id: &CredentialId) -> CredentialResult<Option<u8>>;
    fn store(&self, request: StoreRequest<'_>) -> CredentialResult<StoreResult>;
}

// ===========================================================================
// Credential and bundle data structures
// ===========================================================================

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct StoredCredential {
    pub(super) name: String,
    /// Namespace this credential belongs to.
    pub(super) namespace: String,
    /// Base64-encoded encrypted value (AES-256-GCM ciphertext + tag, or legacy
    /// XOR-obfuscated bytes).
    pub(super) value_b64: String,
    /// Base64-encoded 12-byte nonce.  `None` indicates a legacy XOR credential.
    #[serde(default)]
    pub(super) nonce_b64: Option<String>,
    /// Minimum tier required to load this credential (0 = ROOT only, 1 = OPERATE+, etc.)
    pub(super) min_tier: u8,
    pub(super) stored_at: String,
    pub(super) stored_by: Option<String>,
    /// ISO 8601 expiry timestamp.  `None` means the credential never expires.
    #[serde(default)]
    pub(super) expires_at: Option<String>,
    /// Command to execute when credential expires (auto-refresh).
    /// The command should output a new value to stdout.
    #[serde(default)]
    pub(super) refresh_cmd: Option<String>,
}

// Manual Debug: never include the encrypted blob or nonce so accidental
// `tracing::debug!(?cred)` / `dbg!(&cred)` calls cannot regress into leaking
// ciphertext or correlatable metadata into logs. The encrypted value would
// only be useful if the operator also leaked the root key, but defense in
// depth is cheap and the audit explicitly called this out.
impl std::fmt::Debug for StoredCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredCredential")
            .field("name", &self.name)
            .field("namespace", &self.namespace)
            .field("value_b64", &"***")
            .field("nonce_b64", &self.nonce_b64.as_ref().map(|_| "***"))
            .field("min_tier", &self.min_tier)
            .field("stored_at", &self.stored_at)
            .field("stored_by", &self.stored_by)
            .field("expires_at", &self.expires_at)
            .field("refresh_cmd", &self.refresh_cmd)
            .finish()
    }
}

impl std::fmt::Display for StoredCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "credential({}/{})", self.namespace, self.name)
    }
}

/// A bundle manifest — a named group of credential keys.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct BundleManifest {
    pub(super) name: String,
    pub(super) namespace: String,
    pub(super) keys: Vec<String>,
    pub(super) created_at: String,
}
