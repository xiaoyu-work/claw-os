//! Package verification: the single gate every extension kind passes
//! through before its manifest, operations, capability needs, tool
//! schemas or model-visible metadata are trusted.
//!
//! ## Three ways a package can be trusted
//!
//! 1. **Publisher signature** — the package carries a
//!    `claw.provenance/v1` envelope signed by a trusted, non-revoked
//!    key. This is the only route for user-installed content.
//! 2. **Vendor (package-manager) trust** — the package sits under an
//!    approved, root-owned system root that no unprivileged user can
//!    modify. The tree digest is still computed and pinned so
//!    post-install tampering is detected and audited before use.
//! 3. **Developer trust** — an explicit, persisted
//!    [`crate::provenance::trust::DevGrant`] for one unsigned tree at
//!    one content digest. Developer-trusted packages carry a
//!    materially restricted ceiling and never reach privileged routes.
//!
//! ## Verify-then-use
//!
//! Verification returns a [`VerifiedPackage`] that owns the package's
//! directory descriptor. Every later read goes through `openat` on
//! that descriptor and re-checks the digest, so replacing a file (or
//! the whole directory) between verification and launch/disclosure is
//! detected rather than silently honoured.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use ed25519_dalek::{Signature, VerifyingKey};

use super::envelope::{
    content_digest, Envelope, EnvelopeError, FileEntry, NodeKind, PackageKind, ENVELOPE_FILE,
};
use super::fsec::{self, DirHandle, NodeMeta};
use super::trust::{TrustError, TrustStore, TrustTier};

/// Approved system roots whose contents inherit Debian/rootfs package
/// trust. Anything outside this list — including a development
/// checkout reached through an environment override — must present a
/// signature or a developer grant.
pub const VENDOR_PACKAGE_ROOTS: &[&str] = &[
    "/usr/lib/cos",
    "/usr/share/cos",
    "/usr/lib/claw",
    "/usr/share/claw",
];

/// Total bytes hashed for one package.
pub const MAX_PACKAGE_BYTES: u64 = 256 * 1024 * 1024;
/// Bytes read for one verified file (manifests, skill bodies, …).
pub const MAX_VERIFIED_READ_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ProvenanceError {
    #[error("{kind} package at {path} has no {ENVELOPE_FILE} provenance envelope")]
    MissingEnvelope { kind: &'static str, path: PathBuf },
    #[error("provenance envelope at {path} is invalid: {source}")]
    Envelope {
        path: PathBuf,
        #[source]
        source: EnvelopeError,
    },
    #[error("provenance signature for {path} did not verify")]
    BadSignature { path: PathBuf },
    #[error("{path}: {source}")]
    Trust {
        path: PathBuf,
        #[source]
        source: TrustError,
    },
    #[error("{path}: package content does not match its signed file tree: {reason}")]
    TreeMismatch { path: PathBuf, reason: String },
    #[error("{path}: expected a `{expected}` package but the envelope declares `{actual}`")]
    KindMismatch {
        path: PathBuf,
        expected: &'static str,
        actual: &'static str,
    },
    #[error("{path}: expected package id `{expected}` but the envelope declares `{actual}`")]
    IdMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    #[error("{path}: {reason}")]
    Io { path: PathBuf, reason: String },
    #[error("{path}: developer trust grant is stale (tree digest changed); re-run `cos provenance dev-trust`")]
    StaleDeveloperGrant { path: PathBuf },
    #[error("{path}: unsigned package and no developer trust grant; install a signed package or run `cos provenance dev-trust --kind {kind} --id {id} --path {path}`")]
    Unsigned {
        path: PathBuf,
        kind: &'static str,
        id: String,
    },
    #[error("{0}")]
    Unsupported(String),
}

impl ProvenanceError {
    /// Stable error code for structured CLI/bridge errors.
    pub fn code(&self) -> &'static str {
        match self {
            Self::MissingEnvelope { .. } | Self::Unsigned { .. } => "provenance.unsigned",
            Self::Envelope { .. } => "provenance.envelope_invalid",
            Self::BadSignature { .. } => "provenance.signature_rejected",
            Self::Trust { .. } => "provenance.untrusted_key",
            Self::TreeMismatch { .. } => "provenance.content_mismatch",
            Self::KindMismatch { .. } | Self::IdMismatch { .. } => "provenance.identity_mismatch",
            Self::Io { .. } => "io.provenance_read",
            Self::StaleDeveloperGrant { .. } => "provenance.developer_grant_stale",
            Self::Unsupported(_) => "provenance.unsupported",
        }
    }
}

/// How a verified package earned its trust.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustSource {
    /// Signed by a trusted publisher key.
    Publisher { key_id: String },
    /// Root-owned content under an approved system package root.
    Vendor,
    /// Explicit local developer grant over unsigned content.
    Developer,
}

impl TrustSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Publisher { .. } => "publisher",
            Self::Vendor => "vendor",
            Self::Developer => "developer",
        }
    }
}

