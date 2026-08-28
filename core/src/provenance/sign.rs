//! Package signing: envelope construction and Ed25519 signing.
//!
//! Only the developer/publisher workflow needs this half of the
//! format; the runtime never signs. It lives in the same module as
//! the verifier so the canonical encoding has exactly one
//! implementation and a signing bug cannot drift away from the
//! verification rules.
//!
//! Private keys are never written into a package, never logged and
//! never committed. [`SigningKeyFile`] refuses to load a key file that
//! any other user can read.

use std::io::Read;
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};

use super::envelope::{
    content_digest, key_id_for, validate_tree_path, Envelope, EnvelopeSignature, FileEntry,
    NodeKind, PackageBody, PackageKind, ALG_ED25519, ENVELOPE_FILE, SCHEMA_V1,
};

pub const SIGNING_KEY_SCHEMA_V1: &str = "claw.signing-key/v1";

#[derive(Debug, thiserror::Error)]
pub enum SignError {
    #[error("{path}: {reason}")]
    Io { path: PathBuf, reason: String },
    #[error("signing key file {path} is readable by other users (mode {mode:o})")]
    KeyFilePermissions { path: PathBuf, mode: u32 },
    #[error("signing key file {path} is invalid: {reason}")]
    KeyFile { path: PathBuf, reason: String },
    #[error("{path}: {reason}")]
    Tree { path: PathBuf, reason: String },
    #[error("envelope construction failed: {0}")]
    Envelope(String),
}

/// On-disk private key material. `private_key` is the 32-byte Ed25519
/// seed in lowercase hex.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SigningKeyFile {
    pub schema: String,
    pub algorithm: String,
    pub key_id: String,
    pub public_key: String,
    pub private_key: String,
    #[serde(default)]
    pub comment: Option<String>,
}

impl SigningKeyFile {
    /// Generate a fresh key from the OS CSPRNG.
    pub fn generate(comment: Option<String>) -> Result<Self, SignError> {
        let seed = os_random_32()?;
        let signing = SigningKey::from_bytes(&seed);
        let public = signing.verifying_key().to_bytes();
        Ok(Self {
            schema: SIGNING_KEY_SCHEMA_V1.to_string(),
            algorithm: ALG_ED25519.to_string(),
            key_id: key_id_for(&public),
            public_key: hex::encode(public),
            private_key: hex::encode(seed),
            comment,
        })
    }

    pub fn load(path: &Path) -> Result<Self, SignError> {
        #[cfg(unix)]
        {
            let meta = super::fsec::lstat(path).map_err(|e| SignError::Io {
                path: path.to_path_buf(),
                reason: e.to_string(),
            })?;
            if meta.is_symlink || !meta.is_file {
                return Err(SignError::KeyFile {
                    path: path.to_path_buf(),
                    reason: "not a regular file".to_string(),
                });
            }
            if meta.mode & 0o077 != 0 {
                return Err(SignError::KeyFilePermissions {
                    path: path.to_path_buf(),
                    mode: meta.mode,
                });
            }
        }
        let raw = std::fs::read_to_string(path).map_err(|e| SignError::Io {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })?;
        let parsed: Self = serde_json::from_str(&raw).map_err(|e| SignError::KeyFile {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })?;
        if parsed.schema != SIGNING_KEY_SCHEMA_V1 {
            return Err(SignError::KeyFile {
                path: path.to_path_buf(),
                reason: format!("unsupported schema `{}`", parsed.schema),
            });
        }
        if parsed.algorithm != ALG_ED25519 {
            return Err(SignError::KeyFile {
                path: path.to_path_buf(),
                reason: format!("unsupported algorithm `{}`", parsed.algorithm),
            });
        }
        let key = parsed.signing_key().map_err(|reason| SignError::KeyFile {
            path: path.to_path_buf(),
            reason,
        })?;
        if key_id_for(&key.verifying_key().to_bytes()) != parsed.key_id {
            return Err(SignError::KeyFile {
                path: path.to_path_buf(),
                reason: "key_id does not bind to the private key".to_string(),
            });
        }
        Ok(parsed)
    }

    /// Write the key with `0600` permissions, refusing to clobber.
    pub fn write_new(&self, path: &Path) -> Result<(), SignError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| SignError::Io {
                path: parent.to_path_buf(),
                reason: e.to_string(),
            })?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path).map_err(|e| SignError::Io {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })?;
        use std::io::Write;
        let body = serde_json::to_vec_pretty(self).map_err(|e| SignError::Io {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })?;
        file.write_all(&body).map_err(|e| SignError::Io {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })?;
        file.sync_all().map_err(|e| SignError::Io {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })
    }

    pub fn signing_key(&self) -> Result<SigningKey, String> {
        let bytes = hex::decode(&self.private_key).map_err(|e| e.to_string())?;
        let seed: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| "private_key must be 32 bytes".to_string())?;
        Ok(SigningKey::from_bytes(&seed))
    }

    /// Publisher trust entry for this key, ready to drop into a trust
    /// root. Contains public material only.
    pub fn trust_entry(&self, kinds: &[PackageKind]) -> serde_json::Value {
        serde_json::json!({
            "schema": super::trust::TRUST_SCHEMA_V1,
            "keys": [{
                "key_id": self.key_id,
                "algorithm": ALG_ED25519,
                "public_key": self.public_key,
                "usages": [super::trust::USAGE_PACKAGE_SIGNING],
                "kinds": kinds.iter().map(|k| k.as_str()).collect::<Vec<_>>(),
                "status": "active",
                "comment": self.comment,
            }],
        })
    }
}

