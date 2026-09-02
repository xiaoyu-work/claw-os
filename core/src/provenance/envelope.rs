//! The `claw.provenance/v1` envelope: one versioned content manifest
//! shared by every extension kind (App, Skill, MCP/Adapter).
//!
//! An extension package is a directory. Its provenance envelope lives
//! at [`ENVELOPE_FILE`] inside that directory and binds:
//!
//!   * the publisher key (algorithm + collision-resistant key id),
//!   * the package kind / id / version,
//!   * the manifest schema and the manifest path inside the package,
//!   * the entrypoints and resources the runtime may execute or read,
//!   * the **complete** normalized file tree (path, type, permission
//!     bits, size, SHA-256 digest), and
//!   * a package content digest over that tree.
//!
//! Everything above is covered by one Ed25519 signature over
//! [`canonical_bytes`]. The canonical encoding is length-prefixed and
//! domain-separated, so two different envelopes can never produce the
//! same signing bytes and a field value can never be shifted into a
//! neighbouring field.
//!
//! Parsing is strict on purpose: unknown top-level fields, unknown
//! schema versions, unknown package kinds, unknown file types and
//! unknown signature algorithms are all hard rejects rather than
//! ignored input. Algorithm/version confusion is the classic way a
//! signed-package format degrades into an unsigned one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::crypto::Sha256Stream;

/// File name of the envelope inside a package directory.
pub const ENVELOPE_FILE: &str = ".provenance.json";

/// The only schema string this build accepts.
pub const SCHEMA_V1: &str = "claw.provenance/v1";

/// The only signature algorithm this build accepts.
pub const ALG_ED25519: &str = "ed25519";

/// Domain separator mixed into the canonical signing bytes. A
/// signature produced for some other Claw structure can never be
/// replayed as a package signature.
const CANON_DOMAIN: &[u8] = b"claw-provenance/v1\x00package-envelope\x00";

/// Hard caps that bound parsing work before any signature check.
pub const MAX_ENVELOPE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_FILES: usize = 20_000;
pub const MAX_PATH_BYTES: usize = 512;
pub const MAX_PATH_DEPTH: usize = 16;
pub const MAX_NAME_BYTES: usize = 128;
pub const MAX_ENTRYPOINTS: usize = 64;

/// Which extension surface a package belongs to. The kind is signed,
/// so an App package can never be presented as a Skill (or vice
/// versa) to reach a different capability ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageKind {
    App,
    Skill,
    Mcp,
}

impl PackageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Skill => "skill",
            Self::Mcp => "mcp",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "app" => Some(Self::App),
            "skill" => Some(Self::Skill),
            "mcp" => Some(Self::Mcp),
            _ => None,
        }
    }

    /// Default manifest file for the kind. Used when validating that
    /// the signed `manifest_path` really is the file the runtime will
    /// read for capability derivation.
    pub fn manifest_file(self) -> &'static str {
        match self {
            Self::App => "app.json",
            Self::Skill => "SKILL.md",
            Self::Mcp => "agent-api.json",
        }
    }
}

/// Node type in the signed file tree. Only regular files and
/// directories may appear — symlinks, hardlinks, devices, FIFOs and
/// sockets are rejected at parse time and again at verify time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeKind {
    File,
    Dir,
}

impl NodeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Dir => "dir",
        }
    }
}

/// One node of the signed tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    /// Slash-separated path relative to the package root. Normalized:
    /// no leading `/`, no `.`/`..`, no backslashes, no empty segments.
    pub path: String,
    #[serde(rename = "type")]
    pub kind: NodeKind,
    /// Permission bits only (`0o7777`). Security relevant because the
    /// executable bit decides whether a file can be run directly.
    pub mode: u32,
    /// Size in bytes. Always `0` for directories.
    pub size: u64,
    /// `sha256:<64 lowercase hex>` for files, empty string for dirs.
    pub digest: String,
}

/// The signed body of the envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageBody {
    pub kind: PackageKind,
    pub id: String,
    pub version: String,
    /// Schema string of the *inner* manifest (`cos.app-manifest/v1`,
    /// `claw.agent-api/v1`, `agentskills.io/skill-md/v1`, …). Bound so
    /// a manifest can never be reinterpreted under a different schema.
    pub manifest_schema: String,
    /// Path of the inner manifest inside the package.
    pub manifest_path: String,
    /// Executables/scripts the runtime may launch, and resources it
    /// may disclose. Both must also appear in `files`.
    #[serde(default)]
    pub entrypoints: Vec<String>,
    #[serde(default)]
    pub resources: Vec<String>,
    pub files: Vec<FileEntry>,
    /// `sha256:<hex>` over [`content_digest_bytes`].
    pub content_digest: String,
}