/// A package whose provenance and content have been authenticated.
///
/// Holding this value means "these exact bytes were verified"; every
/// accessor re-derives content from the pinned directory descriptor.
#[derive(Debug)]
pub struct VerifiedPackage {
    kind: PackageKind,
    id: String,
    version: String,
    dir: PathBuf,
    content_digest: String,
    manifest_path: String,
    entrypoints: Vec<String>,
    resources: Vec<String>,
    files: BTreeMap<String, FileEntry>,
    source: TrustSource,
    tier: TrustTier,
    trust_generation: String,
    handle: DirHandle,
    identity: (u64, u64),
}

impl VerifiedPackage {
    pub fn kind(&self) -> PackageKind {
        self.kind
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn dir(&self) -> &Path {
        &self.dir
    }
    pub(crate) fn dir_identity(&self) -> (u64, u64) {
        self.identity
    }
    pub fn content_digest(&self) -> &str {
        &self.content_digest
    }
    pub fn manifest_path(&self) -> &str {
        &self.manifest_path
    }
    pub fn entrypoints(&self) -> &[String] {
        &self.entrypoints
    }
    pub fn resources(&self) -> &[String] {
        &self.resources
    }
    pub fn source(&self) -> &TrustSource {
        &self.source
    }
    pub fn tier(&self) -> TrustTier {
        self.tier
    }
    pub fn trust_generation(&self) -> &str {
        &self.trust_generation
    }
    pub fn files(&self) -> impl Iterator<Item = &FileEntry> {
        self.files.values()
    }

    /// Short digest suitable for log lines and directory names.
    pub fn short_digest(&self) -> String {
        self.content_digest
            .strip_prefix("sha256:")
            .unwrap_or(&self.content_digest)
            .chars()
            .take(16)
            .collect()
    }

    /// Audit-safe projection. Never contains key material, file
    /// contents or model-visible text.
    pub fn audit_facts(&self) -> serde_json::Value {
        let key_id = match &self.source {
            TrustSource::Publisher { key_id } => Some(key_id.as_str()),
            _ => None,
        };
        serde_json::json!({
            "kind": self.kind.as_str(),
            "id": self.id,
            "version": self.version,
            "content_digest": self.content_digest,
            "trust": self.source.as_str(),
            "tier": self.tier.as_str(),
            "publisher_key_id": key_id,
            "files": self.files.len(),
        })
    }

    /// Read a signed file from the pinned directory and re-check its
    /// digest. Fails when the file changed, disappeared, became a
    /// symlink, or was never part of the signed tree.
    pub fn read_verified(&self, rel: &str) -> Result<Vec<u8>, ProvenanceError> {
        let entry = self
            .files
            .get(rel)
            .filter(|e| e.kind == NodeKind::File)
            .ok_or_else(|| ProvenanceError::TreeMismatch {
                path: self.dir.join(rel),
                reason: format!("`{rel}` is not a signed regular file"),
            })?;
        let fd = self
            .handle
            .open_file(rel)
            .map_err(|e| ProvenanceError::TreeMismatch {
                path: self.dir.join(rel),
                reason: format!("re-open failed: {e}"),
            })?;
        let meta = fd.meta();
        if meta.nlink != 1 {
            return Err(ProvenanceError::TreeMismatch {
                path: self.dir.join(rel),
                reason: format!("has {} hard links", meta.nlink),
            });
        }
        let bytes = fd.read_bounded(MAX_VERIFIED_READ_BYTES).map_err(|e| {
            ProvenanceError::TreeMismatch {
                path: self.dir.join(rel),
                reason: e.to_string(),
            }
        })?;
        if bytes.len() as u64 != entry.size {
            return Err(ProvenanceError::TreeMismatch {
                path: self.dir.join(rel),
                reason: format!("size changed to {} (signed {})", bytes.len(), entry.size),
            });
        }
        let digest = digest_bytes(&bytes);
        if digest != entry.digest {
            return Err(ProvenanceError::TreeMismatch {
                path: self.dir.join(rel),
                reason: "content digest changed after verification".to_string(),
            });
        }
        Ok(bytes)
    }

    /// UTF-8 flavour of [`Self::read_verified`]. Disclosure paths use
    /// this so invalid UTF-8 can never reach prompt assembly.
    pub fn read_verified_text(&self, rel: &str) -> Result<String, ProvenanceError> {
        let bytes = self.read_verified(rel)?;
        String::from_utf8(bytes).map_err(|_| ProvenanceError::TreeMismatch {
            path: self.dir.join(rel),
            reason: "file is not valid UTF-8".to_string(),
        })
    }

    /// The manifest bytes used for capability derivation and identity.
    pub fn manifest_text(&self) -> Result<String, ProvenanceError> {
        self.read_verified_text(&self.manifest_path.clone())
    }

    /// Open a signed entrypoint and re-check its digest, returning a
    /// descriptor plus the `/proc/self/fd/N` path the sandbox accepts
    /// as a pinned program source. A mutable interpreter found on
    /// `PATH` can never be substituted for this inode.
    #[cfg(unix)]
    pub fn open_entrypoint(&self, rel: &str) -> Result<fsec::VerifiedFd, ProvenanceError> {
        // The manifest is implicitly bindable: it is signed, and the
        // runtime must be able to pin the exact bytes that drove the
        // capability decision.
        if rel != self.manifest_path && !self.entrypoints.iter().any(|e| e == rel) {
            return Err(ProvenanceError::TreeMismatch {
                path: self.dir.join(rel),
                reason: format!("`{rel}` is not a signed entrypoint"),
            });
        }
        let entry = self
            .files
            .get(rel)
            .ok_or_else(|| ProvenanceError::TreeMismatch {
                path: self.dir.join(rel),
                reason: "entrypoint missing from the signed tree".to_string(),
            })?;
        let fd = self
            .handle
            .open_file(rel)
            .map_err(|e| ProvenanceError::TreeMismatch {
                path: self.dir.join(rel),
                reason: format!("re-open failed: {e}"),
            })?;
        let digest = digest_fd(&fd).map_err(|e| ProvenanceError::TreeMismatch {
            path: self.dir.join(rel),
            reason: e,
        })?;
        if digest != entry.digest {
            return Err(ProvenanceError::TreeMismatch {
                path: self.dir.join(rel),
                reason: "entrypoint changed after verification".to_string(),
            });
        }
        Ok(fd)
    }

    /// `(st_dev, st_ino)` of the verified package directory.
    pub fn identity(&self) -> (u64, u64) {
        self.identity
    }

    /// The capability ceiling this package's tier and identity imply.
    pub fn ceiling(&self) -> super::Ceiling {
        super::Ceiling::for_package(self.tier, self.id.clone())
    }

    /// Bind this package for execution.
    ///
    /// Re-hashes the manifest and every requested entrypoint from the
    /// pinned directory descriptor, keeps those descriptors open, and
    /// returns the inode identities the sandbox must bind. Holding the
    /// binding across `prepare()` and `spawn()` is what makes a
    /// post-verification swap of `main.py` either detected (identity
    /// mismatch when the provider pins the source) or irrelevant (the
    /// descriptor still names the verified inode).
    #[cfg(unix)]
    pub fn bind_for_launch(
        &self,
        entrypoints: &[String],
    ) -> Result<LaunchBinding, ProvenanceError> {
        let mut held = Vec::new();
        let mut identities = Vec::new();
        for rel in entrypoints {
            let fd = self.open_entrypoint(rel)?;
            let meta = fd.meta();
            identities.push((rel.clone(), (meta.dev, meta.ino)));
            held.push(fd);
        }
        Ok(LaunchBinding {
            dir: self.dir.clone(),
            dir_identity: self.identity,
            entries: identities,
            tier: self.tier,
            package: super::runtime::PackageRef::of(self),
            _held: held,
        })
    }

    #[cfg(unix)]
    pub fn materialize_snapshot(&self, destination: &Path) -> Result<(), ProvenanceError> {
        use std::fs;
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        fs::create_dir(destination).map_err(|error| ProvenanceError::Io {
            path: destination.to_path_buf(),
            reason: error.to_string(),
        })?;
        let mut directories = Vec::new();
        for entry in self.files.values() {
            let target = destination.join(&entry.path);
            match entry.kind {
                NodeKind::Dir => {
                    fs::create_dir_all(&target).map_err(|error| ProvenanceError::Io {
                        path: target.clone(),
                        reason: error.to_string(),
                    })?;
                    directories.push((target, entry.mode));
                }
                NodeKind::File => {
                    if let Some(parent) = target.parent() {
                        fs::create_dir_all(parent).map_err(|error| ProvenanceError::Io {
                            path: parent.to_path_buf(),
                            reason: error.to_string(),
                        })?;
                    }
                    let source = self.handle.open_file(&entry.path).map_err(|error| {
                        ProvenanceError::TreeMismatch {
                            path: self.dir.join(&entry.path),
                            reason: format!("snapshot open failed: {error}"),
                        }
                    })?;
                    let bytes = source.read_bounded(MAX_PACKAGE_BYTES).map_err(|error| {
                        ProvenanceError::Io {
                            path: self.dir.join(&entry.path),
                            reason: error.to_string(),
                        }
                    })?;
                    if digest_bytes(&bytes) != entry.digest {
                        return Err(ProvenanceError::TreeMismatch {
                            path: self.dir.join(&entry.path),
                            reason: "file changed while materializing verified snapshot"
                                .to_string(),
                        });
                    }
                    let mut output = fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&target)
                        .map_err(|error| ProvenanceError::Io {
                            path: target.clone(),
                            reason: error.to_string(),
                        })?;
                    output
                        .write_all(&bytes)
                        .and_then(|_| output.sync_all())
                        .map_err(|error| ProvenanceError::Io {
                            path: target.clone(),
                            reason: error.to_string(),
                        })?;
                    fs::set_permissions(&target, fs::Permissions::from_mode(entry.mode & 0o555))
                        .map_err(|error| ProvenanceError::Io {
                            path: target,
                            reason: error.to_string(),
                        })?;
                }
            }
        }
        directories.sort_by_key(|(path, _)| std::cmp::Reverse(path.components().count()));
        for (path, mode) in directories {
            fs::set_permissions(&path, fs::Permissions::from_mode(mode & 0o555)).map_err(
                |error| ProvenanceError::Io {
                    path,
                    reason: error.to_string(),
                },
            )?;
        }
        fs::set_permissions(destination, fs::Permissions::from_mode(0o555)).map_err(|error| {
            ProvenanceError::Io {
                path: destination.to_path_buf(),
                reason: error.to_string(),
            }
        })?;
        Ok(())
    }

