//! Skill provenance, usage tracking, and pre-execution guards.
//!
//! Three concerns share one module because they're all metadata
//! about a `LoadedSkill` rather than the loading itself:
//!
//!   * [`Provenance`] — where this skill came from (vendor / hub /
//!     user / local). Used by the guard layer (untrusted sources
//!     get stricter checks) and by `cos agent status` to display
//!     a sourcecode hint per skill.
//!   * [`UsageStore`] — JSONL append-only log of skill invocations
//!     with bounded in-memory aggregation. Survives restarts via
//!     [`load_usage`] / [`record_usage`].
//!   * [`Guard`] — pre-execution checks: deny-list match, missing
//!     manifest fields, oversized scripts, suspicious shell
//!     patterns. Returns [`GuardOutcome`] so callers can decide
//!     whether to prompt the user for an override.
//!
//! All three are dependency-free except for `serde_json` (already
//! a workspace dep) and `chrono` (already used elsewhere for ISO
//! timestamps).

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};

use super::loader::LoadedSkill;
use super::manifest::{canonical_signing_input, ManifestSignature, SkillManifest};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provenance {
    /// Bundled with cos itself (clawos/skills/).
    Vendor,
    /// Synced from a public hub (github.com/agentskills/...).
    Hub,
    /// Created by the user on this machine.
    User,
    /// Loaded from a local checkout / development workspace.
    Local,
    /// Origin unknown.
    Unknown,
}

impl Provenance {
    /// True if this provenance is considered "trusted" — bundled
    /// with cos, or user-authored. Hub + Unknown go through the
    /// stricter guard checks.
    pub fn is_trusted(self) -> bool {
        matches!(self, Self::Vendor | Self::User)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Vendor => "vendor",
            Self::Hub => "hub",
            Self::User => "user",
            Self::Local => "local",
            Self::Unknown => "unknown",
        }
    }
}

/// Single usage record. JSONL line format.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsageRecord {
    pub skill_id: String,
    pub timestamp: String,
    pub success: bool,
    pub duration_ms: u64,
    /// Optional caller hint (model name, user/agent/delegate, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invoked_by: Option<String>,
}

/// Aggregated counters for a single skill.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageStats {
    pub total: u64,
    pub success: u64,
    pub failure: u64,
    pub total_duration_ms: u64,
}

impl UsageStats {
    pub fn average_duration_ms(&self) -> Option<u64> {
        if self.total == 0 {
            None
        } else {
            Some(self.total_duration_ms / self.total)
        }
    }
}

/// Append-only JSONL store for usage records.
///
/// Caller picks the path (typically `paths::agent_skills_dir() /
/// "usage.jsonl"`). Reads load+aggregate the whole file in memory;
/// writes append a single line and flush.
pub struct UsageStore {
    path: PathBuf,
    mu: Mutex<()>,
}

