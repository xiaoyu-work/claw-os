//! Session identifier.
//!
//! Format: `ses_<13 hex chars of unix-millis>_<12 hex chars of random>`.
//!
//! The timestamp prefix makes `ls /var/lib/cos/sessions/` sort
//! chronologically, which is what every CLI and GUI surface wants by
//! default. The random suffix uses [`uuid::Uuid::new_v4`] truncated to
//! 48 bits — enough to avoid collisions in any realistic per-machine
//! workload while keeping the id short enough to paste into a terminal.
//!
//! IDs are opaque to consumers: never parse the prefix to derive a
//! creation time; read [`crate::session::SessionMeta`]`.created_at`
//! instead. The format is internal and may change.

use std::fmt;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Stable handle to a durable session. Cheap to clone.
///
/// Newtype around a `String` so callers cannot accidentally pass a raw
/// path component or a PID-bound short-session id where a durable
/// session id is required.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(String);

impl SessionId {
    /// Mint a fresh id from the current wall clock + a v4 uuid.
    pub fn generate() -> Self {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        // 48 random bits from a v4 uuid is plenty: per-millisecond
        // collision odds stay negligible at any realistic agent
        // throughput on a single machine.
        let rand = uuid::Uuid::new_v4().as_u128() as u64 & 0x0000_FFFF_FFFF_FFFF;
        Self(format!("ses_{:013x}_{:012x}", millis, rand))
    }

    /// Borrow as a `&str`. Suitable for path joins and logging.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume into the underlying String.
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for SessionId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Validation: the id must look like one [`SessionId::generate`]
/// produced. This is the **only** entry point that turns an untrusted
/// string into a [`SessionId`] — the api socket, CLI, and any callers
/// receiving an id from outside must go through this. It rejects path
/// traversal, whitespace, and anything that isn't the canonical shape.
impl FromStr for SessionId {
    type Err = InvalidSessionId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // "ses_" + 13 hex + "_" + 12 hex
        if s.len() != 4 + 13 + 1 + 12 {
            return Err(InvalidSessionId);
        }
        if !s.starts_with("ses_") {
            return Err(InvalidSessionId);
        }
        let rest = &s[4..];
        let (ts, sep_rand) = rest.split_at(13);
        if !ts.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(InvalidSessionId);
        }
        let mut sep_rand_chars = sep_rand.chars();
        if sep_rand_chars.next() != Some('_') {
            return Err(InvalidSessionId);
        }
        if !sep_rand_chars.all(|c| c.is_ascii_hexdigit()) {
            return Err(InvalidSessionId);
        }
        Ok(Self(s.to_string()))
    }
}

/// Returned by [`SessionId::from_str`] when the input is not a
/// canonical session id. We deliberately do not echo the bad input
/// back: untrusted callers should not be able to make us reflect
/// attacker-controlled bytes into logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidSessionId;

impl fmt::Display for InvalidSessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid session id")
    }
}

impl std::error::Error for InvalidSessionId {}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/session/id.rs"
    ));
}
