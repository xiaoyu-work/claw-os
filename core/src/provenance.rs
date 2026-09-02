//! Authenticated, immutable snapshots of installed extension packages.
//!
//! Package consumers call [`verify`] once and use only the returned
//! [`VerifiedPackage`]. The verifier reads every signed file into memory,
//! proves the complete package inventory and Ed25519 signature, and never
//! exposes the mutable source path to downstream code.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path};

use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

const PROVENANCE_FILE: &str = "provenance.json";
const PROVENANCE_SCHEMA_VERSION: u32 = 1;
const MAX_PROVENANCE_BYTES: u64 = 64 * 1024;
const MAX_PACKAGE_FILES: usize = 128;
const MAX_PACKAGE_BYTES: u64 = 1024 * 1024;
const MAX_RELATIVE_PATH_BYTES: usize = 240;
const SIGNING_DOMAIN: &[u8] = b"claw-os.package-provenance.v1";

const RELEASE_PUBLIC_KEY: [u8; 32] = [
    0xb1, 0xcd, 0xf5, 0x61, 0x79, 0x53, 0x29, 0x94, 0xfa, 0x3c, 0x30, 0xf8, 0x24, 0xe2, 0x59, 0x9b,
    0x0c, 0x1e, 0xcc, 0xc1, 0xb0, 0x4a, 0x8d, 0x7f, 0xa1, 0x78, 0x56, 0x2d, 0xed, 0x10, 0x72, 0xf0,
];