/// Signature block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvelopeSignature {
    pub algorithm: String,
    /// `sha256:<hex>` of the raw 32-byte public key. A key id is a
    /// binding to the key material, never an operator-chosen alias.
    pub key_id: String,
    /// Lowercase hex of the 32-byte Ed25519 verifying key.
    pub public_key: String,
    /// Lowercase hex of the 64-byte signature over [`canonical_bytes`].
    pub value: String,
}

/// Full on-disk envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub schema: String,
    pub package: PackageBody,
    pub signature: EnvelopeSignature,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EnvelopeError {
    #[error("envelope is {len} bytes; exceeds cap {cap}")]
    TooLarge { len: usize, cap: usize },
    #[error("envelope is not valid JSON: {0}")]
    Malformed(String),
    #[error("unsupported provenance schema `{0}` (expected `{SCHEMA_V1}`)")]
    UnsupportedSchema(String),
    #[error("unsupported signature algorithm `{0}` (expected `{ALG_ED25519}`)")]
    UnsupportedAlgorithm(String),
    #[error("field `{field}` is invalid: {reason}")]
    InvalidField { field: &'static str, reason: String },
    #[error("file tree is invalid at `{path}`: {reason}")]
    InvalidTree { path: String, reason: String },
    #[error("declared content_digest does not match the signed file tree")]
    ContentDigestMismatch,
    #[error(
        "key_id `{declared}` is not the digest of the supplied public key (expected `{computed}`)"
    )]
    KeyIdMismatch { declared: String, computed: String },
}

/// Compute the canonical key id for a raw Ed25519 verifying key.
///
/// The id is a digest of the key material, so two trust entries can
/// never claim the same id while carrying different keys, and a
/// package cannot point at a trusted id while shipping a different
/// public key.
pub fn key_id_for(public_key: &[u8; 32]) -> String {
    let mut h = Sha256Stream::new();
    h.update(b"claw-provenance/v1\x00publisher-key\x00");
    h.update(public_key);
    format!("sha256:{}", h.finalize_hex())
}

pub(crate) fn is_lower_hex(s: &str, bytes: usize) -> bool {
    s.len() == bytes * 2
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(crate) fn is_sha256_ref(s: &str) -> bool {
    match s.strip_prefix("sha256:") {
        Some(rest) => is_lower_hex(rest, 32),
        None => false,
    }
}

/// Validate one tree path. Rejects absolute paths, traversal, alternate
/// separators, empty/dot segments, over-long names and over-deep trees.
pub fn validate_tree_path(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("empty path".to_string());
    }
    if path.len() > MAX_PATH_BYTES {
        return Err(format!("path exceeds {MAX_PATH_BYTES} bytes"));
    }
    if !path.is_ascii() {
        // Non-ASCII names invite Unicode-normalisation and homoglyph
        // collisions between the signed tree and the filesystem.
        return Err("path must be ASCII".to_string());
    }
    if path.starts_with('/') {
        return Err("absolute paths are not allowed".to_string());
    }
    if path.contains('\\') {
        return Err("backslash separators are not allowed".to_string());
    }
    if path.contains('\0') {
        return Err("NUL byte in path".to_string());
    }
    if path.ends_with('/') {
        return Err("trailing separator".to_string());
    }
    let segments: Vec<&str> = path.split('/').collect();
    if segments.len() > MAX_PATH_DEPTH {
        return Err(format!("path depth exceeds {MAX_PATH_DEPTH}"));
    }
    for segment in segments {
        if segment.is_empty() {
            return Err("empty path segment".to_string());
        }
        if segment == "." || segment == ".." {
            return Err("`.`/`..` segments are not allowed".to_string());
        }
        if segment.len() > MAX_NAME_BYTES {
            return Err(format!("segment exceeds {MAX_NAME_BYTES} bytes"));
        }
        if segment.bytes().any(|b| b < 0x20 || b == 0x7f) {
            return Err("control character in path segment".to_string());
        }
        if segment.ends_with(' ') || segment.ends_with('.') {
            // Trailing space/dot names collide on case-insensitive and
            // Windows-compatible filesystems.
            return Err("segment ends with `.` or space".to_string());
        }
    }
    Ok(())
}