    /// Re-assert that the pinned directory is still the one that was
    /// verified and that the trust store has not moved on. Callers
    /// invoke this immediately before launching or disclosing.
    pub fn assert_current(&self, trust: &TrustStore) -> Result<(), ProvenanceError> {
        if trust.generation() != self.trust_generation {
            return Err(ProvenanceError::TreeMismatch {
                path: self.dir.clone(),
                reason: "trust store changed since verification".to_string(),
            });
        }
        if trust.is_package_revoked(&self.content_digest) {
            return Err(ProvenanceError::Trust {
                path: self.dir.clone(),
                source: TrustError::RevokedPackage(self.content_digest.clone()),
            });
        }
        if let Err(reason) = super::runtime::PackageRef::of(self).is_live(trust) {
            return Err(ProvenanceError::TreeMismatch {
                path: self.dir.clone(),
                reason,
            });
        }
        let meta = fsec::lstat(&self.dir).map_err(|e| ProvenanceError::Io {
            path: self.dir.clone(),
            reason: e.to_string(),
        })?;
        if (meta.dev, meta.ino) != self.identity {
            return Err(ProvenanceError::TreeMismatch {
                path: self.dir.clone(),
                reason: "package directory was replaced after verification".to_string(),
            });
        }
        Ok(())
    }
}