#[cfg(debug_assertions)]
const DEBUG_PUBLIC_KEY: [u8; 32] = [
    0xa3, 0x64, 0x68, 0xc0, 0x4a, 0xed, 0xb2, 0x2f, 0x93, 0x32, 0x06, 0xac, 0x31, 0xd5, 0x15, 0x4b,
    0x60, 0x6b, 0x18, 0xd3, 0xbf, 0xf5, 0x74, 0xac, 0xec, 0xdc, 0x01, 0x27, 0x17, 0xe2, 0xbc, 0xc7,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PackageKind {
    AgentExtension,
}

impl PackageKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AgentExtension => "agent-extension",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedFile {
    pub path: String,
    pub sha256: String,
    pub size: u64,
    pub executable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageProvenance {
    pub schema_version: u32,
    pub kind: PackageKind,
    pub publisher: String,
    pub key_id: String,
    pub package_id: String,
    pub package_version: String,
    pub package_digest: String,
    pub files: Vec<SignedFile>,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotFile {
    pub path: String,
    pub executable: bool,
    pub bytes_base64: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageSnapshot {
    pub provenance: PackageProvenance,
    pub files: Vec<SnapshotFile>,
}

#[derive(Debug, Clone)]
pub struct VerifiedPackage {
    provenance: PackageProvenance,
    files: BTreeMap<String, VerifiedFile>,
}

#[derive(Debug, Clone)]
struct VerifiedFile {
    bytes: Vec<u8>,
    executable: bool,
}

impl VerifiedPackage {
    pub fn id(&self) -> &str {
        &self.provenance.package_id
    }

    pub fn version(&self) -> &str {
        &self.provenance.package_version
    }

    pub fn digest(&self) -> &str {
        &self.provenance.package_digest
    }

    pub fn publisher(&self) -> &str {
        &self.provenance.publisher
    }

    pub fn signed_files(&self) -> &[SignedFile] {
        &self.provenance.files
    }

    pub fn file_bytes(&self, path: &str) -> Option<&[u8]> {
        self.files.get(path).map(|file| file.bytes.as_slice())
    }

    pub fn file_is_executable(&self, path: &str) -> bool {
        self.files.get(path).is_some_and(|file| file.executable)
    }

    pub fn snapshot(&self) -> PackageSnapshot {
        PackageSnapshot {
            provenance: self.provenance.clone(),
            files: self
                .files
                .iter()
                .map(|(path, file)| SnapshotFile {
                    path: path.clone(),
                    executable: file.executable,
                    bytes_base64: base64::engine::general_purpose::STANDARD.encode(&file.bytes),
                })
                .collect(),
        }
    }
}

struct TrustRoot {
    publisher: &'static str,
    key_id: &'static str,
    public_key: [u8; 32],
}

fn trust_roots() -> Vec<TrustRoot> {
    let mut roots = vec![TrustRoot {
        publisher: "claw-os",
        key_id: "release-1",
        public_key: RELEASE_PUBLIC_KEY,
    }];
    #[cfg(debug_assertions)]
    roots.push(TrustRoot {
        publisher: "claw-os-test",
        key_id: "debug-1",
        public_key: DEBUG_PUBLIC_KEY,
    });
    roots
}

/// Verify an installed package and return a content-complete immutable
/// snapshot. Production verification requires every package object and its
/// ancestor chain to be root-owned and not group/world writable.
pub fn verify(path: &Path, expected_kind: PackageKind) -> Result<VerifiedPackage, String> {
    verify_path_with_roots(path, expected_kind, &trust_roots(), true)
}

/// Re-verify a transported snapshot inside `claw-extension-host`.
///
/// This second verification means a compromised worker cannot ask the host to
/// execute bytes that were never signed, even though the worker controls the
/// private host-control request body.
pub fn verify_snapshot(
    snapshot: &PackageSnapshot,
    expected_kind: PackageKind,
) -> Result<VerifiedPackage, String> {
    verify_snapshot_with_roots(snapshot, expected_kind, &trust_roots())
}

fn verify_path_with_roots(
    path: &Path,
    expected_kind: PackageKind,
    roots: &[TrustRoot],
    require_root_ownership: bool,
) -> Result<VerifiedPackage, String> {
    let root_metadata =
        fs::symlink_metadata(path).map_err(|error| format!("inspect package root: {error}"))?;
    if !root_metadata.is_dir() || root_metadata.file_type().is_symlink() {
        return Err("package root is not a real directory".to_string());
    }
    if require_root_ownership {
        validate_root_owned_ancestors(path)?;
    }

    let provenance_path = path.join(PROVENANCE_FILE);
    let provenance_metadata = fs::symlink_metadata(&provenance_path)
        .map_err(|error| format!("inspect {PROVENANCE_FILE}: {error}"))?;
    if !provenance_metadata.is_file()
        || provenance_metadata.file_type().is_symlink()
        || provenance_metadata.len() == 0
        || provenance_metadata.len() > MAX_PROVENANCE_BYTES
    {
        return Err(format!("{PROVENANCE_FILE} has an unsafe shape"));
    }
    if require_root_ownership {
        require_root_owned_file(&provenance_metadata, PROVENANCE_FILE)?;
    }
    let provenance_bytes =
        read_bounded_file(&provenance_path, MAX_PROVENANCE_BYTES, &provenance_metadata)?;
    let provenance: PackageProvenance = serde_json::from_slice(&provenance_bytes)
        .map_err(|error| format!("parse {PROVENANCE_FILE}: {error}"))?;

    let mut files = BTreeMap::new();
    collect_package_files(path, path, require_root_ownership, &mut files, 0, &mut 0)?;
    let root_after =
        fs::symlink_metadata(path).map_err(|error| format!("recheck package root: {error}"))?;
    if !same_file_identity(&root_metadata, &root_after) {
        return Err("package root changed during verification".to_string());
    }
    let snapshot = PackageSnapshot {
        provenance,
        files: files
            .into_iter()
            .map(|(path, file)| SnapshotFile {
                path,
                executable: file.executable,
                bytes_base64: base64::engine::general_purpose::STANDARD.encode(file.bytes),
            })
            .collect(),
    };
    verify_snapshot_with_roots(&snapshot, expected_kind, roots)
}

fn collect_package_files(
    root: &Path,
    directory: &Path,
    require_root_ownership: bool,
    out: &mut BTreeMap<String, VerifiedFile>,
    depth: usize,
    total_bytes: &mut u64,
) -> Result<(), String> {
    if depth > 16 {
        return Err("package tree is too deep".to_string());
    }
    let mut entries = fs::read_dir(directory)
        .map_err(|error| format!("list package directory: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("read package directory: {error}"))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|_| "package entry escaped its root".to_string())?;
        let relative = normalize_relative_path(relative)?;
        if relative == PROVENANCE_FILE {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("inspect package entry `{relative}`: {error}"))?;
        if metadata.file_type().is_symlink() {
            return Err(format!("package entry `{relative}` is a symlink"));
        }
        if require_root_ownership {
            require_root_owned_file(&metadata, &relative)?;
        }
        if metadata.is_dir() {
            collect_package_files(
                root,
                &path,
                require_root_ownership,
                out,
                depth + 1,
                total_bytes,
            )?;
            let after = fs::symlink_metadata(&path)
                .map_err(|error| format!("recheck package directory `{relative}`: {error}"))?;
            if !same_file_identity(&metadata, &after) {
                return Err(format!(
                    "package directory `{relative}` changed during verification"
                ));
            }
            continue;
        }
        if !metadata.is_file() {
            return Err(format!("package entry `{relative}` is not a regular file"));
        }
        if out.len() >= MAX_PACKAGE_FILES {
            return Err("package contains too many files".to_string());
        }
        *total_bytes = total_bytes
            .checked_add(metadata.len())
            .ok_or_else(|| "package size overflow".to_string())?;
        if *total_bytes > MAX_PACKAGE_BYTES {
            return Err("package exceeds its size limit".to_string());
        }
        let bytes = read_bounded_file(&path, metadata.len(), &metadata)?;
        let after = fs::symlink_metadata(&path)
            .map_err(|error| format!("recheck package entry `{relative}`: {error}"))?;
        if !same_file_identity(&metadata, &after) {
            return Err(format!(
                "package entry `{relative}` changed during verification"
            ));
        }
        out.insert(
            relative,
            VerifiedFile {
                bytes,
                executable: metadata.permissions().mode() & 0o111 != 0,
            },
        );
    }
    Ok(())
}

fn verify_snapshot_with_roots(
    snapshot: &PackageSnapshot,
    expected_kind: PackageKind,
    roots: &[TrustRoot],
) -> Result<VerifiedPackage, String> {
    validate_provenance_shape(&snapshot.provenance, expected_kind)?;
    let mut decoded = BTreeMap::new();
    let mut total_bytes = 0u64;
    for file in &snapshot.files {
        let path = normalize_relative_path(Path::new(&file.path))?;
        if path != file.path || path == PROVENANCE_FILE || decoded.contains_key(&path) {
            return Err("package snapshot has a duplicate or reserved path".to_string());
        }
        if file.bytes_base64.len() > (MAX_PACKAGE_BYTES as usize).div_ceil(3) * 4 + 4 {
            return Err(format!(
                "package file `{path}` has an oversized base64 encoding"
            ));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&file.bytes_base64)
            .map_err(|_| format!("package file `{path}` is not valid base64"))?;
        total_bytes = total_bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| "package size overflow".to_string())?;
        if decoded.len() >= MAX_PACKAGE_FILES || total_bytes > MAX_PACKAGE_BYTES {
            return Err("package snapshot exceeds its limits".to_string());
        }
        decoded.insert(
            path,
            VerifiedFile {
                bytes,
                executable: file.executable,
            },
        );
    }

    let actual_files = decoded
        .iter()
        .map(|(path, file)| SignedFile {
            path: path.clone(),
            sha256: crate::crypto::sha256_hex(&file.bytes),
            size: file.bytes.len() as u64,
            executable: file.executable,
        })
        .collect::<Vec<_>>();
    if actual_files != snapshot.provenance.files {
        return Err("package inventory does not match the signed provenance".to_string());
    }
    let digest = package_digest(&actual_files);
    if digest != snapshot.provenance.package_digest {
        return Err("package digest does not match the signed inventory".to_string());
    }
    verify_signature(&snapshot.provenance, roots)?;
    Ok(VerifiedPackage {
        provenance: snapshot.provenance.clone(),
        files: decoded,
    })
}

fn validate_provenance_shape(
    provenance: &PackageProvenance,
    expected_kind: PackageKind,
) -> Result<(), String> {
    if provenance.schema_version != PROVENANCE_SCHEMA_VERSION {
        return Err(format!(
            "unsupported package provenance version {}",
            provenance.schema_version
        ));
    }
    if provenance.kind != expected_kind {
        return Err(format!(
            "package kind mismatch: expected {}, found {}",
            expected_kind.as_str(),
            provenance.kind.as_str()
        ));
    }
    validate_identity(&provenance.publisher, "publisher")?;
    validate_identity(&provenance.key_id, "key id")?;
    validate_identity(&provenance.package_id, "package id")?;
    validate_version(&provenance.package_version)?;
    validate_digest(&provenance.package_digest, "package digest")?;
    if provenance.files.is_empty() || provenance.files.len() > MAX_PACKAGE_FILES {
        return Err("signed package inventory has an invalid size".to_string());
    }
    let mut prior = None;
    let mut seen = BTreeSet::new();
    let mut total = 0u64;
    for file in &provenance.files {
        let normalized = normalize_relative_path(Path::new(&file.path))?;
        if normalized != file.path || file.path == PROVENANCE_FILE || !seen.insert(&file.path) {
            return Err("signed package inventory has an invalid path".to_string());
        }
        if prior.is_some_and(|value: &str| value >= file.path.as_str()) {
            return Err("signed package inventory is not strictly sorted".to_string());
        }
        prior = Some(file.path.as_str());
        validate_digest(&file.sha256, "file digest")?;
        total = total
            .checked_add(file.size)
            .ok_or_else(|| "signed package size overflow".to_string())?;
        if total > MAX_PACKAGE_BYTES {
            return Err("signed package inventory exceeds its size limit".to_string());
        }
    }
    if provenance.signature.len() != 128
        || !provenance
            .signature
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err("package signature has an invalid encoding".to_string());
    }
    Ok(())
}

fn verify_signature(provenance: &PackageProvenance, roots: &[TrustRoot]) -> Result<(), String> {
    let root = roots
        .iter()
        .find(|root| root.publisher == provenance.publisher && root.key_id == provenance.key_id)
        .ok_or_else(|| {
            format!(
                "package publisher `{}` key `{}` is not trusted",
                provenance.publisher, provenance.key_id
            )
        })?;
    let key = VerifyingKey::from_bytes(&root.public_key)
        .map_err(|error| format!("compiled package trust root is invalid: {error}"))?;
    let signature_bytes = hex::decode(&provenance.signature)
        .map_err(|_| "package signature is not hex".to_string())?;
    let signature_bytes: [u8; 64] = signature_bytes
        .try_into()
        .map_err(|_| "package signature has the wrong length".to_string())?;
    key.verify(
        &signing_input(provenance),
        &Signature::from_bytes(&signature_bytes),
    )
    .map_err(|_| "package signature verification failed".to_string())
}

/// Canonical bytes signed by package publishers.
///
/// The encoding is length-prefixed instead of relying on JSON object order.
/// It is public so release tooling and process tests can create packages
/// without duplicating the wire contract.
pub fn signing_input(provenance: &PackageProvenance) -> Vec<u8> {
    let mut out = Vec::new();
    push_bytes(&mut out, SIGNING_DOMAIN);
    push_u32(&mut out, provenance.schema_version);
    push_bytes(&mut out, provenance.kind.as_str().as_bytes());
    push_bytes(&mut out, provenance.publisher.as_bytes());
    push_bytes(&mut out, provenance.key_id.as_bytes());
    push_bytes(&mut out, provenance.package_id.as_bytes());
    push_bytes(&mut out, provenance.package_version.as_bytes());
    push_bytes(&mut out, provenance.package_digest.as_bytes());
    push_u32(&mut out, provenance.files.len() as u32);
    for file in &provenance.files {
        push_bytes(&mut out, file.path.as_bytes());
        push_bytes(&mut out, file.sha256.as_bytes());
        push_u64(&mut out, file.size);
        out.push(u8::from(file.executable));
    }
    out
}

pub fn package_digest(files: &[SignedFile]) -> String {
    let mut input = Vec::new();
    push_bytes(&mut input, b"claw-os.package-content.v1");
    push_u32(&mut input, files.len() as u32);
    for file in files {
        push_bytes(&mut input, file.path.as_bytes());
        push_bytes(&mut input, file.sha256.as_bytes());
        push_u64(&mut input, file.size);
        input.push(u8::from(file.executable));
    }
    crate::crypto::sha256_hex(&input)
}

fn normalize_relative_path(path: &Path) -> Result<String, String> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err("package path must be non-empty and relative".to_string());
    }
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                let value = value
                    .to_str()
                    .ok_or_else(|| "package path is not UTF-8".to_string())?;
                if value.is_empty()
                    || value.starts_with('.')
                    || value.bytes().any(|byte| {
                        !(byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
                    })
                {
                    return Err("package path contains an invalid component".to_string());
                }
                parts.push(value);
            }
            _ => return Err("package path contains traversal".to_string()),
        }
    }
    let normalized = parts.join("/");
    if normalized.len() > MAX_RELATIVE_PATH_BYTES {
        return Err("package path is too long".to_string());
    }
    Ok(normalized)
}

