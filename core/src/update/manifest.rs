//! The `claw.release-security/v1` manifest.
//!
//! One signed document per published package that states, for a single
//! coordinated release: which security epoch and ABI generation it
//! belongs to, its exact Debian version, the SHA-256 of every security
//! component it installs, the protocol epochs its binaries speak, the
//! lowest mutually compatible version of every sibling package, the
//! repository suite/component it is published into, and when it stops
//! being valid.
//!
//! It is produced at package build time by
//! `packaging/release-security/make-manifest.py`, embedded in the
//! package under [`RELEASE_SECURITY_DIR`](super::RELEASE_SECURITY_DIR),
//! copied verbatim into the maintainer scripts, and published beside
//! the APT repository. Every consumer parses it through
//! [`Manifest::parse`], which refuses a document that is not in the
//! canonical encoding, so the bytes a signature covers are the only
//! bytes that can ever be interpreted.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde_json::Value;

use super::canonical;
use super::debver;

pub const FORMAT: &str = "claw.release-security/v1";

/// Largest manifest that will be read from disk or from a maintainer
/// script. Real manifests are a few kilobytes.
pub const MAX_MANIFEST_BYTES: u64 = 256 * 1024;

/// A component digest recorded by the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentDigest {
    pub name: String,
    pub path: String,
    pub sha256: String,
}

/// A parsed, structurally valid manifest. Validity in *time* and
/// against the local floor is decided separately in
/// [`super::decide`], so parsing never has to know the clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub security_epoch: u64,
    pub abi: u32,
    pub package: String,
    pub version: String,
    pub architecture: String,
    pub suite: String,
    pub component: String,
    pub issued_at: DateTime<Utc>,
    pub valid_until: DateTime<Utc>,
    pub protocols: BTreeMap<String, u32>,
    pub minimum_compatible: BTreeMap<String, String>,
    pub components: Vec<ComponentDigest>,
    pub revoked_digests: BTreeSet<String>,
    pub revoked_keys: BTreeSet<String>,
    /// SHA-256 of the canonical bytes: the identity a floor records and
    /// a revocation list names.
    pub digest: String,
    /// Exactly the bytes that were parsed, so a signature check and a
    /// digest comparison cannot drift from what was interpreted.
    pub bytes: Vec<u8>,
}

impl Manifest {
    /// Parse and structurally validate a manifest.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() as u64 > MAX_MANIFEST_BYTES {
            return Err("release manifest is larger than the accepted maximum".to_string());
        }
        let value = canonical::parse_canonical(bytes)
            .map_err(|error| format!("release manifest rejected: {error}"))?;
        let object = value
            .as_object()
            .ok_or_else(|| "release manifest is not a JSON object".to_string())?;

        let format = string_field(object, "format")?;
        if format != FORMAT {
            return Err(format!(
                "release manifest format `{format}` is not `{FORMAT}`"
            ));
        }

        let security_epoch = u64_field(object, "security_epoch")?;
        let abi = u64_field(object, "abi")?
            .try_into()
            .map_err(|_| "abi generation is out of range".to_string())?;

        let release = object
            .get("release")
            .and_then(Value::as_object)
            .ok_or_else(|| "release manifest has no `release` object".to_string())?;
        let package = token_field(release, "package")?;
        let version = string_field(release, "version")?;
        if !debver::is_valid(&version) {
            return Err(format!(
                "release version `{version}` is not a Debian version"
            ));
        }
        // The security epoch has to be visible to APT, or it cannot
        // influence which candidate APT selects and an emergency
        // release with a lower upstream version would never be chosen.
        // The Debian epoch is the only field that outranks every
        // upstream version, so the two must agree.
        let declared = debver::epoch_of(&version);
        if declared != security_epoch {
            return Err(format!(
                "release version `{version}` has Debian epoch {declared}, but the \
                 security epoch is {security_epoch}; APT would not order this \
                 release above the versions it must supersede"
            ));
        }
        let architecture = token_field(release, "architecture")?;
        let suite = token_field(release, "suite")?;
        let component = token_field(release, "component")?;

        let issued_at = timestamp_field(object, "issued_at")?;
        let valid_until = timestamp_field(object, "valid_until")?;
        if valid_until <= issued_at {
            return Err("release manifest expires before it was issued".to_string());
        }

        let mut protocols = BTreeMap::new();
        let protocol_object = object
            .get("protocols")
            .and_then(Value::as_object)
            .ok_or_else(|| "release manifest has no `protocols` object".to_string())?;
        for (name, raw) in protocol_object {
            require_token(name, "protocol name")?;
            let epoch = raw
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| format!("protocol `{name}` is not an unsigned integer"))?;
            protocols.insert(name.clone(), epoch);
        }
        if protocols.is_empty() {
            return Err("release manifest declares no protocol epochs".to_string());
        }

        let mut minimum_compatible = BTreeMap::new();
        let minimum_object = object
            .get("minimum_compatible")
            .and_then(Value::as_object)
            .ok_or_else(|| "release manifest has no `minimum_compatible` object".to_string())?;
        for (name, raw) in minimum_object {
            require_token(name, "package name")?;
            let floor = raw.as_str().ok_or_else(|| {
                format!("minimum compatible version for `{name}` is not a string")
            })?;
            if !debver::is_valid(floor) {
                return Err(format!(
                    "minimum compatible version for `{name}` is not a Debian version"
                ));
            }
            minimum_compatible.insert(name.clone(), floor.to_string());
        }

        let mut components = Vec::new();
        let component_array = object
            .get("components")
            .and_then(Value::as_array)
            .ok_or_else(|| "release manifest has no `components` array".to_string())?;
        let mut seen = BTreeSet::new();
        for entry in component_array {
            let entry = entry
                .as_object()
                .ok_or_else(|| "component entry is not an object".to_string())?;
            let name = token_field(entry, "name")?;
            let path = string_field(entry, "path")?;
            if !path.starts_with('/') || path.contains("..") || path.contains('\\') {
                return Err(format!("component `{name}` has a suspicious path"));
            }
            let sha256 = digest_field(entry, "sha256")?;
            if !seen.insert(name.clone()) {
                return Err(format!("component `{name}` is listed twice"));
            }
            components.push(ComponentDigest { name, path, sha256 });
        }
        if components.is_empty() {
            return Err("release manifest lists no components".to_string());
        }

        let revoked_digests = digest_set(object, "revoked_digests")?;
        let revoked_keys = key_set(object, "revoked_keys")?;

        Ok(Self {
            security_epoch,
            abi,
            package,
            version,
            architecture,
            suite,
            component,
            issued_at,
            valid_until,
            protocols,
            minimum_compatible,
            components,
            revoked_digests,
            revoked_keys,
            digest: crate::crypto::sha256_hex(bytes),
            bytes: bytes.to_vec(),
        })
    }

    /// Digest recorded for one component, if the manifest lists it.
    pub fn component_digest(&self, name: &str) -> Option<&ComponentDigest> {
        self.components.iter().find(|entry| entry.name == name)
    }

    /// Expired relative to `now`.
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        now > self.valid_until
    }
}