/// Length-prefixed, domain-separated canonical encoder.
///
/// Each field is written as
/// `u32le(key.len()) || key || u64le(value.len()) || value`, so the
/// concatenation is injective: no value can be re-parsed as a
/// different field boundary.
struct Canon {
    out: Vec<u8>,
}

impl Canon {
    fn new(domain: &[u8]) -> Self {
        let mut out = Vec::with_capacity(4096);
        out.extend_from_slice(&(domain.len() as u64).to_le_bytes());
        out.extend_from_slice(domain);
        Self { out }
    }

    fn text(&mut self, key: &str, value: &str) -> &mut Self {
        self.bytes(key, value.as_bytes())
    }

    fn bytes(&mut self, key: &str, value: &[u8]) -> &mut Self {
        self.out
            .extend_from_slice(&(key.len() as u32).to_le_bytes());
        self.out.extend_from_slice(key.as_bytes());
        self.out
            .extend_from_slice(&(value.len() as u64).to_le_bytes());
        self.out.extend_from_slice(value);
        self
    }

    fn uint(&mut self, key: &str, value: u64) -> &mut Self {
        self.bytes(key, &value.to_le_bytes())
    }

    fn finish(self) -> Vec<u8> {
        self.out
    }
}

/// Canonical bytes covering only the file tree. Hashed into
/// `package.content_digest` so an artifact can be referenced by a
/// single stable digest (used for revocation and rollback pinning).
pub fn content_digest_bytes(files: &[FileEntry]) -> Vec<u8> {
    let mut c = Canon::new(b"claw-provenance/v1\x00content-tree\x00");
    c.uint("count", files.len() as u64);
    for f in files {
        c.text("path", &f.path)
            .text("type", f.kind.as_str())
            .uint("mode", u64::from(f.mode & 0o7777))
            .uint("size", f.size)
            .text("digest", &f.digest);
    }
    c.finish()
}

/// `sha256:<hex>` over [`content_digest_bytes`].
pub fn content_digest(files: &[FileEntry]) -> String {
    let mut h = Sha256Stream::new();
    h.update(&content_digest_bytes(files));
    format!("sha256:{}", h.finalize_hex())
}

/// Deterministic signing bytes for an envelope. Covers the publisher
/// key material and every signed field, including the algorithm and
/// schema strings, so downgrade/confusion attacks change the message.
pub fn canonical_bytes(
    body: &PackageBody,
    algorithm: &str,
    key_id: &str,
    public_key: &str,
) -> Vec<u8> {
    let mut c = Canon::new(CANON_DOMAIN);
    c.text("schema", SCHEMA_V1)
        .text("algorithm", algorithm)
        .text("key_id", key_id)
        .text("public_key", public_key)
        .text("kind", body.kind.as_str())
        .text("id", &body.id)
        .text("version", &body.version)
        .text("manifest_schema", &body.manifest_schema)
        .text("manifest_path", &body.manifest_path);
    c.uint("entrypoints", body.entrypoints.len() as u64);
    for e in &body.entrypoints {
        c.text("entrypoint", e);
    }
    c.uint("resources", body.resources.len() as u64);
    for r in &body.resources {
        c.text("resource", r);
    }
    c.bytes("tree", &content_digest_bytes(&body.files));
    c.text("content_digest", &body.content_digest);
    c.finish()
}

impl Envelope {
    /// Parse and structurally validate an envelope. This never touches
    /// the filesystem and never verifies the signature — see
    /// [`crate::provenance::verify`].
    pub fn parse(raw: &str) -> Result<Self, EnvelopeError> {
        if raw.len() > MAX_ENVELOPE_BYTES {
            return Err(EnvelopeError::TooLarge {
                len: raw.len(),
                cap: MAX_ENVELOPE_BYTES,
            });
        }
        let envelope: Envelope =
            serde_json::from_str(raw).map_err(|e| EnvelopeError::Malformed(e.to_string()))?;
        envelope.validate()?;
        Ok(envelope)
    }

