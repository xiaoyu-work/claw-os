use serde::{Deserialize, Serialize};

/// Request/response header used to negotiate the presentation protocol.
pub const PROTOCOL_VERSION_HEADER: &str = "x-clawos-agent-protocol-version";
/// Response header advertising the oldest protocol accepted after refusal.
pub const PROTOCOL_MIN_VERSION_HEADER: &str = "x-clawos-agent-min-protocol-version";
pub const CURRENT_PROTOCOL_VERSION: u16 = 1;
pub const MIN_SUPPORTED_PROTOCOL_VERSION: u16 = 1;
pub const CURRENT_PROTOCOL_VERSION_HEADER_VALUE: &str = "1";
pub const MIN_SUPPORTED_PROTOCOL_VERSION_HEADER_VALUE: &str = "1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProtocolVersion(pub u16);

impl ProtocolVersion {
    pub const CURRENT: Self = Self(CURRENT_PROTOCOL_VERSION);

    pub const fn is_supported(self) -> bool {
        ProtocolMetadata::CURRENT.contains(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolMetadata {
    pub protocol_version: ProtocolVersion,
    pub min_protocol_version: ProtocolVersion,
}

impl ProtocolMetadata {
    pub const CURRENT: Self = Self {
        protocol_version: ProtocolVersion(CURRENT_PROTOCOL_VERSION),
        min_protocol_version: ProtocolVersion(MIN_SUPPORTED_PROTOCOL_VERSION),
    };

    pub const fn is_valid(self) -> bool {
        self.min_protocol_version.0 <= self.protocol_version.0
    }

    pub const fn contains(self, version: ProtocolVersion) -> bool {
        self.is_valid()
            && version.0 >= self.min_protocol_version.0
            && version.0 <= self.protocol_version.0
    }

    /// Select the highest version in the intersection of two ranges.
    pub const fn negotiate_highest(self, peer: Self) -> Option<ProtocolVersion> {
        if !self.is_valid() || !peer.is_valid() {
            return None;
        }
        let minimum = if self.min_protocol_version.0 > peer.min_protocol_version.0 {
            self.min_protocol_version.0
        } else {
            peer.min_protocol_version.0
        };
        let maximum = if self.protocol_version.0 < peer.protocol_version.0 {
            self.protocol_version.0
        } else {
            peer.protocol_version.0
        };
        if minimum <= maximum {
            Some(ProtocolVersion(maximum))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/version.rs"));
}