fn validate_identity(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
    {
        return Err(format!("package {label} is invalid"));
    }
    Ok(())
}

fn validate_version(version: &str) -> Result<(), String> {
    let parsed =
        semver::Version::parse(version).map_err(|_| "package version is not semver".to_string())?;
    if parsed.to_string() != version || version.len() > 64 {
        return Err("package version is not canonical semver".to_string());
    }
    Ok(())
}

fn validate_digest(value: &str, label: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(format!("{label} is invalid"));
    }
    Ok(())
}

fn read_bounded_file(path: &Path, max: u64, expected: &fs::Metadata) -> Result<Vec<u8>, String> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| format!("open package file {}: {error}", path.display()))?;
    let opened = file
        .metadata()
        .map_err(|error| format!("inspect opened package file {}: {error}", path.display()))?;
    if !opened.is_file()
        || opened.dev() != expected.dev()
        || opened.ino() != expected.ino()
        || opened.len() != expected.len()
    {
        return Err(format!(
            "package file {} changed before it was opened",
            path.display()
        ));
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(max.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read package file {}: {error}", path.display()))?;
    if bytes.len() as u64 > max {
        return Err(format!("package file {} exceeds its limit", path.display()));
    }
    let after = file
        .metadata()
        .map_err(|error| format!("recheck opened package file {}: {error}", path.display()))?;
    if !same_file_identity(expected, &after) {
        return Err(format!(
            "package file {} changed while it was read",
            path.display()
        ));
    }
    let named_after = fs::symlink_metadata(path)
        .map_err(|error| format!("recheck package file name {}: {error}", path.display()))?;
    if !same_file_identity(expected, &named_after) {
        return Err(format!(
            "package file name {} changed while it was read",
            path.display()
        ));
    }
    Ok(bytes)
}

fn same_file_identity(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.len() == after.len()
        && before.uid() == after.uid()
        && before.gid() == after.gid()
        && before.mode() == after.mode()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
        && before.ctime() == after.ctime()
        && before.ctime_nsec() == after.ctime_nsec()
}

fn require_root_owned_file(metadata: &fs::Metadata, label: &str) -> Result<(), String> {
    if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err(format!(
            "package object `{label}` is not root-owned and non-writable"
        ));
    }
    Ok(())
}

fn validate_root_owned_ancestors(path: &Path) -> Result<(), String> {
    let mut current = Some(path);
    while let Some(component) = current {
        let metadata = fs::symlink_metadata(component).map_err(|error| {
            format!("inspect package ancestor {}: {error}", component.display())
        })?;
        if metadata.file_type().is_symlink() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0
        {
            return Err(format!(
                "package ancestor {} has unsafe ownership or mode",
                component.display()
            ));
        }
        current = component.parent();
    }
    Ok(())
}

fn push_bytes(out: &mut Vec<u8>, value: &[u8]) {
    push_u32(out, value.len() as u32);
    out.extend_from_slice(value);
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn push_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_be_bytes());
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/provenance.rs"
    ));
}
