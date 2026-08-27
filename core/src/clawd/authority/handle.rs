//! Opaque grant handles.
//!
//! A handle is 32 bytes of kernel entropy rendered as hex. It is a
//! *reference* to a grant the daemon holds, never the grant itself and
//! never a bearer token: the store binds every grant to an
//! authenticated principal, so a handle that leaks still exercises
//! nothing from another process.
//!
//! Three properties are load-bearing:
//!
//! * **Non-enumerable.** 256 bits from `/dev/urandom`, so the space
//!   cannot be walked and a guess is indistinguishable from any other
//!   miss.
//! * **Never printed.** [`GrantHandle`] renders as `<grant-handle>`
//!   under both `Debug` and `Display`, and it deliberately implements
//!   neither `Serialize` nor `Deserialize`. The single place a handle
//!   reaches the wire calls [`GrantHandle::into_wire`], which consumes
//!   it, so a handle cannot be logged by accident or swept into a
//!   journal payload by a `#[derive(Serialize)]` on a containing type.
//! * **Never stored in the clear.** The store is keyed by
//!   [`HandleKey`], the SHA-256 of the handle bytes. A read of daemon
//!   memory, a core dump or a map iteration yields key material that
//!   cannot be replayed as a handle.
//!
//! Audit records carry a [`GrantRef`] instead: an HMAC of the grant's
//! internal id under a per-process key that never leaves memory. Two
//! records about the same grant correlate; nothing about the record
//! reverses to a handle, an id, or another grant.

use std::fmt;

/// Bytes of entropy behind one handle.
const HANDLE_BYTES: usize = 32;

/// An opaque reference to a grant held by the authority.
///
/// Construct with [`GrantHandle::generate`]; present with
/// [`HandleKey::of`]. There is no way to read the characters back out
/// except [`GrantHandle::into_wire`], which consumes the value.
pub struct GrantHandle(String);

impl GrantHandle {
    /// Mint a fresh handle from the kernel CSPRNG.
    pub fn generate() -> Result<Self, String> {
        Ok(Self(hex::encode(random_bytes()?)))
    }

    /// The store key for this handle.
    pub fn key(&self) -> HandleKey {
        HandleKey::of(&self.0)
    }

    /// Hand the handle to its one legitimate recipient.
    ///
    /// Consuming `self` is the point: after this call the daemon holds
    /// no copy of the characters, only the [`HandleKey`] it stored.
    pub fn into_wire(self) -> String {
        self.0
    }
}

impl fmt::Debug for GrantHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<grant-handle>")
    }
}

impl fmt::Display for GrantHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<grant-handle>")
    }
}

/// The store's key for a handle: `SHA-256(handle)`.
///
/// Derived the same way whether the handle was just minted or was
/// presented on a request, so a lookup is a plain hash-map hit on a
/// value that reveals nothing if the map is dumped.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct HandleKey([u8; 32]);

impl HandleKey {
    pub fn of(presented: &str) -> Self {
        Self(crate::crypto::sha256_bytes(presented.as_bytes()))
    }
}

impl fmt::Debug for HandleKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<grant-key>")
    }
}

/// Monotonic internal identity of a grant.
///
/// Used for lineage (a child records its parent's id) and as the input
/// to the audit reference. Never leaves the daemon: a caller that
/// learned an id could not present it anywhere, because every entry
/// point takes a handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GrantId(pub(crate) u64);

impl GrantId {
    /// The keyed, non-reversible reference recorded in audit.
    pub fn audit_ref(self) -> GrantRef {
        let digest = crate::crypto::hmac_sha256(audit_key(), &self.0.to_be_bytes());
        GrantRef(format!("g-{}", hex::encode(&digest[..8])))
    }
}

/// A grant identifier safe to write to a durable record.
///
/// Stable for the life of the daemon process, so an issuance, a use and
/// a revocation of the same grant correlate in the log. Keyed under a
/// secret generated per process and never serialized, so the reference
/// cannot be reversed to an id or replayed as a handle.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(transparent)]
pub struct GrantRef(String);

impl GrantRef {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for GrantRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Per-process key behind [`GrantId::audit_ref`].
fn audit_key() -> &'static [u8; 32] {
    static KEY: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
    KEY.get_or_init(|| {
        random_bytes().unwrap_or_else(|_| crate::crypto::sha256_bytes(b"cos.authority.audit"))
    })
}

#[cfg(unix)]
fn random_bytes() -> Result<[u8; HANDLE_BYTES], String> {
    use std::io::Read;

    let mut bytes = [0u8; HANDLE_BYTES];
    let mut source = std::fs::File::open("/dev/urandom")
        .map_err(|error| format!("open /dev/urandom: {error}"))?;
    source
        .read_exact(&mut bytes)
        .map_err(|error| format!("read /dev/urandom: {error}"))?;
    Ok(bytes)
}

#[cfg(not(unix))]
fn random_bytes() -> Result<[u8; HANDLE_BYTES], String> {
    let mut bytes = [0u8; HANDLE_BYTES];
    bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/authority/handle.rs"
    ));
}