fn string_field(object: &serde_json::Map<String, Value>, key: &str) -> Result<String, String> {
    let raw = object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("release manifest field `{key}` is missing or not a string"))?;
    if raw.is_empty() || raw.len() > 512 {
        return Err(format!(
            "release manifest field `{key}` has an invalid length"
        ));
    }
    if raw.bytes().any(|byte| !(0x20..0x7f).contains(&byte)) {
        return Err(format!(
            "release manifest field `{key}` contains non-printable ASCII"
        ));
    }
    Ok(raw.to_string())
}

/// A conservative identifier: what package names, suites, components,
/// architectures and protocol names are allowed to contain.
fn token_field(object: &serde_json::Map<String, Value>, key: &str) -> Result<String, String> {
    let raw = string_field(object, key)?;
    require_token(&raw, key)?;
    Ok(raw)
}

fn require_token(raw: &str, what: &str) -> Result<(), String> {
    if raw.len() > 128
        || !raw
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+'))
    {
        return Err(format!("`{raw}` is not a valid {what}"));
    }
    Ok(())
}

fn u64_field(object: &serde_json::Map<String, Value>, key: &str) -> Result<u64, String> {
    object
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("release manifest field `{key}` is not an unsigned integer"))
}

fn timestamp_field(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<DateTime<Utc>, String> {
    let raw = string_field(object, key)?;
    DateTime::parse_from_rfc3339(&raw)
        .map(|parsed| parsed.with_timezone(&Utc))
        .map_err(|_| format!("release manifest field `{key}` is not an RFC 3339 timestamp"))
}

fn digest_field(object: &serde_json::Map<String, Value>, key: &str) -> Result<String, String> {
    let raw = string_field(object, key)?;
    require_digest(&raw)?;
    Ok(raw)
}

/// Digests are always lowercase hex SHA-256 so a comparison is a byte
/// comparison and no caller has to normalize.
pub fn require_digest(raw: &str) -> Result<(), String> {
    if raw.len() != 64 || !raw.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("`{raw}` is not a SHA-256 digest"));
    }
    if raw.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return Err("digests must be lowercase hexadecimal".to_string());
    }
    Ok(())
}

fn digest_set(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<BTreeSet<String>, String> {
    let mut set = BTreeSet::new();
    let Some(array) = object.get(key) else {
        return Ok(set);
    };
    let array = array
        .as_array()
        .ok_or_else(|| format!("release manifest field `{key}` is not an array"))?;
    for entry in array {
        let raw = entry
            .as_str()
            .ok_or_else(|| format!("release manifest field `{key}` has a non-string entry"))?;
        require_digest(raw)?;
        set.insert(raw.to_string());
    }
    Ok(set)
}

fn key_set(object: &serde_json::Map<String, Value>, key: &str) -> Result<BTreeSet<String>, String> {
    let mut set = BTreeSet::new();
    let Some(array) = object.get(key) else {
        return Ok(set);
    };
    let array = array
        .as_array()
        .ok_or_else(|| format!("release manifest field `{key}` is not an array"))?;
    for entry in array {
        let raw = entry
            .as_str()
            .ok_or_else(|| format!("release manifest field `{key}` has a non-string entry"))?;
        set.insert(super::signature::normalize_key_id(raw)?);
    }
    Ok(set)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/update/manifest.rs"
    ));
}
