//! The stored record and the bytes its MAC covers.
//!
//! A record is one line of `events.jsonl`. Its MAC is computed over an
//! unambiguous, length-prefixed encoding rather than over the JSON
//! text, so no amount of whitespace, key reordering or unicode
//! escaping changes what was signed, and no field can be moved into
//! another field's bytes.
//!
//! ```text
//! LP("cos.session.journal")  domain separation
//! U32(schema)               refuse a version this daemon does not know
//! LP(partition)             which chain this belongs to
//! U32(owner_uid)            whose chain it is
//! U64(seq)                  strictly increasing within the partition
//! U64(epoch)                writer lease that produced it
//! U64(recorded_at_ms)       daemon clock, evidence only
//! LP(source)                kernel / worker / recovery
//! LP(event kind)            the ACL-relevant name
//! LP(body digest)           SHA-256 of the serialized event
//! LP(previous MAC)          the chain link
//! LP(key id)                which key signs this record
//! ```
//!
//! `LP(x)` is a big-endian `u64` length followed by the bytes, so two
//! different field splits can never produce the same input.
//!
//! The body digest — not the body — is signed. That keeps the MAC
//! input a fixed size, and any change to a field changes the digest.
//! A reader recomputes the digest from the typed event it decoded, so a
//! record decorated with fields the schema does not declare is
//! reduced to its declared meaning before anything trusts it.

use serde::{Deserialize, Serialize};

use super::acl::EventSource;
use super::event::{JournalEvent, SCHEMA_VERSION};
use super::JournalError;

/// Domain separator, so a journal MAC can never be confused with a
/// grant, an anchor or an audit digest computed elsewhere.
const DOMAIN: &[u8] = b"cos.session.journal";

/// `prev` for the first record in a partition.
pub const GENESIS_MAC: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Longest stored line the reader will accept.
pub const MAX_RECORD_BYTES: usize = 8 * 1024;

/// One authoritative, signed line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalRecord {
    /// Schema version. Bound into the MAC.
    pub v: u32,
    /// Strictly increasing within the partition, starting at 1.
    pub seq: u64,
    /// Writer lease epoch that produced the record.
    pub epoch: u64,
    /// Daemon clock in milliseconds. Evidence, never ordering: `seq`
    /// is what orders the chain.
    pub recorded_at_ms: u64,
    pub partition: String,
    pub owner_uid: u32,
    pub source: EventSource,
    pub key_id: String,
    /// MAC of the previous record, or [`GENESIS_MAC`].
    pub prev: String,
    pub mac: String,
    pub event: JournalEvent,
}

/// Everything the MAC covers except the key.
pub struct Preimage<'a> {
    pub schema: u32,
    pub partition: &'a str,
    pub owner_uid: u32,
    pub seq: u64,
    pub epoch: u64,
    pub recorded_at_ms: u64,
    pub source: EventSource,
    pub event: &'a JournalEvent,
    pub prev: &'a str,
    pub key_id: &'a str,
}

impl Preimage<'_> {
    fn encode(&self) -> Result<Vec<u8>, JournalError> {
        let body = serde_json::to_vec(self.event)
            .map_err(|error| JournalError::Encode(format!("encode journal event: {error}")))?;
        let body_digest = crate::crypto::sha256_hex(&body);

        let mut out = Vec::with_capacity(256);
        push_bytes(&mut out, DOMAIN);
        out.extend_from_slice(&self.schema.to_be_bytes());
        push_bytes(&mut out, self.partition.as_bytes());
        out.extend_from_slice(&self.owner_uid.to_be_bytes());
        out.extend_from_slice(&self.seq.to_be_bytes());
        out.extend_from_slice(&self.epoch.to_be_bytes());
        out.extend_from_slice(&self.recorded_at_ms.to_be_bytes());
        push_bytes(&mut out, self.source.as_str().as_bytes());
        push_bytes(&mut out, self.event.kind().as_bytes());
        push_bytes(&mut out, body_digest.as_bytes());
        push_bytes(&mut out, self.prev.as_bytes());
        push_bytes(&mut out, self.key_id.as_bytes());
        Ok(out)
    }

    /// The MAC for this preimage under `key`.
    pub fn seal(&self, key: &[u8]) -> Result<String, JournalError> {
        Ok(crate::crypto::hmac_sha256_hex(key, &self.encode()?))
    }
}

fn push_bytes(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u64).to_be_bytes());
    out.extend_from_slice(value);
}

impl JournalRecord {
    /// Recompute this record's MAC under `key` and compare it in
    /// constant time.
    pub fn verify(&self, key: &[u8]) -> Result<(), JournalError> {
        if self.v != SCHEMA_VERSION {
            return Err(JournalError::Integrity(format!(
                "journal record at seq {} declares schema {}; this daemon knows {SCHEMA_VERSION}",
                self.seq, self.v
            )));
        }
        let expected = Preimage {
            schema: self.v,
            partition: &self.partition,
            owner_uid: self.owner_uid,
            seq: self.seq,
            epoch: self.epoch,
            recorded_at_ms: self.recorded_at_ms,
            source: self.source,
            event: &self.event,
            prev: &self.prev,
            key_id: &self.key_id,
        }
        .seal(key)?;
        if constant_time_eq(expected.as_bytes(), self.mac.as_bytes()) {
            Ok(())
        } else {
            Err(JournalError::Integrity(format!(
                "journal record at seq {} does not match its MAC",
                self.seq
            )))
        }
    }

    /// Serialize to the exact line stored on disk.
    pub fn encode_line(&self) -> Result<String, JournalError> {
        let line = serde_json::to_string(self)
            .map_err(|error| JournalError::Encode(format!("encode journal record: {error}")))?;
        if line.len() > MAX_RECORD_BYTES {
            return Err(JournalError::Quota(format!(
                "journal record is {} bytes; the ceiling is {MAX_RECORD_BYTES}",
                line.len()
            )));
        }
        Ok(line)
    }

    /// Decode one stored line.
    ///
    /// `deny_unknown_fields` on the record means a line decorated with
    /// extra top-level keys is refused outright rather than silently
    /// reduced, so an injected record cannot smuggle a payload past a
    /// reader that only looks at declared fields.
    pub fn decode_line(line: &str) -> Result<Self, JournalError> {
        if line.len() > MAX_RECORD_BYTES {
            return Err(JournalError::Integrity(format!(
                "journal line is {} bytes; the ceiling is {MAX_RECORD_BYTES}",
                line.len()
            )));
        }
        serde_json::from_str(line)
            .map_err(|error| JournalError::Integrity(format!("journal line is unusable: {error}")))
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/session/journal/record.rs"
    ));
}