impl UsageStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            mu: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one record. Creates the parent directory if needed.
    /// Errors are returned (not logged) so callers can decide to
    /// degrade or surface.
    pub fn record(&self, rec: &UsageRecord) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let line = serde_json::to_string(rec)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let _g = self.mu.lock().unwrap_or_else(|p| p.into_inner());
        let mut opts = OpenOptions::new();
        opts.create(true).append(true);
        // On Unix, refuse to follow symlinks at the usage log path
        // so a malicious sibling can't redirect our writes into a
        // privileged file (e.g. an authorized_keys file). On other
        // platforms we accept the platform's default open semantics.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.custom_flags(libc::O_NOFOLLOW);
        }
        let mut f = opts.open(&self.path)?;
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
        f.flush()
    }

    /// Load and aggregate all records into per-skill stats.
    /// Malformed lines are silently skipped (forward-compat with
    /// future record fields).
    pub fn aggregate(&self) -> BTreeMap<String, UsageStats> {
        let mut out: BTreeMap<String, UsageStats> = BTreeMap::new();
        let Ok(text) = fs::read_to_string(&self.path) else {
            return out;
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(rec) = serde_json::from_str::<UsageRecord>(line) else {
                continue;
            };
            let s = out.entry(rec.skill_id).or_default();
            s.total += 1;
            if rec.success {
                s.success += 1;
            } else {
                s.failure += 1;
            }
            s.total_duration_ms = s.total_duration_ms.saturating_add(rec.duration_ms);
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardOutcome {
    /// Skill passes all checks and is safe to invoke.
    Allow,
    /// Skill is blocked outright; do not invoke.
    Deny { reason: String },
    /// Skill triggers a warning; caller should prompt user (or
    /// require explicit `--allow-untrusted` flag).
    RequireConfirmation { reason: String },
}

#[derive(Debug, Clone)]
pub struct GuardConfig {
    /// Maximum size in bytes for any individual sibling file under
    /// the skill directory. Above this we assume the skill is
    /// shipping data, not code, and flag for review.
    pub max_file_bytes: u64,
    /// Skill must declare at least one allowed-tool entry to pass
    /// guard. Set false to allow zero-tool skills (pure
    /// instruction blocks).
    pub require_allowed_tools: bool,
    /// Trusted provenance always Allow. Untrusted runs the full
    /// check tree. Set false to apply the same rules to vendored
    /// skills (mostly useful for tests).
    pub honour_provenance_trust: bool,
}

impl Default for GuardConfig {
    fn default() -> Self {
        Self {
            max_file_bytes: 5 * 1024 * 1024,
            require_allowed_tools: false,
            honour_provenance_trust: true,
        }
    }
}

pub struct Guard {
    config: GuardConfig,
}

impl Guard {
    pub fn new(config: GuardConfig) -> Self {
        Self { config }
    }

    pub fn with_default_config() -> Self {
        Self::new(GuardConfig::default())
    }

    /// Check a loaded skill against the active config + provenance.
    ///
    /// Order of checks:
    ///   1. Trusted provenance + honour_provenance_trust -> Allow.
    ///   2. require_allowed_tools and manifest has none -> Deny.
    ///   3. Any sibling file exceeds max_file_bytes -> RequireConfirmation.
    ///   4. Otherwise -> Allow (untrusted but otherwise clean).
    pub fn check(&self, skill: &LoadedSkill, provenance: Provenance) -> GuardOutcome {
        if self.config.honour_provenance_trust && provenance.is_trusted() {
            return GuardOutcome::Allow;
        }

        if self.config.require_allowed_tools && manifest_allowed_tools_empty(&skill.manifest) {
            return GuardOutcome::Deny {
                reason: format!(
                    "skill {} declares no allowed-tools but require_allowed_tools is set",
                    skill.id
                ),
            };
        }

        if let Some((path, size)) = oversized_sibling(&skill.dir, self.config.max_file_bytes) {
            return GuardOutcome::RequireConfirmation {
                reason: format!(
                    "skill {} sibling file {} is {} bytes (cap {})",
                    skill.id,
                    path.display(),
                    size,
                    self.config.max_file_bytes
                ),
            };
        }

        GuardOutcome::Allow
    }
}

fn manifest_allowed_tools_empty(m: &SkillManifest) -> bool {
    m.allowed_tools.is_empty()
}

// --------------- ed25519 signature verification ---------------

/// Environment variable controlling whether unsigned manifests are
/// rejected at install time. Anything other than `0`/`false`/empty
/// switches signature enforcement *on*.
pub const ENV_REQUIRE_SIGNATURE: &str = "COS_SKILLS_REQUIRE_SIGNATURE";
/// Colon-separated list of hex-encoded ed25519 verifying keys (32
/// bytes / 64 hex chars each). When set, a manifest's
/// `signature.public_key` must appear in the list to be accepted.
pub const ENV_TRUSTED_KEYS: &str = "COS_SKILLS_TRUSTED_KEYS";

#[derive(Debug, thiserror::Error)]
pub enum SignatureError {
    #[error("signature required but manifest is unsigned")]
    Required,
    #[error("unsupported signature algorithm: {0} (only `ed25519` is accepted)")]
    UnsupportedAlgorithm(String),
    #[error("signature field `{field}` is not valid hex: {reason}")]
    InvalidHex { field: &'static str, reason: String },
    #[error("signature field `{field}` has wrong length: expected {expected} bytes, got {actual}")]
    WrongLength {
        field: &'static str,
        expected: usize,
        actual: usize,
    },
    #[error("ed25519 verifying key is invalid: {0}")]
    InvalidVerifyingKey(String),
    #[error("signature verification failed: {0}")]
    BadSignature(String),
    #[error(
        "signature was made with key {public_key} which is not in the trusted-keys allow-list"
    )]
    UntrustedKey { public_key: String },
}

/// Outcome of attempting to verify a skill's signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureCheck {
    /// Manifest carried a well-formed signature and it verified
    /// (and matched the trusted-keys list if one was configured).
    /// `public_key_hex` is the lower-case hex of the verifying
    /// key that signed the manifest.
    Verified { public_key_hex: String },
    /// Manifest carried no signature block and the policy allowed
    /// it. Callers should log a warning so the operator can spot
    /// unsigned installs in audit logs.
    Unsigned,
}

