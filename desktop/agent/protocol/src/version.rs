use serde::{Deserialize, Serialize};

/// Request/response header used to negotiate the presentation protocol.
pub const PROTOCOL_VERSION_HEADER: &str = "x-clawos-agent-protocol-version";
pub const CURRENT_PROTOCOL_VERSION: u16 = 1;
pub const MIN_SUPPORTED_PROTOCOL_VERSION: u16 = 1;
pub const CURRENT_PROTOCOL_VERSION_HEADER_VALUE: &str = "1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProtocolVersion(pub u16);

impl ProtocolVersion {
    pub const CURRENT: Self = Self(CURRENT_PROTOCOL_VERSION);

    pub const fn is_supported(self) -> bool {
        self.0 >= MIN_SUPPORTED_PROTOCOL_VERSION && self.0 <= CURRENT_PROTOCOL_VERSION
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
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/version.rs"));
}