/// Read one already-scanned file for manifest inspection. Best effort:
/// a package whose manifest cannot be read simply declares no
/// entrypoints beyond the manifest path itself.
fn read_scanned(handle: &DirHandle, rel: &str) -> Option<String> {
    let fd = handle.open_file(rel).ok()?;
    let bytes = fd.read_bounded(MAX_VERIFIED_READ_BYTES).ok()?;
    String::from_utf8(bytes).ok()
}

/// Keep only declared entries that exist as regular files in the tree.
fn restrict_to_present(declared: Vec<String>, files: &[FileEntry]) -> Vec<String> {
    declared
        .into_iter()
        .filter(|rel| {
            files
                .iter()
                .any(|f| f.path == *rel && f.kind == NodeKind::File)
        })
        .collect()
}

/// Open descriptors on a verified package's executable surface, plus
/// the inode identities the sandbox must bind.
///
/// Dropping this releases the descriptors, so callers keep it alive
/// until the child has been spawned.
#[cfg(unix)]
#[derive(Debug)]
pub struct LaunchBinding {
    dir: PathBuf,
    dir_identity: (u64, u64),
    entries: Vec<(String, (u64, u64))>,
    tier: TrustTier,
    package: super::runtime::PackageRef,
    _held: Vec<fsec::VerifiedFd>,
}

#[cfg(unix)]
impl LaunchBinding {
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn dir_identity(&self) -> (u64, u64) {
        self.dir_identity
    }

    /// The trust tier of the snapshot this binding pins.
    ///
    /// Carried on the binding rather than looked up again so the
    /// sandbox shape, the capability ceiling and the executed bytes all
    /// come from one verification.
    pub fn tier(&self) -> TrustTier {
        self.tier
    }

    pub fn ceiling(&self) -> super::Ceiling {
        super::Ceiling::for_tier(self.tier)
    }

    /// The revocable identity of the snapshot this binding pins.
    ///
    /// Carried alongside the pinned inodes so the launch's broker
    /// endpoint checks the same package the sandbox is executing,
    /// without re-reading anything mutable.
    pub fn package_ref(&self) -> super::runtime::PackageRef {
        self.package.clone()
    }

    /// Absolute host path plus required inode identity for each bound
    /// entrypoint.
    pub fn entries(&self) -> Vec<(PathBuf, (u64, u64))> {
        self.entries
            .iter()
            .map(|(rel, id)| (self.dir.join(rel), *id))
            .collect()
    }

    /// The verified identity for one absolute path, when it belongs to
    /// this binding.
    pub fn identity_for(&self, path: &Path) -> Option<(u64, u64)> {
        if path == self.dir {
            return Some(self.dir_identity);
        }
        self.entries
            .iter()
            .find(|(rel, _)| self.dir.join(rel) == path)
            .map(|(_, id)| *id)
    }
}

fn digest_bytes(data: &[u8]) -> String {
    let mut h = crate::crypto::Sha256Stream::new();
    h.update(data);
    format!("sha256:{}", h.finalize_hex())
}

#[cfg(unix)]
fn digest_fd(fd: &fsec::VerifiedFd) -> Result<String, String> {
    let bytes = fd
        .read_bounded(MAX_PACKAGE_BYTES)
        .map_err(|e| e.to_string())?;
    Ok(digest_bytes(&bytes))
}

