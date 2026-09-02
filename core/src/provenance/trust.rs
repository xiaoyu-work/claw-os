//! Publisher trust roots, key revocation/rotation and the segregated
//! developer trust store.
//!
//! ## Where trust can come from
//!
//! | Tier | Root | Required owner |
//! | --- | --- | --- |
//! | [`TrustTier::Vendor`] | `/usr/lib/cos/trust/publishers.d` | `root` |
//! | [`TrustTier::System`] | `/etc/cos/trust/publishers.d` | `root` |
//! | [`TrustTier::User`] | `~/.config/cos/trust/publishers.d` | the owner |
//! | [`TrustTier::Developer`] | `~/.config/cos/trust/developer.d` | the owner |
//!
//! Every root — and every ancestor directory up to `/` — must be a
//! non-symlink directory owned by the required uid and free of
//! group/world write bits. A root that fails those checks contributes
//! **nothing**; it is reported as a diagnostic instead of being
//! silently skipped.
//!
//! ## What cannot add trust
//!
//! There is deliberately no environment variable that appends a trust
//! root, relaxes the ownership checks, or disables verification. The
//! per-user root is resolved from the passwd entry of the effective
//! uid, not from `HOME`/`COS_USER_CONFIG_DIR`, so redirecting the
//! process environment cannot point trust at attacker-controlled
//! files. Tests and embedders build a store explicitly with
//! [`TrustStore::load_roots`]; that is a compiled-in call, not ambient
//! configuration, and the model has no route to it.
//!
//! ## Key ids
//!
//! A key id is `sha256:<hex>` over the raw verifying key
//! ([`super::envelope::key_id_for`]). Trust entries whose declared id
//! does not match their key material are rejected, so an operator
//! cannot alias one publisher's id onto another publisher's key and
//! no two distinct keys can occupy the same id.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::envelope::{is_lower_hex, is_sha256_ref, key_id_for, PackageKind, ALG_ED25519};
use super::fsec::{self, PathTrustError};
use super::state::{self, TrustDomain, TrustState, TrustWatch};

/// Schema string for a publisher trust file.
pub const TRUST_SCHEMA_V1: &str = "claw.trust/v1";
/// Schema string for the developer (unsigned-package) trust file.
pub const DEV_TRUST_SCHEMA_V1: &str = "claw.trust-dev/v1";

/// The only usage constraint that authorises package signing. A key
/// minted for release-artifact signing or TLS must not be able to
/// authorise extension code by accident.
pub const USAGE_PACKAGE_SIGNING: &str = "package-signing";

/// Root-owned vendor trust root shipped by the Debian/rootfs package.
pub const VENDOR_TRUST_ROOT: &str = "/usr/lib/cos/trust/publishers.d";
/// Root-owned operator trust root.
pub const SYSTEM_TRUST_ROOT: &str = "/etc/cos/trust/publishers.d";
/// Per-user publisher trust root, relative to the owner's home.
pub const USER_TRUST_SUBDIR: &str = ".config/cos/trust/publishers.d";
/// Per-user developer trust root, relative to the owner's home.
pub const DEV_TRUST_SUBDIR: &str = ".config/cos/trust/developer.d";

const MAX_TRUST_FILE_BYTES: u64 = 1024 * 1024;
const MAX_TRUST_FILES: usize = 256;

/// Where a key or grant came from. Ordered weakest-last so a caller
/// can compare tiers when deciding what a package may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustTier {
    Vendor,
    System,
    User,
    Developer,
}

impl TrustTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Vendor => "vendor",
            Self::System => "system",
            Self::User => "user",
            Self::Developer => "developer",
        }
    }

    /// Developer-trusted content is unsigned by definition and must
    /// never reach privileged routes or wildcard capability scopes.
    pub fn allows_privileged_routes(self) -> bool {
        !matches!(self, Self::Developer)
    }
}