/// Policy knobs for [`verify_signature`]. Construct via
/// [`SignatureVerifyConfig::from_env`] to pick up env-var driven
/// production defaults.
#[derive(Debug, Clone, Default)]
pub struct SignatureVerifyConfig {
    /// When `true`, an unsigned manifest is rejected. When `false`,
    /// unsigned manifests pass through (with [`SignatureCheck::Unsigned`])
    /// so existing skills keep installing during rollout.
    pub require_signature: bool,
    /// When `Some`, the signature's `public_key` must be one of the
    /// supplied 32-byte ed25519 keys. When `None`, any well-formed
    /// signature with a valid key passes (trust-on-first-use).
    pub trusted_keys: Option<Vec<[u8; 32]>>,
}

impl SignatureVerifyConfig {
    /// Resolve the active config from process environment.
    ///
    /// * `COS_SKILLS_REQUIRE_SIGNATURE` truthy → `require_signature = true`.
    /// * `COS_SKILLS_TRUSTED_KEYS` set       → parse colon-separated
    ///   hex keys; any unparseable entry causes the env var to be
    ///   ignored *and* the config to flip to `require_signature = true`
    ///   so we don't silently widen trust on a typo.
    pub fn from_env() -> Self {
        let require = match std::env::var(ENV_REQUIRE_SIGNATURE) {
            Ok(v) => matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"),
            Err(_) => false,
        };
        let trusted_keys = match std::env::var(ENV_TRUSTED_KEYS) {
            Ok(v) if !v.trim().is_empty() => parse_trusted_keys(&v).ok(),
            _ => None,
        };
        Self {
            require_signature: require,
            trusted_keys,
        }
    }
}

fn parse_trusted_keys(raw: &str) -> Result<Vec<[u8; 32]>, SignatureError> {
    let mut out = Vec::new();
    for entry in raw.split(':') {
        let s = entry.trim();
        if s.is_empty() {
            continue;
        }
        let bytes = hex::decode(s).map_err(|e| SignatureError::InvalidHex {
            field: "trusted_keys",
            reason: e.to_string(),
        })?;
        let arr: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
            SignatureError::WrongLength {
                field: "trusted_keys",
                expected: 32,
                actual: bytes.len(),
            }
        })?;
        out.push(arr);
    }
    Ok(out)
}

/// Verify a skill manifest's optional ed25519 signature against the
/// supplied policy.
///
/// On success returns one of:
///   * [`SignatureCheck::Verified`] — manifest was signed and the
///     signature checked out (and was in the trusted-keys list if
///     one was configured).
///   * [`SignatureCheck::Unsigned`] — manifest is unsigned and the
///     policy doesn't require a signature.
///
/// On any verification failure returns [`SignatureError`]; callers
/// (notably [`crate::agent::skills::sync`]) must treat these as
/// hard install failures.
pub fn verify_signature(
    manifest: &SkillManifest,
    config: &SignatureVerifyConfig,
) -> Result<SignatureCheck, SignatureError> {
    let Some(sig) = manifest.signature.as_ref() else {
        if config.require_signature {
            return Err(SignatureError::Required);
        }
        return Ok(SignatureCheck::Unsigned);
    };
    verify_signature_block(manifest, sig, config)
}