    pub fn validate(&self) -> Result<(), EnvelopeError> {
        if self.schema != SCHEMA_V1 {
            return Err(EnvelopeError::UnsupportedSchema(self.schema.clone()));
        }
        if self.signature.algorithm != ALG_ED25519 {
            // Deliberately case-sensitive: `ED25519` is not a value we
            // ever emit, so accepting it would widen the accepted set
            // for no benefit.
            return Err(EnvelopeError::UnsupportedAlgorithm(
                self.signature.algorithm.clone(),
            ));
        }
        if !is_lower_hex(&self.signature.public_key, 32) {
            return Err(EnvelopeError::InvalidField {
                field: "signature.public_key",
                reason: "expected 64 lowercase hex characters".to_string(),
            });
        }
        if !is_lower_hex(&self.signature.value, 64) {
            return Err(EnvelopeError::InvalidField {
                field: "signature.value",
                reason: "expected 128 lowercase hex characters".to_string(),
            });
        }
        if !is_sha256_ref(&self.signature.key_id) {
            return Err(EnvelopeError::InvalidField {
                field: "signature.key_id",
                reason: "expected `sha256:<64 hex>`".to_string(),
            });
        }
        let pk = self.public_key_bytes()?;
        let computed = key_id_for(&pk);
        if computed != self.signature.key_id {
            return Err(EnvelopeError::KeyIdMismatch {
                declared: self.signature.key_id.clone(),
                computed,
            });
        }
        self.package.validate()
    }

    pub fn public_key_bytes(&self) -> Result<[u8; 32], EnvelopeError> {
        let bytes =
            hex::decode(&self.signature.public_key).map_err(|e| EnvelopeError::InvalidField {
                field: "signature.public_key",
                reason: e.to_string(),
            })?;
        bytes
            .as_slice()
            .try_into()
            .map_err(|_| EnvelopeError::InvalidField {
                field: "signature.public_key",
                reason: "expected 32 bytes".to_string(),
            })
    }

    pub fn signature_bytes(&self) -> Result<[u8; 64], EnvelopeError> {
        let bytes =
            hex::decode(&self.signature.value).map_err(|e| EnvelopeError::InvalidField {
                field: "signature.value",
                reason: e.to_string(),
            })?;
        bytes
            .as_slice()
            .try_into()
            .map_err(|_| EnvelopeError::InvalidField {
                field: "signature.value",
                reason: "expected 64 bytes".to_string(),
            })
    }

    /// The exact bytes this envelope's signature must cover.
    pub fn signing_bytes(&self) -> Vec<u8> {
        canonical_bytes(
            &self.package,
            &self.signature.algorithm,
            &self.signature.key_id,
            &self.signature.public_key,
        )
    }
}

fn validate_ident(field: &'static str, value: &str) -> Result<(), EnvelopeError> {
    if value.is_empty() || value.len() > MAX_NAME_BYTES {
        return Err(EnvelopeError::InvalidField {
            field,
            reason: format!("length must be 1..={MAX_NAME_BYTES}"),
        });
    }
    if !value.bytes().all(|b| {
        b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.' | b'+')
    }) {
        return Err(EnvelopeError::InvalidField {
            field,
            reason: "only [a-z0-9-_.+] are allowed".to_string(),
        });
    }
    Ok(())
}