/// Options that let each extension kind apply its own constraints
/// without forking the verifier.
#[derive(Debug, Clone)]
pub struct VerifyOptions {
    pub kind: PackageKind,
    /// When set, the envelope's package id must match exactly.
    pub expect_id: Option<String>,
    /// Allow Debian/rootfs package trust for approved system roots.
    pub allow_vendor: bool,
    /// Allow an explicit developer grant to stand in for a signature.
    pub allow_developer: bool,
    pub max_bytes: u64,
}

impl VerifyOptions {
    pub fn new(kind: PackageKind) -> Self {
        Self {
            kind,
            expect_id: None,
            allow_vendor: true,
            allow_developer: true,
            max_bytes: MAX_PACKAGE_BYTES,
        }
    }

    pub fn expect_id(mut self, id: impl Into<String>) -> Self {
        self.expect_id = Some(id.into());
        self
    }

    /// Staging verification during install: the artifact lives in a
    /// private temp dir, so neither vendor nor developer trust applies.
    pub fn signature_only(mut self) -> Self {
        self.allow_vendor = false;
        self.allow_developer = false;
        self
    }
}

/// Verify one package directory.
pub fn verify_package(
    dir: &Path,
    options: &VerifyOptions,
    trust: &TrustStore,
) -> Result<VerifiedPackage, ProvenanceError> {
    let handle = DirHandle::open(dir).map_err(|e| ProvenanceError::Io {
        path: dir.to_path_buf(),
        reason: format!("open package directory: {e}"),
    })?;
    let dir_meta = fsec::lstat(dir).map_err(|e| ProvenanceError::Io {
        path: dir.to_path_buf(),
        reason: e.to_string(),
    })?;
    if !dir_meta.is_dir {
        return Err(ProvenanceError::Io {
            path: dir.to_path_buf(),
            reason: "not a directory".to_string(),
        });
    }
    let identity = (dir_meta.dev, dir_meta.ino);
    let canonical_dir = dir.canonicalize().map_err(|error| ProvenanceError::Io {
        path: dir.to_path_buf(),
        reason: format!("canonicalize package directory: {error}"),
    })?;
    let canonical_meta = fsec::lstat(&canonical_dir).map_err(|error| ProvenanceError::Io {
        path: canonical_dir.clone(),
        reason: error.to_string(),
    })?;
    if (canonical_meta.dev, canonical_meta.ino) != identity {
        return Err(ProvenanceError::Io {
            path: canonical_dir,
            reason: "canonical package directory identity does not match opened directory"
                .to_string(),
        });
    }

    let envelope_raw = match handle
        .open_file(ENVELOPE_FILE)
        .and_then(|fd| fd.read_bounded(super::envelope::MAX_ENVELOPE_BYTES as u64))
    {
        Ok(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(ProvenanceError::Io {
                path: dir.join(ENVELOPE_FILE),
                reason: e.to_string(),
            })
        }
    };

    let mut package = match envelope_raw {
        Some(raw) => verify_signed(dir, handle, identity, &raw, options, trust),
        None => verify_unsigned(dir, handle, identity, dir_meta, options, trust),
    }?;
    package.dir = canonical_dir;
    Ok(package)
}

fn verify_signed(
    dir: &Path,
    handle: DirHandle,
    identity: (u64, u64),
    raw: &str,
    options: &VerifyOptions,
    trust: &TrustStore,
) -> Result<VerifiedPackage, ProvenanceError> {
    let envelope = Envelope::parse(raw).map_err(|source| ProvenanceError::Envelope {
        path: dir.join(ENVELOPE_FILE),
        source,
    })?;
    let body = &envelope.package;
    if body.kind != options.kind {
        return Err(ProvenanceError::KindMismatch {
            path: dir.to_path_buf(),
            expected: options.kind.as_str(),
            actual: body.kind.as_str(),
        });
    }
    if let Some(expected) = &options.expect_id {
        if expected != &body.id {
            return Err(ProvenanceError::IdMismatch {
                path: dir.to_path_buf(),
                expected: expected.clone(),
                actual: body.id.clone(),
            });
        }
    }

    // Signature first: never spend I/O hashing a tree that no trusted
    // key vouches for.
    let public_key = envelope
        .public_key_bytes()
        .map_err(|source| ProvenanceError::Envelope {
            path: dir.join(ENVELOPE_FILE),
            source,
        })?;
    let signature = envelope
        .signature_bytes()
        .map_err(|source| ProvenanceError::Envelope {
            path: dir.join(ENVELOPE_FILE),
            source,
        })?;
    let key = trust
        .authorize(
            &envelope.signature.key_id,
            &public_key,
            body.kind,
            &body.content_digest,
            chrono::Utc::now(),
        )
        .map_err(|source| ProvenanceError::Trust {
            path: dir.to_path_buf(),
            source,
        })?;
    let verifying =
        VerifyingKey::from_bytes(&public_key).map_err(|_| ProvenanceError::BadSignature {
            path: dir.to_path_buf(),
        })?;
    verifying
        .verify_strict(
            &envelope.signing_bytes(),
            &Signature::from_bytes(&signature),
        )
        .map_err(|_| ProvenanceError::BadSignature {
            path: dir.to_path_buf(),
        })?;

    verify_tree(dir, &handle, &body.files, options.max_bytes)?;

    Ok(VerifiedPackage {
        kind: body.kind,
        id: body.id.clone(),
        version: body.version.clone(),
        dir: dir.to_path_buf(),
        content_digest: body.content_digest.clone(),
        manifest_path: body.manifest_path.clone(),
        entrypoints: body.entrypoints.clone(),
        resources: body.resources.clone(),
        files: body
            .files
            .iter()
            .map(|f| (f.path.clone(), f.clone()))
            .collect(),
        source: TrustSource::Publisher {
            key_id: key.key_id.clone(),
        },
        tier: key.tier,
        trust_generation: trust.generation().to_string(),
        handle,
        identity,
    })
}