/// One trusted publisher key after validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedKey {
    pub key_id: String,
    pub public_key: [u8; 32],
    pub usages: BTreeSet<String>,
    pub kinds: BTreeSet<PackageKind>,
    pub tier: TrustTier,
    pub source: PathBuf,
    pub comment: Option<String>,
    /// Validity window, parsed and normalised to UTC. Rotation is
    /// "publish the successor, then bound the predecessor".
    pub validity: Validity,
}

/// A key's validity window.
///
/// Both bounds are parsed as strict RFC 3339 timestamps and normalised
/// to UTC, so `2026-01-01T00:00:00+01:00` and `2025-12-31T23:00:00Z`
/// compare equal. A malformed, ambiguous or out-of-range value rejects
/// the whole trust entry rather than being ignored: a key whose expiry
/// cannot be understood must not authorise anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Validity {
    pub not_before: Option<chrono::DateTime<chrono::Utc>>,
    pub not_after: Option<chrono::DateTime<chrono::Utc>>,
}

impl Validity {
    /// Parse and validate a window. Ordering is checked so a window
    /// that can never be satisfied is refused at load time.
    pub fn parse(not_before: Option<&str>, not_after: Option<&str>) -> Result<Self, String> {
        let before = not_before.map(parse_rfc3339_utc).transpose()?;
        let after = not_after.map(parse_rfc3339_utc).transpose()?;
        if let (Some(b), Some(a)) = (before, after) {
            if b >= a {
                return Err(format!(
                    "not_before ({b}) is not earlier than not_after ({a})"
                ));
            }
        }
        Ok(Self {
            not_before: before,
            not_after: after,
        })
    }

    pub fn contains(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        if let Some(before) = self.not_before {
            if now < before {
                return false;
            }
        }
        if let Some(after) = self.not_after {
            if now > after {
                return false;
            }
        }
        true
    }

    fn render(bound: Option<chrono::DateTime<chrono::Utc>>) -> String {
        bound
            .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
            .unwrap_or_default()
    }
}

/// Strict RFC 3339 parse, normalised to UTC.
///
/// `chrono` accepts a numeric offset or `Z`; both are converted to UTC
/// so comparison is never lexicographic. Anything else — a bare date, a
/// space separator, a local timestamp with no offset, an impossible
/// date, a year outside a sane range — is a hard error.
fn parse_rfc3339_utc(raw: &str) -> Result<chrono::DateTime<chrono::Utc>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("empty timestamp".to_string());
    }
    if trimmed != raw {
        return Err(format!("timestamp `{raw}` has surrounding whitespace"));
    }
    // `chrono` tolerates a space where RFC 3339 requires `T`. Accepting
    // both would mean two spellings of one instant, and the looser one
    // is exactly what a hand-edited trust file is likely to contain.
    if trimmed.len() < 11 || !matches!(trimmed.as_bytes()[10], b'T' | b't') {
        return Err(format!(
            "`{raw}` is not RFC 3339: the date and time must be separated by `T`"
        ));
    }
    let parsed = chrono::DateTime::parse_from_rfc3339(trimmed)
        .map_err(|e| format!("`{raw}` is not a valid RFC 3339 timestamp: {e}"))?;
    let utc = parsed.with_timezone(&chrono::Utc);
    // Reject values that would make ordering meaningless. Trust entries
    // are operational data, not archaeology.
    let year = chrono::Datelike::year(&utc);
    if !(2000..=2200).contains(&year) {
        return Err(format!("`{raw}` is outside the supported 2000-2200 range"));
    }
    Ok(utc)
}

/// A persistent, explicitly recorded decision to run one unsigned
/// package from a development tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevGrant {
    pub kind: PackageKind,
    pub id: String,
    /// Absolute path of the development tree the grant covers.
    pub path: PathBuf,
    /// Content digest of the tree at the moment the operator agreed.
    /// A changed tree invalidates the grant and must be re-approved.
    pub content_digest: String,
    pub granted_at: String,
    #[serde(default)]
    pub note: Option<String>,
}

