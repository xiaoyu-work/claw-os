//! Owner/App attenuation stored with the existing root-owned approval generations.
//! Restoration removes a deny gate, never the manifest or launcher's ceiling.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::caps::{Cap, CapSet, Verb};

pub const SESSION_PREFIX: &str = "app-permission:";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    pub cap: Cap,
    pub generation: u32,
}

impl Block {
    pub fn session(&self, uid: u32, app: &str) -> String {
        let digest = Sha256::digest(serde_json::to_vec(&self.cap).expect("cap serialization"));
        format!("{SESSION_PREFIX}{uid}:{app}:{}:{digest:x}", self.generation)
    }

    pub fn enabled(&self, uid: u32, app: &str) -> Result<bool, String> {
        super::has_approved_grant_for_owner(&self.session(uid, app), &self.cap, Some(uid))
    }
}

/// Only broker-mediated verbs are controllable. Filesystem mounts, native
/// D-Bus authority and direct egress require process containment, not this gate.
pub fn supported(verb: Verb) -> bool {
    matches!(
        verb,
        Verb::SYS_OBSERVE
            | Verb::UI_ACCESSIBILITY
            | Verb::DEVICE_AUDIO
            | Verb::DEVICE_MICROPHONE
            | Verb::DEVICE_MEDIA_ROUTE
            | Verb::DEVICE_BLUETOOTH
            | Verb::DEVICE_CAMERA
            | Verb::DEVICE_DISPLAY
            | Verb::DESKTOP_WINDOW
            | Verb::DEVICE_LOCATION
            | Verb::NET_MANAGE
            | Verb::SYS_POWER
            | Verb::DEVICE_PRINTER
            | Verb::SYS_IDENTITY
    )
}

pub fn blocks(uid: u32, app: &str) -> Result<Vec<Block>, String> {
    super::generations::app_blocks(uid, app)
}

pub fn revoke(uid: u32, app: &str, cap: Cap) -> Result<Block, String> {
    if !supported(cap.verb) {
        return Err("this permission requires containment of direct resources and cannot yet be changed in Settings".into());
    }
    super::generations::block_app_cap(uid, app, cap)
}

pub fn require(uid: u32, app: &str, caps: &CapSet) -> Result<(), String> {
    for block in blocks(uid, app)? {
        if caps
            .iter()
            .any(|cap| block.cap.covers(cap) || cap.covers(&block.cap))
            && !block.enabled(uid, app)?
        {
            return Err(format!(
                "App `{app}` permission {} {} is revoked; request restoration in Settings",
                block.cap.verb, block.cap.scope
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/approvals/app_policy.rs"
    ));
}