fn verify_unsigned(
    dir: &Path,
    handle: DirHandle,
    identity: (u64, u64),
    dir_meta: NodeMeta,
    options: &VerifyOptions,
    trust: &TrustStore,
) -> Result<VerifiedPackage, ProvenanceError> {
    let manifest_rel = options.kind.manifest_file().to_string();
    let vendor = options.allow_vendor && is_vendor_root_path(dir);
    let id = options
        .expect_id
        .clone()
        .unwrap_or_else(|| default_id_for(dir));
    let grant = if options.allow_developer {
        trust.dev_grant(options.kind, &id).cloned()
    } else {
        None
    };
    if !vendor && grant.is_none() {
        // Nothing can vouch for this tree. Fail before hashing it so an
        // unsigned drop-in cannot turn discovery into an I/O amplifier.
        return Err(ProvenanceError::Unsigned {
            path: dir.to_path_buf(),
            kind: options.kind.as_str(),
            id,
        });
    }
    if vendor {
        // Every component must be root-owned and non-writable by
        // anyone else, otherwise "it lives under /usr" means nothing.
        fsec::require_secure_location(dir, &[0]).map_err(|e| ProvenanceError::Io {
            path: dir.to_path_buf(),
            reason: format!("vendor package root rejected: {e}"),
        })?;
    }

    let files = scan_tree(dir, &handle, options.max_bytes, vendor)?;
    let digest = content_digest(&files);

    if vendor {
        pin_vendor_digest(dir, options.kind, &id, &digest)?;
        let manifest_body = read_scanned(&handle, &manifest_rel);
        let entrypoints = restrict_to_present(
            declared_entrypoints(options.kind, &manifest_rel, manifest_body.as_deref()),
            &files,
        );
        return Ok(VerifiedPackage {
            kind: options.kind,
            id,
            version: "0".to_string(),
            dir: dir.to_path_buf(),
            content_digest: digest,
            manifest_path: manifest_rel,
            entrypoints,
            resources: Vec::new(),
            files: files.into_iter().map(|f| (f.path.clone(), f)).collect(),
            source: TrustSource::Vendor,
            tier: TrustTier::Vendor,
            trust_generation: trust.generation().to_string(),
            handle,
            identity,
        });
    }

    if options.allow_developer {
        if let Some(grant) = grant {
            let same_path = grant
                .path
                .canonicalize()
                .ok()
                .zip(dir.canonicalize().ok())
                .map(|(a, b)| a == b)
                .unwrap_or(false);
            if !same_path {
                return Err(ProvenanceError::StaleDeveloperGrant {
                    path: dir.to_path_buf(),
                });
            }
            if grant.content_digest != digest {
                return Err(ProvenanceError::StaleDeveloperGrant {
                    path: dir.to_path_buf(),
                });
            }
            if dir_meta.is_group_or_world_writable() {
                return Err(ProvenanceError::Io {
                    path: dir.to_path_buf(),
                    reason: "developer package directory is group- or world-writable".to_string(),
                });
            }
            let manifest_body = read_scanned(&handle, &manifest_rel);
            let entrypoints = restrict_to_present(
                declared_entrypoints(options.kind, &manifest_rel, manifest_body.as_deref()),
                &files,
            );
            return Ok(VerifiedPackage {
                kind: options.kind,
                id,
                version: "dev".to_string(),
                dir: dir.to_path_buf(),
                content_digest: digest,
                manifest_path: manifest_rel,
                entrypoints,
                resources: Vec::new(),
                files: files.into_iter().map(|f| (f.path.clone(), f)).collect(),
                source: TrustSource::Developer,
                tier: TrustTier::Developer,
                trust_generation: trust.generation().to_string(),
                handle,
                identity,
            });
        }
    }

    Err(ProvenanceError::Unsigned {
        path: dir.to_path_buf(),
        kind: options.kind.as_str(),
        id,
    })
}