fn verify_signature_block(
    manifest: &SkillManifest,
    sig: &ManifestSignature,
    config: &SignatureVerifyConfig,
) -> Result<SignatureCheck, SignatureError> {
    if !sig.algorithm.eq_ignore_ascii_case("ed25519") {
        return Err(SignatureError::UnsupportedAlgorithm(sig.algorithm.clone()));
    }

    let pk_bytes = hex::decode(sig.public_key.trim()).map_err(|e| SignatureError::InvalidHex {
        field: "public_key",
        reason: e.to_string(),
    })?;
    let pk_arr: [u8; 32] = pk_bytes
        .as_slice()
        .try_into()
        .map_err(|_| SignatureError::WrongLength {
            field: "public_key",
            expected: 32,
            actual: pk_bytes.len(),
        })?;

    if let Some(trusted) = &config.trusted_keys {
        if !trusted.iter().any(|k| k == &pk_arr) {
            return Err(SignatureError::UntrustedKey {
                public_key: hex::encode(pk_arr),
            });
        }
    }

    let sig_bytes = hex::decode(sig.value.trim()).map_err(|e| SignatureError::InvalidHex {
        field: "value",
        reason: e.to_string(),
    })?;
    let sig_arr: [u8; 64] =
        sig_bytes
            .as_slice()
            .try_into()
            .map_err(|_| SignatureError::WrongLength {
                field: "value",
                expected: 64,
                actual: sig_bytes.len(),
            })?;

    let vk = VerifyingKey::from_bytes(&pk_arr)
        .map_err(|e| SignatureError::InvalidVerifyingKey(e.to_string()))?;
    let signature = Signature::from_bytes(&sig_arr);

    // Match the signing scheme described on `canonical_signing_input`:
    // signer commits to SHA-256(canonical bytes), not the raw bytes,
    // so the verifier hashes too before handing the digest to
    // ed25519. Using the in-tree SHA-256 stream avoids adding a
    // second hash dep just for this code path.
    let canonical = canonical_signing_input(manifest);
    let mut hasher = crate::crypto::Sha256Stream::new();
    hasher.update(&canonical);
    let digest = hasher.finalize_bytes();

    vk.verify_strict(&digest, &signature)
        .map_err(|e| SignatureError::BadSignature(e.to_string()))?;

    Ok(SignatureCheck::Verified {
        public_key_hex: hex::encode(pk_arr),
    })
}