fn os_random_32() -> Result<[u8; 32], SignError> {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 32];
        let mut file = std::fs::File::open("/dev/urandom").map_err(|e| SignError::Io {
            path: PathBuf::from("/dev/urandom"),
            reason: e.to_string(),
        })?;
        file.read_exact(&mut buf).map_err(|e| SignError::Io {
            path: PathBuf::from("/dev/urandom"),
            reason: e.to_string(),
        })?;
        Ok(buf)
    }
    #[cfg(not(unix))]
    {
        Err(SignError::Io {
            path: PathBuf::from("/dev/urandom"),
            reason: "key generation requires a Unix host".to_string(),
        })
    }
}

/// What to bind into the envelope beyond the file tree.
#[derive(Debug, Clone)]
pub struct SignRequest {
    pub kind: PackageKind,
    pub id: String,
    pub version: String,
    pub manifest_schema: String,
    pub manifest_path: String,
    pub entrypoints: Vec<String>,
    pub resources: Vec<String>,
}

/// Build the signed body by walking `dir`.
///
/// Rejects the same shapes the verifier rejects, so a package can
/// never be signed into a state that will not verify: symlinks,
/// hardlinks, special files, group/world-writable modes, traversal or
/// case-colliding names.
pub fn build_body(dir: &Path, request: &SignRequest) -> Result<PackageBody, SignError> {
    let mut files = Vec::new();
    collect(dir, dir, &mut files)?;
    files.sort_by(|a: &FileEntry, b: &FileEntry| a.path.cmp(&b.path));
    let digest = content_digest(&files);
    let body = PackageBody {
        kind: request.kind,
        id: request.id.clone(),
        version: request.version.clone(),
        manifest_schema: request.manifest_schema.clone(),
        manifest_path: request.manifest_path.clone(),
        entrypoints: request.entrypoints.clone(),
        resources: request.resources.clone(),
        files,
        content_digest: digest,
    };
    body.validate()
        .map_err(|e| SignError::Envelope(e.to_string()))?;
    Ok(body)
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<FileEntry>) -> Result<(), SignError> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| SignError::Io {
            path: dir.to_path_buf(),
            reason: e.to_string(),
        })?
        .filter_map(Result::ok)
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .map_err(|_| SignError::Tree {
                path: path.clone(),
                reason: "escaped the package root".to_string(),
            })?
            .to_string_lossy()
            .replace('\\', "/");
        if rel == ENVELOPE_FILE {
            continue;
        }
        validate_tree_path(&rel).map_err(|reason| SignError::Tree {
            path: path.clone(),
            reason,
        })?;
        let meta = std::fs::symlink_metadata(&path).map_err(|e| SignError::Io {
            path: path.clone(),
            reason: e.to_string(),
        })?;
        if meta.file_type().is_symlink() {
            return Err(SignError::Tree {
                path,
                reason: "symlinks cannot be signed".to_string(),
            });
        }
        let mode = file_mode(&meta);
        if mode & 0o022 != 0 {
            return Err(SignError::Tree {
                path,
                reason: format!("mode {mode:o} is group- or world-writable"),
            });
        }
        if meta.is_dir() {
            out.push(FileEntry {
                path: rel,
                kind: NodeKind::Dir,
                mode,
                size: 0,
                digest: String::new(),
            });
            collect(root, &path, out)?;
            continue;
        }
        if !meta.is_file() {
            return Err(SignError::Tree {
                path,
                reason: "special files cannot be signed".to_string(),
            });
        }
        let bytes = std::fs::read(&path).map_err(|e| SignError::Io {
            path: path.clone(),
            reason: e.to_string(),
        })?;
        let mut h = crate::crypto::Sha256Stream::new();
        h.update(&bytes);
        out.push(FileEntry {
            path: rel,
            kind: NodeKind::File,
            mode,
            size: bytes.len() as u64,
            digest: format!("sha256:{}", h.finalize_hex()),
        });
    }
    Ok(())
}

#[cfg(unix)]
fn file_mode(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    meta.mode() & 0o7777
}

#[cfg(not(unix))]
fn file_mode(meta: &std::fs::Metadata) -> u32 {
    if meta.is_dir() {
        0o755
    } else {
        0o644
    }
}

/// Sign a package directory and return the envelope.
pub fn sign_body(body: PackageBody, key: &SigningKeyFile) -> Result<Envelope, SignError> {
    let signing = key.signing_key().map_err(SignError::Envelope)?;
    let public = signing.verifying_key().to_bytes();
    let key_id = key_id_for(&public);
    let public_hex = hex::encode(public);
    let message = super::envelope::canonical_bytes(&body, ALG_ED25519, &key_id, &public_hex);
    let signature = signing.sign(&message);
    let envelope = Envelope {
        schema: SCHEMA_V1.to_string(),
        package: body,
        signature: EnvelopeSignature {
            algorithm: ALG_ED25519.to_string(),
            key_id,
            public_key: public_hex,
            value: hex::encode(signature.to_bytes()),
        },
    };
    envelope
        .validate()
        .map_err(|e| SignError::Envelope(e.to_string()))?;
    Ok(envelope)
}

/// Build, sign and write `.provenance.json` into `dir`.
pub fn sign_directory(
    dir: &Path,
    request: &SignRequest,
    key: &SigningKeyFile,
) -> Result<Envelope, SignError> {
    let body = build_body(dir, request)?;
    let envelope = sign_body(body, key)?;
    let path = dir.join(ENVELOPE_FILE);
    let body = serde_json::to_vec_pretty(&envelope).map_err(|e| SignError::Io {
        path: path.clone(),
        reason: e.to_string(),
    })?;
    std::fs::write(&path, body).map_err(|e| SignError::Io {
        path: path.clone(),
        reason: e.to_string(),
    })?;
    Ok(envelope)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/provenance/sign.rs"
    ));
}