/// Entrypoints an *unsigned* package is allowed to execute.
///
/// A signed package states them in its envelope. Vendor and developer
/// content has no envelope, so they are derived from the manifest the
/// package actually declares — never "every regular file in the tree".
/// A developer package that ships a helper script it did not declare
/// must not be able to run it as an entrypoint.
fn declared_entrypoints(
    kind: PackageKind,
    manifest_rel: &str,
    manifest_body: Option<&str>,
) -> Vec<String> {
    let mut out = vec![manifest_rel.to_string()];
    let Some(body) = manifest_body else {
        return out;
    };
    match kind {
        PackageKind::App => {
            if let Ok(manifest) = crate::caps::manifest::Manifest::from_json(body) {
                out.push(
                    manifest
                        .entry
                        .clone()
                        .unwrap_or_else(|| manifest.runtime.default_entry().to_string()),
                );
                if let Some(session) = manifest.session.as_ref() {
                    out.push(
                        session.entry.clone().unwrap_or_else(|| {
                            manifest.runtime.default_session_entry().to_string()
                        }),
                    );
                }
            }
        }
        PackageKind::Mcp => {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
                for arg in value
                    .get("args")
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str())
                {
                    if let Some(rel) = arg.strip_prefix("${manifest_dir}/") {
                        out.push(rel.to_string());
                    }
                }
                if let Some(command) = value.get("command").and_then(|v| v.as_str()) {
                    if let Some(rel) = command.strip_prefix("${manifest_dir}/") {
                        out.push(rel.to_string());
                    }
                }
            }
        }
        // A Skill is text, not an executable: its manifest is the only
        // thing the runtime reads directly.
        PackageKind::Skill => {}
    }
    out.sort();
    out.dedup();
    out
}

fn default_id_for(dir: &Path) -> String {
    dir.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_ascii_lowercase()
}

/// True when `dir` is inside one of [`VENDOR_PACKAGE_ROOTS`]. The
/// comparison is on the canonical path so a symlinked development
/// checkout cannot masquerade as installed vendor content.
pub fn is_vendor_root_path(dir: &Path) -> bool {
    let Ok(canonical) = dir.canonicalize() else {
        return false;
    };
    VENDOR_PACKAGE_ROOTS
        .iter()
        .any(|root| canonical.starts_with(root))
}