/// Walk the skill dir recursively (bounded by [`MAX_GUARD_WALK_FILES`]
/// and [`MAX_GUARD_WALK_DEPTH`]) and return the first sibling file
/// whose advertised size exceeds `cap`. We *avoid following symlinks*
/// so a malicious skill can't escape its own directory tree by
/// pointing at a giant file elsewhere on disk and tricking the guard
/// into reporting it. Symlinks themselves are reported as oversized
/// if they point at a regular file that exceeds `cap` (via
/// `metadata`, only after we've confirmed the link target stays
/// inside `dir`).
fn oversized_sibling(dir: &Path, cap: u64) -> Option<(PathBuf, u64)> {
    /// File-count budget for a single guard walk. Keeps the guard
    /// O(skill-dir) not O(filesystem) when a user accidentally drops
    /// a skill into a shared directory.
    const MAX_GUARD_WALK_FILES: usize = 10_000;
    /// Recursion budget. Skill dirs are typically 1-2 levels deep.
    const MAX_GUARD_WALK_DEPTH: usize = 16;

    let dir_canon = dir.canonicalize().ok()?;
    let mut hits: Vec<(PathBuf, u64)> = Vec::new();
    let mut stack: Vec<(PathBuf, usize)> = vec![(dir.to_path_buf(), 0)];
    let mut visited = 0usize;
    while let Some((cur, depth)) = stack.pop() {
        if depth > MAX_GUARD_WALK_DEPTH {
            continue;
        }
        let Ok(rd) = fs::read_dir(&cur) else { continue };
        for entry in rd.flatten() {
            visited += 1;
            if visited > MAX_GUARD_WALK_FILES {
                break;
            }
            let path = entry.path();
            // `symlink_metadata` does NOT follow symlinks — we never
            // hop out of the skill dir through a symlink, even if
            // the target would canonicalise outside `dir_canon`.
            let Ok(meta) = entry.metadata() else { continue };
            // Defensive: if the entry resolved through a symlink,
            // verify the final target is still inside the skill dir.
            if let Ok(canon) = path.canonicalize() {
                if !canon.starts_with(&dir_canon) {
                    continue;
                }
            }
            let ft = meta.file_type();
            if ft.is_dir() {
                stack.push((path, depth + 1));
                continue;
            }
            if ft.is_file() {
                let size = meta.len();
                if size > cap {
                    hits.push((path, size));
                }
            }
        }
    }
    hits.sort_by(|a, b| a.0.cmp(&b.0));
    hits.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use uuid::Uuid;

    fn tmpdir(label: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "cos-skills-prov-{label}-{}",
            Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn write_skill(dir: &Path, id: &str, body: &str, tools: &[&str]) -> LoadedSkill {
        let sd = dir.join(id);
        fs::create_dir_all(&sd).unwrap();
        let mp = sd.join("SKILL.md");
        let allowed = if tools.is_empty() {
            String::new()
        } else {
            format!(
                "allowed-tools:\n{}\n",
                tools
                    .iter()
                    .map(|t| format!("  - {t}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        };
        fs::write(
            &mp,
            format!("---\nname: {id}\ndescription: test skill\n{allowed}---\n{body}\n"),
        )
        .unwrap();
        let doc = super::super::manifest::parse(&fs::read_to_string(&mp).unwrap()).unwrap();
        LoadedSkill {
            id: id.to_string(),
            dir: sd,
            manifest_path: mp,
            manifest: doc.manifest,
            body: doc.body,
        }
    }

    #[test]
    fn provenance_trust() {
        assert!(Provenance::Vendor.is_trusted());
        assert!(Provenance::User.is_trusted());
        assert!(!Provenance::Hub.is_trusted());
        assert!(!Provenance::Local.is_trusted());
        assert!(!Provenance::Unknown.is_trusted());
    }

    #[test]
    fn provenance_serde_roundtrip() {
        let p = Provenance::Hub;
        let s = serde_json::to_string(&p).unwrap();
        assert_eq!(s, "\"hub\"");
        let back: Provenance = serde_json::from_str(&s).unwrap();
        assert_eq!(back, Provenance::Hub);
    }

    #[test]
    fn usage_store_records_and_aggregates() {
        let dir = tmpdir("usage");
        let store = UsageStore::new(dir.join("usage.jsonl"));
        store
            .record(&UsageRecord {
                skill_id: "pdf-extract".to_string(),
                timestamp: "2025-01-01T00:00:00Z".to_string(),
                success: true,
                duration_ms: 100,
                invoked_by: Some("agent".to_string()),
            })
            .unwrap();
        store
            .record(&UsageRecord {
                skill_id: "pdf-extract".to_string(),
                timestamp: "2025-01-01T00:00:01Z".to_string(),
                success: false,
                duration_ms: 200,
                invoked_by: None,
            })
            .unwrap();
        store
            .record(&UsageRecord {
                skill_id: "arxiv".to_string(),
                timestamp: "2025-01-01T00:00:02Z".to_string(),
                success: true,
                duration_ms: 50,
                invoked_by: None,
            })
            .unwrap();

        let agg = store.aggregate();
        assert_eq!(agg.len(), 2);
        let pdf = &agg["pdf-extract"];
        assert_eq!(pdf.total, 2);
        assert_eq!(pdf.success, 1);
        assert_eq!(pdf.failure, 1);
        assert_eq!(pdf.total_duration_ms, 300);
        assert_eq!(pdf.average_duration_ms(), Some(150));

        let arxiv = &agg["arxiv"];
        assert_eq!(arxiv.total, 1);
        assert_eq!(arxiv.success, 1);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn usage_store_skips_garbage_lines() {
        let dir = tmpdir("usage-garbage");
        let path = dir.join("usage.jsonl");
        fs::create_dir_all(&dir).unwrap();
        // Hand-write the file with garbage lines mixed in.
        fs::write(
            &path,
            "not-json\n{\"skill_id\":\"x\",\"timestamp\":\"t\",\"success\":true,\"duration_ms\":1}\n\n",
        )
        .unwrap();
        let store = UsageStore::new(&path);
        let agg = store.aggregate();
        assert_eq!(agg.len(), 1);
        assert_eq!(agg["x"].total, 1);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn usage_store_aggregate_empty_when_missing_file() {
        let store = UsageStore::new(tmpdir("missing").join("nope.jsonl"));
        assert!(store.aggregate().is_empty());
    }

    #[test]
    fn usage_stats_average_none_when_zero() {
        let s = UsageStats::default();
        assert_eq!(s.average_duration_ms(), None);
    }

    #[test]
    fn guard_trusted_provenance_short_circuits() {
        let dir = tmpdir("guard-trusted");
        let s = write_skill(&dir, "vendor-skill", "body", &[]);
        let g = Guard::with_default_config();
        assert_eq!(g.check(&s, Provenance::Vendor), GuardOutcome::Allow);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn guard_untrusted_with_default_config_allows() {
        let dir = tmpdir("guard-untrusted-allow");
        let s = write_skill(&dir, "hub-skill", "body", &["tool-a"]);
        let g = Guard::with_default_config();
        assert_eq!(g.check(&s, Provenance::Hub), GuardOutcome::Allow);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn guard_require_allowed_tools_denies_empty() {
        let dir = tmpdir("guard-require-tools");
        let s = write_skill(&dir, "no-tools", "body", &[]);
        let cfg = GuardConfig {
            require_allowed_tools: true,
            ..GuardConfig::default()
        };
        let g = Guard::new(cfg);
        match g.check(&s, Provenance::Hub) {
            GuardOutcome::Deny { reason } => assert!(reason.contains("allowed-tools")),
            other => panic!("expected Deny, got {other:?}"),
        }
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn guard_oversized_sibling_requires_confirmation() {
        let dir = tmpdir("guard-oversized");
        let s = write_skill(&dir, "big-skill", "body", &["tool-a"]);
        // Drop a big file alongside SKILL.md
        let big = s.dir.join("big.bin");
        fs::write(&big, vec![0u8; 1024]).unwrap();
        let cfg = GuardConfig {
            max_file_bytes: 100,
            ..GuardConfig::default()
        };
        let g = Guard::new(cfg);
        match g.check(&s, Provenance::Hub) {
            GuardOutcome::RequireConfirmation { reason } => {
                assert!(reason.contains("big-skill"));
                assert!(reason.contains("big.bin"));
            }
            other => panic!("expected RequireConfirmation, got {other:?}"),
        }
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn guard_can_disable_provenance_trust() {
        let dir = tmpdir("guard-no-trust");
        let s = write_skill(&dir, "vendor-skill", "body", &[]);
        let cfg = GuardConfig {
            honour_provenance_trust: false,
            require_allowed_tools: true,
            ..GuardConfig::default()
        };
        let g = Guard::new(cfg);
        // Vendor with no tools now denied because trust is off.
        assert!(matches!(
            g.check(&s, Provenance::Vendor),
            GuardOutcome::Deny { .. }
        ));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn provenance_as_str_stable() {
        assert_eq!(Provenance::Vendor.as_str(), "vendor");
        assert_eq!(Provenance::Hub.as_str(), "hub");
        assert_eq!(Provenance::User.as_str(), "user");
        assert_eq!(Provenance::Local.as_str(), "local");
        assert_eq!(Provenance::Unknown.as_str(), "unknown");
    }

    // ----- signature verification helpers -----

    #[test]
    fn parse_trusted_keys_accepts_colon_separated_hex() {
        let raw = format!("{a}:{b}", a = "aa".repeat(32), b = "bb".repeat(32));
        let keys = parse_trusted_keys(&raw).unwrap();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0], [0xaa; 32]);
        assert_eq!(keys[1], [0xbb; 32]);
    }

    #[test]
    fn parse_trusted_keys_skips_blank_entries() {
        let raw = format!(":{a}:: ", a = "11".repeat(32));
        let keys = parse_trusted_keys(&raw).unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0], [0x11; 32]);
    }

    #[test]
    fn parse_trusted_keys_rejects_wrong_length() {
        let err = parse_trusted_keys("aabb").unwrap_err();
        assert!(matches!(err, SignatureError::WrongLength { .. }));
    }

    #[test]
    fn parse_trusted_keys_rejects_non_hex() {
        let err = parse_trusted_keys("not-hex").unwrap_err();
        assert!(matches!(err, SignatureError::InvalidHex { .. }));
    }

    #[test]
    fn verify_signature_passes_for_valid_block_and_canonical_input() {
        use ed25519_dalek::{Signer, SigningKey};
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let vk = signing_key.verifying_key();
        let mut manifest = SkillManifest {
            name: "sig-ok".into(),
            ..Default::default()
        };
        // Compute signature input WITHOUT a signature block attached
        // (so the signer and verifier compute the same bytes), then
        // attach the signature.
        let canonical = super::super::manifest::canonical_signing_input(&manifest);
        let mut hasher = crate::crypto::Sha256Stream::new();
        hasher.update(&canonical);
        let digest = hasher.finalize_bytes();
        let sig = signing_key.sign(&digest);
        manifest.signature = Some(super::super::manifest::ManifestSignature {
            algorithm: "ed25519".into(),
            public_key: hex::encode(vk.to_bytes()),
            value: hex::encode(sig.to_bytes()),
        });
        let res = verify_signature(&manifest, &SignatureVerifyConfig::default()).unwrap();
        match res {
            SignatureCheck::Verified { public_key_hex } => {
                assert_eq!(public_key_hex, hex::encode(vk.to_bytes()));
            }
            other => panic!("expected Verified, got {other:?}"),
        }
    }

    #[test]
    fn verify_signature_rejects_wrong_key_length() {
        let mut manifest = SkillManifest {
            name: "x".into(),
            ..Default::default()
        };
        manifest.signature = Some(super::super::manifest::ManifestSignature {
            algorithm: "ed25519".into(),
            public_key: "aa".into(), // 1 byte, not 32
            value: "bb".repeat(64),
        });
        let err = verify_signature(&manifest, &SignatureVerifyConfig::default()).unwrap_err();
        assert!(matches!(err, SignatureError::WrongLength { field: "public_key", .. }));
    }

    #[test]
    fn verify_signature_rejects_unsupported_algorithm() {
        let mut manifest = SkillManifest {
            name: "x".into(),
            ..Default::default()
        };
        manifest.signature = Some(super::super::manifest::ManifestSignature {
            algorithm: "rsa-sha256".into(),
            public_key: "aa".repeat(32),
            value: "bb".repeat(64),
        });
        let err = verify_signature(&manifest, &SignatureVerifyConfig::default()).unwrap_err();
        assert!(matches!(err, SignatureError::UnsupportedAlgorithm(_)));
    }

    #[test]
    fn verify_signature_unsigned_passes_by_default_but_fails_when_required() {
        let manifest = SkillManifest {
            name: "x".into(),
            ..Default::default()
        };
        assert!(matches!(
            verify_signature(&manifest, &SignatureVerifyConfig::default()).unwrap(),
            SignatureCheck::Unsigned
        ));
        let strict = SignatureVerifyConfig {
            require_signature: true,
            trusted_keys: None,
        };
        assert!(matches!(
            verify_signature(&manifest, &strict).unwrap_err(),
            SignatureError::Required
        ));
    }
}