impl DevGrant {
    pub fn key(kind: PackageKind, id: &str) -> String {
        format!("{}/{}", kind.as_str(), id)
    }

    pub fn store_key(&self) -> String {
        Self::key(self.kind, &self.id)
    }
}

// --------------------------------------------------------------- on-disk

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustFile {
    schema: String,
    #[serde(default)]
    keys: Vec<TrustFileKey>,
    #[serde(default)]
    revoked_keys: Vec<String>,
    #[serde(default)]
    revoked_packages: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustFileKey {
    key_id: String,
    algorithm: String,
    public_key: String,
    #[serde(default)]
    usages: Vec<String>,
    #[serde(default)]
    kinds: Vec<PackageKind>,
    #[serde(default = "default_status")]
    status: String,
    #[serde(default)]
    not_before: Option<String>,
    #[serde(default)]
    not_after: Option<String>,
    #[serde(default)]
    comment: Option<String>,
}

fn default_status() -> String {
    "active".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DevTrustFile {
    schema: String,
    #[serde(default)]
    grants: Vec<DevGrant>,
}

// ------------------------------------------------------------ trust store

/// A trust root to load, with the uid set that may own it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustRootSpec {
    pub path: PathBuf,
    pub tier: TrustTier,
    pub allowed_uids: Vec<u32>,
    /// Which durable state file versions this root. Roots in the same
    /// domain share one generation counter and one fingerprint.
    pub domain: TrustDomain,
}

impl TrustRootSpec {
    /// Directory holding the domain's `state.json`. For the system
    /// domain that is `/etc/cos/trust`; for an owner it is
    /// `~/.config/cos/trust`. Both are the parent of the roots
    /// themselves.
    pub fn state_dir(&self) -> PathBuf {
        self.path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.path.clone())
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TrustError {
    #[error("no trusted publisher key matches key id `{0}`")]
    UnknownKey(String),
    #[error("publisher key `{0}` has been revoked")]
    RevokedKey(String),
    #[error("publisher key `{key_id}` is outside its validity window ({window})")]
    OutsideValidity { key_id: String, window: String },
    #[error("publisher key `{key_id}` is not authorised for `{usage}`")]
    UsageNotPermitted { key_id: String, usage: String },
    #[error("publisher key `{key_id}` is not authorised to sign `{kind}` packages")]
    KindNotPermitted { key_id: String, kind: &'static str },
    #[error("package content digest `{0}` has been revoked")]
    RevokedPackage(String),
    #[error("trusted key `{key_id}` does not match the key material in the package")]
    KeyMaterialMismatch { key_id: String },
}

/// The loaded set of publisher keys, revocations and developer grants.
#[derive(Debug, Clone, Default)]
pub struct TrustStore {
    keys: BTreeMap<String, TrustedKey>,
    revoked_keys: BTreeSet<String>,
    revoked_packages: BTreeSet<String>,
    dev_grants: BTreeMap<String, DevGrant>,
    diagnostics: Vec<String>,
    /// Durable per-domain generation state observed at load time.
    domains: BTreeMap<String, TrustState>,
    /// Cheap stamps a long-lived process re-checks to decide whether
    /// this store is still current.
    watch: TrustWatch,
    /// Digest over everything above. Callers cache verification
    /// results keyed by this value, so revoking a key or a package
    /// digest invalidates every cache entry at once.
    generation: String,
}

impl TrustStore {
    /// Production trust roots, in the order they are consulted.
    ///
    /// The per-user roots resolve from the passwd home of the
    /// effective uid; when that cannot be resolved (no passwd entry,
    /// non-Unix host) only the root-owned roots are used.
    pub fn default_roots() -> Vec<TrustRootSpec> {
        Self::roots_for_owner(fsec::effective_uid())
    }

    /// Production roots for an explicitly authenticated owner.
    ///
    /// Privileged brokers use this before dropping to a dedicated execution
    /// identity. The owner comes from kernel-authenticated task state, never
    /// from an extension or manifest.
    pub fn roots_for_owner(owner_uid: u32) -> Vec<TrustRootSpec> {
        let mut roots = vec![
            TrustRootSpec {
                path: PathBuf::from(VENDOR_TRUST_ROOT),
                tier: TrustTier::Vendor,
                allowed_uids: vec![0],
                domain: TrustDomain::System,
            },
            TrustRootSpec {
                path: PathBuf::from(SYSTEM_TRUST_ROOT),
                tier: TrustTier::System,
                allowed_uids: vec![0],
                domain: TrustDomain::System,
            },
        ];
        #[cfg(unix)]
        {
            if let Ok(home) = crate::paths::verified_home_for_uid(owner_uid) {
                roots.push(TrustRootSpec {
                    path: home.join(USER_TRUST_SUBDIR),
                    tier: TrustTier::User,
                    allowed_uids: vec![owner_uid],
                    domain: TrustDomain::Owner(owner_uid),
                });
                roots.push(TrustRootSpec {
                    path: home.join(DEV_TRUST_SUBDIR),
                    tier: TrustTier::Developer,
                    allowed_uids: vec![owner_uid],
                    domain: TrustDomain::Owner(owner_uid),
                });
            }
        }
        roots
    }

    /// Load the production trust roots.
    pub fn load_default() -> Self {
        Self::load_roots(&Self::default_roots())
    }

    /// Load an explicit root list. Used by the CLI (to inspect a
    /// specific root) and by tests; never driven by the environment.
    pub fn load_roots(roots: &[TrustRootSpec]) -> Self {
        let mut store = Self::default();
        let usable = store.validate_domains(roots);
        for root in roots {
            if !usable.contains(&root.domain.as_key()) {
                continue;
            }
            store.load_root(root);
        }
        store.watch = TrustWatch::observe(&Self::watch_paths(roots));
        store.recompute_generation();
        store
    }

    /// Every path a reader stats to notice a change.
    ///
    /// The state file and each root directory catch additions,
    /// removals and re-recorded generations. The individual trust files
    /// are included too: editing one in place changes neither the
    /// directory's mtime nor the state file, and a daemon must still
    /// notice — an unrecorded edit then fails the domain closed on the
    /// fingerprint check rather than being honoured.
    pub fn watch_paths(roots: &[TrustRootSpec]) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
        for root in roots {
            for path in state::watch_paths(&root.state_dir(), std::slice::from_ref(&root.path)) {
                if seen.insert(path.clone()) {
                    out.push(path);
                }
            }
            if let Ok(entries) = std::fs::read_dir(&root.path) {
                let mut files: Vec<PathBuf> = entries
                    .filter_map(Result::ok)
                    .map(|e| e.path())
                    .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
                    .collect();
                files.sort();
                for path in files {
                    if seen.insert(path.clone()) {
                        out.push(path);
                    }
                }
            }
        }
        out
    }

    /// Validate each domain's durable state before any of its roots are
    /// read, and return the domains that may contribute.
    ///
    /// A domain whose state file is corrupt, mis-owned, or disagrees
    /// with the bytes on disk contributes nothing: an operator's
    /// revocation must not be undone by restoring an old trust file, so
    /// the safe answer to "I cannot tell which version this is" is no
    /// keys at all.
    fn validate_domains(&mut self, roots: &[TrustRootSpec]) -> BTreeSet<String> {
        let mut usable = BTreeSet::new();
        let mut domains: BTreeMap<String, (TrustDomain, PathBuf, Vec<PathBuf>)> = BTreeMap::new();
        for root in roots {
            let entry = domains
                .entry(root.domain.as_key())
                .or_insert_with(|| (root.domain, root.state_dir(), Vec::new()));
            entry.2.push(root.path.clone());
        }

        for (key, (domain, state_dir, root_paths)) in domains {
            let recorded = match state::read_state(&state_dir, domain) {
                Ok(recorded) => recorded,
                Err(e) => {
                    self.note(format!("trust domain `{key}` fails closed: {e}"));
                    continue;
                }
            };
            // Read the domain's trust files first, because whether a
            // *missing* state file is legitimate depends on whether the
            // domain has any content at all.
            let mut observed: Vec<(String, Vec<u8>)> = Vec::new();
            let mut readable = true;
            for path in &root_paths {
                match state::read_domain_files(path) {
                    Ok(files) => observed.extend(files),
                    Err(e) => {
                        self.note(format!("trust domain `{key}` fails closed: {e}"));
                        readable = false;
                        break;
                    }
                }
            }
            if !readable {
                continue;
            }
            let Some(recorded) = recorded else {
                if observed.is_empty() {
                    // Never initialised: no trust files and no state.
                    // A legitimate empty domain on a machine where
                    // nobody has installed a key, and still fail-closed
                    // — no keys means nothing verifies.
                    usable.insert(key);
                } else {
                    // Trust files but no state. The domain *was*
                    // initialised — every command that writes one of
                    // these files records the state in the same
                    // operation — so the state was removed after the
                    // fact. Since the state is the only record of which
                    // generation these bytes belong to, treating its
                    // absence as "fresh" would make deleting one file
                    // the way to reinstate a revoked key. It fails
                    // closed instead, and says how to fix it.
                    self.note(format!(
                        "trust domain `{key}` fails closed: {} trust file(s) are present but \
                         `{}` is missing; re-run a `cos provenance trust` command to \
                         re-record the domain's generation",
                        observed.len(),
                        state::TRUST_STATE_FILE
                    ));
                }
                continue;
            };
            let fingerprint = state::fingerprint_files(&observed);
            if fingerprint != recorded.fingerprint {
                self.note(format!(
                    "trust domain `{key}` fails closed: recorded fingerprint does not match \
                     the trust files on disk (generation {}); re-run a `cos provenance trust` \
                     command to re-record it",
                    recorded.generation
                ));
                continue;
            }
            self.domains.insert(key.clone(), recorded);
            usable.insert(key);
        }
        usable
    }

    /// Cheap check that this store still matches the roots on disk.
    ///
    /// Long-lived daemons call this before every launch, disclosure,
    /// attach and authority use. It stats a handful of paths; only a
    /// change forces the expensive reload.
    pub fn is_current(&self, roots: &[TrustRootSpec]) -> bool {
        self.watch == TrustWatch::observe(&Self::watch_paths(roots))
    }

    /// Durable generation of one domain, when it has been initialised.
    pub fn domain_generation(&self, domain: TrustDomain) -> Option<u64> {
        self.domains.get(&domain.as_key()).map(|s| s.generation)
    }

    fn note(&mut self, message: impl Into<String>) {
        let message = message.into();
        tracing::warn!(target: "provenance", "{message}");
        self.diagnostics.push(message);
    }

    fn load_root(&mut self, root: &TrustRootSpec) {
        let meta = match fsec::require_secure_location(&root.path, &root.allowed_uids) {
            Ok(meta) => meta,
            Err(PathTrustError::Unreadable { .. }) => return, // absent root is normal
            Err(e) => {
                self.note(format!("trust root ignored: {e}"));
                return;
            }
        };
        if !meta.is_dir {
            self.note(format!(
                "trust root ignored: {} is not a directory",
                root.path.display()
            ));
            return;
        }
        let handle = match fsec::DirHandle::open(&root.path) {
            Ok(h) => h,
            Err(e) => {
                self.note(format!("trust root {}: {e}", root.path.display()));
                return;
            }
        };
        let entries = match handle.entries(None) {
            Ok(e) => e,
            Err(e) => {
                self.note(format!("trust root {}: {e}", root.path.display()));
                return;
            }
        };
        if entries.len() > MAX_TRUST_FILES {
            self.note(format!(
                "trust root {} holds {} entries; cap is {MAX_TRUST_FILES}",
                root.path.display(),
                entries.len()
            ));
            return;
        }
        for (name, node) in entries {
            if !name.ends_with(".json") || name.starts_with('.') {
                continue;
            }
            let file_path = root.path.join(&name);
            if node.is_symlink {
                self.note(format!(
                    "trust file ignored: {} is a symlink",
                    file_path.display()
                ));
                continue;
            }
            if !node.is_file {
                continue;
            }
            if !root.allowed_uids.contains(&node.uid) {
                self.note(format!(
                    "trust file ignored: {} is owned by uid {}",
                    file_path.display(),
                    node.uid
                ));
                continue;
            }
            if node.is_group_or_world_writable() {
                self.note(format!(
                    "trust file ignored: {} has mode {:o}",
                    file_path.display(),
                    node.mode
                ));
                continue;
            }
            if node.size > MAX_TRUST_FILE_BYTES {
                self.note(format!(
                    "trust file ignored: {} is {} bytes",
                    file_path.display(),
                    node.size
                ));
                continue;
            }
            let raw = match handle.open_file(&name).and_then(|fd| {
                fd.read_bounded(MAX_TRUST_FILE_BYTES)
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
            }) {
                Ok(raw) => raw,
                Err(e) => {
                    self.note(format!("trust file {}: {e}", file_path.display()));
                    continue;
                }
            };
            match root.tier {
                TrustTier::Developer => self.ingest_dev_file(&file_path, &raw),
                tier => self.ingest_trust_file(&file_path, &raw, tier),
            }
        }
    }

    fn ingest_trust_file(&mut self, path: &Path, raw: &str, tier: TrustTier) {
        let parsed: TrustFile = match serde_json::from_str(raw) {
            Ok(p) => p,
            Err(e) => {
                self.note(format!("trust file {}: {e}", path.display()));
                return;
            }
        };
        if parsed.schema != TRUST_SCHEMA_V1 {
            self.note(format!(
                "trust file {}: unsupported schema `{}`",
                path.display(),
                parsed.schema
            ));
            return;
        }
        for digest in parsed.revoked_packages {
            if is_sha256_ref(&digest) {
                self.revoked_packages.insert(digest);
            } else {
                self.note(format!(
                    "trust file {}: ignoring malformed revoked package digest",
                    path.display()
                ));
            }
        }
        for key_id in parsed.revoked_keys {
            if is_sha256_ref(&key_id) {
                self.revoked_keys.insert(key_id);
            } else {
                self.note(format!(
                    "trust file {}: ignoring malformed revoked key id",
                    path.display()
                ));
            }
        }
        for entry in parsed.keys {
            match self.build_key(path, entry, tier) {
                Ok(key) => {
                    if let Some(existing) = self.keys.get(&key.key_id) {
                        if existing.public_key != key.public_key {
                            // Cannot happen while ids are digests, but
                            // fail closed rather than pick a winner.
                            self.note(format!(
                                "trust file {}: key id {} conflicts with {}",
                                path.display(),
                                key.key_id,
                                existing.source.display()
                            ));
                            self.revoked_keys.insert(key.key_id);
                            continue;
                        }
                        // Keep the strongest (lowest) tier.
                        if existing.tier <= key.tier {
                            continue;
                        }
                    }
                    self.keys.insert(key.key_id.clone(), key);
                }
                Err(reason) => {
                    self.note(format!("trust file {}: {reason}", path.display()));
                }
            }
        }
    }

    fn build_key(
        &mut self,
        source: &Path,
        entry: TrustFileKey,
        tier: TrustTier,
    ) -> Result<TrustedKey, String> {
        if entry.algorithm != ALG_ED25519 {
            return Err(format!("unsupported key algorithm `{}`", entry.algorithm));
        }
        if !is_lower_hex(&entry.public_key, 32) {
            return Err("public_key must be 64 lowercase hex characters".to_string());
        }
        let bytes = hex::decode(&entry.public_key).map_err(|e| e.to_string())?;
        let public_key: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| "public_key must be 32 bytes".to_string())?;
        let computed = key_id_for(&public_key);
        if computed != entry.key_id {
            return Err(format!(
                "key id `{}` does not bind to its public key (expected `{computed}`)",
                entry.key_id
            ));
        }
        let validity = Validity::parse(entry.not_before.as_deref(), entry.not_after.as_deref())
            .map_err(|reason| {
                format!(
                    "key `{}` has an invalid validity window: {reason}",
                    entry.key_id
                )
            })?;
        let status = entry.status.to_ascii_lowercase();
        match status.as_str() {
            "active" => {}
            "revoked" => {
                self.revoked_keys.insert(entry.key_id.clone());
            }
            other => return Err(format!("unknown key status `{other}`")),
        }
        let usages: BTreeSet<String> = if entry.usages.is_empty() {
            BTreeSet::new()
        } else {
            entry.usages.into_iter().collect()
        };
        if !usages.contains(USAGE_PACKAGE_SIGNING) {
            return Err(format!(
                "key `{}` does not declare the `{USAGE_PACKAGE_SIGNING}` usage",
                entry.key_id
            ));
        }
        let kinds: BTreeSet<PackageKind> = if entry.kinds.is_empty() {
            [PackageKind::App, PackageKind::Skill, PackageKind::Mcp]
                .into_iter()
                .collect()
        } else {
            entry.kinds.into_iter().collect()
        };
        Ok(TrustedKey {
            key_id: entry.key_id,
            public_key,
            usages,
            kinds,
            tier,
            source: source.to_path_buf(),
            comment: entry.comment,
            validity,
        })
    }

    fn ingest_dev_file(&mut self, path: &Path, raw: &str) {
        let parsed: DevTrustFile = match serde_json::from_str(raw) {
            Ok(p) => p,
            Err(e) => {
                self.note(format!("developer trust file {}: {e}", path.display()));
                return;
            }
        };
        if parsed.schema != DEV_TRUST_SCHEMA_V1 {
            self.note(format!(
                "developer trust file {}: unsupported schema `{}`",
                path.display(),
                parsed.schema
            ));
            return;
        }
        for grant in parsed.grants {
            if !is_sha256_ref(&grant.content_digest) {
                self.note(format!(
                    "developer trust file {}: grant for `{}` has a malformed digest",
                    path.display(),
                    grant.id
                ));
                continue;
            }
            if !grant.path.is_absolute() {
                self.note(format!(
                    "developer trust file {}: grant for `{}` has a relative path",
                    path.display(),
                    grant.id
                ));
                continue;
            }
            self.dev_grants.insert(grant.store_key(), grant);
        }
    }

    fn recompute_generation(&mut self) {
        let mut h = crate::crypto::Sha256Stream::new();
        h.update(b"claw-provenance/v1\x00trust-generation\x00");
        for (id, key) in &self.keys {
            h.update(id.as_bytes());
            h.update(&key.public_key);
            h.update(key.tier.as_str().as_bytes());
            h.update(Validity::render(key.validity.not_before).as_bytes());
            h.update(Validity::render(key.validity.not_after).as_bytes());
        }
        for (domain, recorded) in &self.domains {
            h.update(b"dom");
            h.update(domain.as_bytes());
            h.update(&recorded.generation.to_le_bytes());
            h.update(recorded.fingerprint.as_bytes());
        }
        for id in &self.revoked_keys {
            h.update(b"rk");
            h.update(id.as_bytes());
        }
        for digest in &self.revoked_packages {
            h.update(b"rp");
            h.update(digest.as_bytes());
        }
        for (key, grant) in &self.dev_grants {
            h.update(b"dg");
            h.update(key.as_bytes());
            h.update(grant.content_digest.as_bytes());
        }
        self.generation = h.finalize_hex();
    }

    /// Stable digest of the store's contents. Verification caches key
    /// on this so a revocation invalidates them immediately.
    pub fn generation(&self) -> &str {
        &self.generation
    }

    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }

    pub fn keys(&self) -> impl Iterator<Item = &TrustedKey> {
        self.keys.values()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn dev_grants(&self) -> impl Iterator<Item = &DevGrant> {
        self.dev_grants.values()
    }

    pub fn dev_grant(&self, kind: PackageKind, id: &str) -> Option<&DevGrant> {
        self.dev_grants.get(&DevGrant::key(kind, id))
    }

    pub fn is_package_revoked(&self, content_digest: &str) -> bool {
        self.revoked_packages.contains(content_digest)
    }

    pub fn is_key_revoked(&self, key_id: &str) -> bool {
        self.revoked_keys.contains(key_id)
    }

    pub fn key(&self, key_id: &str) -> Option<&TrustedKey> {
        self.keys.get(key_id)
    }

    /// Resolve a package signature to a usable trust decision.
    ///
    /// The caller supplies the key id and key material taken from the
    /// envelope. Both must agree with the trusted entry: matching the
    /// id alone would let a package present a trusted id with a
    /// different key.
    pub fn authorize(
        &self,
        key_id: &str,
        public_key: &[u8; 32],
        kind: PackageKind,
        content_digest: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<&TrustedKey, TrustError> {
        if self.revoked_packages.contains(content_digest) {
            return Err(TrustError::RevokedPackage(content_digest.to_string()));
        }
        if self.revoked_keys.contains(key_id) {
            return Err(TrustError::RevokedKey(key_id.to_string()));
        }
        let key = self
            .keys
            .get(key_id)
            .ok_or_else(|| TrustError::UnknownKey(key_id.to_string()))?;
        if &key.public_key != public_key {
            return Err(TrustError::KeyMaterialMismatch {
                key_id: key_id.to_string(),
            });
        }
        if !key.usages.contains(USAGE_PACKAGE_SIGNING) {
            return Err(TrustError::UsageNotPermitted {
                key_id: key_id.to_string(),
                usage: USAGE_PACKAGE_SIGNING.to_string(),
            });
        }
        if !key.kinds.contains(&kind) {
            return Err(TrustError::KindNotPermitted {
                key_id: key_id.to_string(),
                kind: kind.as_str(),
            });
        }
        if !key.validity.contains(now) {
            return Err(TrustError::OutsideValidity {
                key_id: key_id.to_string(),
                window: format!(
                    "not_before={} not_after={}",
                    Validity::render(key.validity.not_before),
                    Validity::render(key.validity.not_after)
                ),
            });
        }
        Ok(key)
    }
}

/// Resolve the per-user publisher trust directory for the effective
/// uid. Errors when the owner's home cannot be verified.
pub fn user_trust_dir() -> Result<PathBuf, String> {
    owner_subdir(USER_TRUST_SUBDIR)
}

/// Resolve the per-user developer trust directory for the effective uid.
pub fn developer_trust_dir() -> Result<PathBuf, String> {
    owner_subdir(DEV_TRUST_SUBDIR)
}

fn owner_subdir(subdir: &str) -> Result<PathBuf, String> {
    #[cfg(unix)]
    {
        let uid = fsec::effective_uid();
        let home = crate::paths::verified_home_for_uid(uid)?;
        Ok(home.join(subdir))
    }
    #[cfg(not(unix))]
    {
        let _ = subdir;
        Err("per-user trust stores require a Unix host".to_string())
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/provenance/trust.rs"
    ));
}