/// Walk the package tree and produce a normalized file list.
fn scan_tree(
    dir: &Path,
    handle: &DirHandle,
    max_bytes: u64,
    require_root_owned: bool,
) -> Result<Vec<FileEntry>, ProvenanceError> {
    let mut files: Vec<FileEntry> = Vec::new();
    let mut total: u64 = 0;
    let mut stack: Vec<Option<String>> = vec![None];
    while let Some(rel) = stack.pop() {
        let entries = handle
            .entries(rel.as_deref())
            .map_err(|e| ProvenanceError::Io {
                path: dir.join(rel.clone().unwrap_or_default()),
                reason: e.to_string(),
            })?;
        for (name, meta) in entries {
            let child = match &rel {
                Some(parent) => format!("{parent}/{name}"),
                None => name.clone(),
            };
            if rel.is_none() && name == ENVELOPE_FILE {
                continue;
            }
            super::envelope::validate_tree_path(&child).map_err(|reason| {
                ProvenanceError::TreeMismatch {
                    path: dir.join(&child),
                    reason,
                }
            })?;
            if meta.is_symlink {
                return Err(ProvenanceError::TreeMismatch {
                    path: dir.join(&child),
                    reason: "symlinks are not allowed inside a package".to_string(),
                });
            }
            if require_root_owned && meta.uid != 0 {
                return Err(ProvenanceError::TreeMismatch {
                    path: dir.join(&child),
                    reason: format!("vendor content is owned by uid {}", meta.uid),
                });
            }
            if meta.is_group_or_world_writable() {
                return Err(ProvenanceError::TreeMismatch {
                    path: dir.join(&child),
                    reason: format!("mode {:o} is group- or world-writable", meta.mode),
                });
            }
            if meta.is_dir {
                files.push(FileEntry {
                    path: child.clone(),
                    kind: NodeKind::Dir,
                    mode: meta.mode,
                    size: 0,
                    digest: String::new(),
                });
                stack.push(Some(child));
                continue;
            }
            if !meta.is_file {
                return Err(ProvenanceError::TreeMismatch {
                    path: dir.join(&child),
                    reason: "device, FIFO or socket nodes are not allowed".to_string(),
                });
            }
            if meta.nlink != 1 {
                return Err(ProvenanceError::TreeMismatch {
                    path: dir.join(&child),
                    reason: format!("hard-linked file ({} links)", meta.nlink),
                });
            }
            total = total.saturating_add(meta.size);
            if total > max_bytes {
                return Err(ProvenanceError::TreeMismatch {
                    path: dir.join(&child),
                    reason: format!("package exceeds the {max_bytes}-byte cap"),
                });
            }
            if files.len() >= super::envelope::MAX_FILES {
                return Err(ProvenanceError::TreeMismatch {
                    path: dir.join(&child),
                    reason: format!("package exceeds {} entries", super::envelope::MAX_FILES),
                });
            }
            let fd = handle
                .open_file(&child)
                .map_err(|e| ProvenanceError::TreeMismatch {
                    path: dir.join(&child),
                    reason: e.to_string(),
                })?;
            let bytes = fd
                .read_bounded(max_bytes)
                .map_err(|e| ProvenanceError::TreeMismatch {
                    path: dir.join(&child),
                    reason: e.to_string(),
                })?;
            files.push(FileEntry {
                path: child,
                kind: NodeKind::File,
                mode: meta.mode,
                size: bytes.len() as u64,
                digest: digest_bytes(&bytes),
            });
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// Confirm the on-disk tree is exactly the signed tree: no missing
/// entries, no extra entries, no type/mode/size/digest drift.
fn verify_tree(
    dir: &Path,
    handle: &DirHandle,
    declared: &[FileEntry],
    max_bytes: u64,
) -> Result<(), ProvenanceError> {
    let actual = scan_tree(dir, handle, max_bytes, false)?;
    let declared_map: BTreeMap<&str, &FileEntry> =
        declared.iter().map(|f| (f.path.as_str(), f)).collect();
    let actual_map: BTreeMap<&str, &FileEntry> =
        actual.iter().map(|f| (f.path.as_str(), f)).collect();

    for (path, want) in &declared_map {
        let Some(have) = actual_map.get(path) else {
            return Err(ProvenanceError::TreeMismatch {
                path: dir.join(path),
                reason: "signed file is missing from the package".to_string(),
            });
        };
        if have.kind != want.kind {
            return Err(ProvenanceError::TreeMismatch {
                path: dir.join(path),
                reason: format!(
                    "expected a {} but found a {}",
                    want.kind.as_str(),
                    have.kind.as_str()
                ),
            });
        }
        if have.mode != want.mode {
            return Err(ProvenanceError::TreeMismatch {
                path: dir.join(path),
                reason: format!(
                    "mode {:o} does not match signed mode {:o}",
                    have.mode, want.mode
                ),
            });
        }
        if have.size != want.size || have.digest != want.digest {
            return Err(ProvenanceError::TreeMismatch {
                path: dir.join(path),
                reason: "content does not match the signed digest".to_string(),
            });
        }
    }
    for path in actual_map.keys() {
        if !declared_map.contains_key(path) {
            return Err(ProvenanceError::TreeMismatch {
                path: dir.join(path),
                reason: "file is present but not covered by the signature".to_string(),
            });
        }
    }
    Ok(())
}

// ------------------------------------------------------------ vendor pins

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct VendorPins {
    #[serde(default)]
    pins: BTreeMap<String, String>,
}

/// Record (or confirm) the content digest of a vendor package.
///
/// Only root can write under an approved package root, so a changed
/// digest is either a legitimate package upgrade or a root-level
/// compromise. Either way the change is surfaced: the pin rotates and
/// an audit record is written rather than the change passing silently.
fn pin_vendor_digest(
    dir: &Path,
    kind: PackageKind,
    id: &str,
    digest: &str,
) -> Result<(), ProvenanceError> {
    let path = crate::paths::vendor_pin_path();
    let key = format!("{}:{}:{}", kind.as_str(), id, dir.display());
    let mut pins: VendorPins = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    match pins.pins.get(&key) {
        Some(existing) if existing == digest => return Ok(()),
        Some(existing) => {
            crate::audit::log_event(
                &crate::paths::provenance_audit_path(),
                serde_json::json!({
                    "kind": "provenance.vendor_pin_rotated",
                    "package_kind": kind.as_str(),
                    "package_id": id,
                    "previous_digest": existing,
                    "content_digest": digest,
                    "path": dir.display().to_string(),
                }),
            );
        }
        None => {}
    }
    pins.pins.insert(key, digest.to_string());
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let body = serde_json::to_string_pretty(&pins).unwrap_or_else(|_| "{}".to_string());
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
    Ok(())
}

// ----------------------------------------------------------------- cache

type CacheMap = BTreeMap<String, Arc<VerifiedPackage>>;

fn cache() -> &'static Mutex<CacheMap> {
    static CACHE: OnceLock<Mutex<CacheMap>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Drop every cached verification. Called after any trust mutation
/// (key add/revoke, package revoke, developer grant change).
pub fn invalidate_cache() {
    if let Ok(mut guard) = cache().lock() {
        guard.clear();
    }
}

/// Verify with a process-level cache keyed by path, kind and trust
/// generation. A revoked key or package changes the generation and
/// therefore misses every previously cached entry.
pub fn verify_package_cached(
    dir: &Path,
    options: &VerifyOptions,
    trust: &TrustStore,
) -> Result<Arc<VerifiedPackage>, ProvenanceError> {
    let key = format!(
        "{}|{}|{}",
        options.kind.as_str(),
        dir.display(),
        trust.generation()
    );
    if let Ok(guard) = cache().lock() {
        if let Some(hit) = guard.get(&key) {
            if hit.assert_current(trust).is_ok() {
                return Ok(Arc::clone(hit));
            }
        }
    }
    let verified = Arc::new(verify_package(dir, options, trust)?);
    if let Ok(mut guard) = cache().lock() {
        guard.insert(key, Arc::clone(&verified));
    }
    Ok(verified)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/provenance/verify.rs"
    ));
}