impl PackageBody {
    pub fn validate(&self) -> Result<(), EnvelopeError> {
        validate_ident("package.id", &self.id)?;
        if self.version.is_empty() || self.version.len() > MAX_NAME_BYTES {
            return Err(EnvelopeError::InvalidField {
                field: "package.version",
                reason: format!("length must be 1..={MAX_NAME_BYTES}"),
            });
        }
        if self.manifest_schema.is_empty() || self.manifest_schema.len() > MAX_NAME_BYTES {
            return Err(EnvelopeError::InvalidField {
                field: "package.manifest_schema",
                reason: format!("length must be 1..={MAX_NAME_BYTES}"),
            });
        }
        if self.files.len() > MAX_FILES {
            return Err(EnvelopeError::InvalidField {
                field: "package.files",
                reason: format!("more than {MAX_FILES} entries"),
            });
        }
        if self.entrypoints.len() > MAX_ENTRYPOINTS {
            return Err(EnvelopeError::InvalidField {
                field: "package.entrypoints",
                reason: format!("more than {MAX_ENTRYPOINTS} entries"),
            });
        }

        // Tree must be sorted, unique, and free of case-collisions.
        // Sorting is part of the format so canonical bytes cannot be
        // permuted into a second valid encoding of the same tree.
        let mut previous: Option<&str> = None;
        let mut folded: BTreeMap<String, &str> = BTreeMap::new();
        for entry in &self.files {
            validate_tree_path(&entry.path).map_err(|reason| EnvelopeError::InvalidTree {
                path: entry.path.clone(),
                reason,
            })?;
            if entry.path == ENVELOPE_FILE {
                return Err(EnvelopeError::InvalidTree {
                    path: entry.path.clone(),
                    reason: "the envelope cannot describe itself".to_string(),
                });
            }
            if let Some(prev) = previous {
                match prev.cmp(entry.path.as_str()) {
                    std::cmp::Ordering::Less => {}
                    std::cmp::Ordering::Equal => {
                        return Err(EnvelopeError::InvalidTree {
                            path: entry.path.clone(),
                            reason: "duplicate path".to_string(),
                        })
                    }
                    std::cmp::Ordering::Greater => {
                        return Err(EnvelopeError::InvalidTree {
                            path: entry.path.clone(),
                            reason: "file tree must be sorted by path".to_string(),
                        })
                    }
                }
            }
            previous = Some(&entry.path);

            let key = entry.path.to_ascii_lowercase();
            if let Some(other) = folded.insert(key, &entry.path) {
                if other != entry.path {
                    return Err(EnvelopeError::InvalidTree {
                        path: entry.path.clone(),
                        reason: format!("case-collides with `{other}`"),
                    });
                }
            }

            if entry.mode & !0o7777 != 0 {
                return Err(EnvelopeError::InvalidTree {
                    path: entry.path.clone(),
                    reason: "mode carries bits outside 0o7777".to_string(),
                });
            }
            if entry.mode & 0o022 != 0 {
                return Err(EnvelopeError::InvalidTree {
                    path: entry.path.clone(),
                    reason: "group/world-writable mode is not allowed".to_string(),
                });
            }
            match entry.kind {
                NodeKind::Dir => {
                    if entry.size != 0 || !entry.digest.is_empty() {
                        return Err(EnvelopeError::InvalidTree {
                            path: entry.path.clone(),
                            reason: "directories must have size 0 and empty digest".to_string(),
                        });
                    }
                }
                NodeKind::File => {
                    if !is_sha256_ref(&entry.digest) {
                        return Err(EnvelopeError::InvalidTree {
                            path: entry.path.clone(),
                            reason: "expected `sha256:<64 hex>` digest".to_string(),
                        });
                    }
                }
            }
        }

        // Every path must have its parent directory declared, so the
        // verifier can reject any directory that is not in the tree.
        let declared: BTreeMap<&str, NodeKind> = self
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.kind))
            .collect();
        for entry in &self.files {
            if let Some((parent, _)) = entry.path.rsplit_once('/') {
                match declared.get(parent) {
                    Some(NodeKind::Dir) => {}
                    Some(NodeKind::File) => {
                        return Err(EnvelopeError::InvalidTree {
                            path: entry.path.clone(),
                            reason: format!("parent `{parent}` is declared as a file"),
                        })
                    }
                    None => {
                        return Err(EnvelopeError::InvalidTree {
                            path: entry.path.clone(),
                            reason: format!("parent directory `{parent}` is not declared"),
                        })
                    }
                }
            }
        }

        let require_file = |field: &'static str, path: &str| -> Result<(), EnvelopeError> {
            validate_tree_path(path).map_err(|reason| EnvelopeError::InvalidField {
                field,
                reason: format!("{path}: {reason}"),
            })?;
            match declared.get(path) {
                Some(NodeKind::File) => Ok(()),
                _ => Err(EnvelopeError::InvalidField {
                    field,
                    reason: format!("`{path}` is not a signed regular file"),
                }),
            }
        };

        require_file("package.manifest_path", &self.manifest_path)?;
        for e in &self.entrypoints {
            require_file("package.entrypoints", e)?;
        }
        for r in &self.resources {
            require_file("package.resources", r)?;
        }

        if content_digest(&self.files) != self.content_digest {
            return Err(EnvelopeError::ContentDigestMismatch);
        }
        Ok(())
    }

    /// Look up a signed node by tree path.
    pub fn entry(&self, path: &str) -> Option<&FileEntry> {
        self.files.iter().find(|f| f.path == path)
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/provenance/envelope.rs"
    ));
}
