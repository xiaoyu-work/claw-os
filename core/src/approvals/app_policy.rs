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

/// Durable consent to remove one deny gate, not a redeemable execution grant.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestorationBinding {
    pub authorization: super::ApprovalAuthorization,
    pub generation: u32,
    pub reference: String,
}

impl Block {
    pub fn session(&self, uid: u32, app: &str) -> String {
        let digest = Sha256::digest(serde_json::to_vec(&self.cap).expect("cap serialization"));
        format!("{SESSION_PREFIX}{uid}:{app}:{}:{digest:x}", self.generation)
    }

    pub fn enabled(&self, uid: u32, app: &str) -> Result<bool, String> {
        super::ensure_dirs().map_err(|error| format!("approvals dir: {error}"))?;
        crate::filelock::with_exclusive_path_lock(&super::grant_lock_path(), || {
            // A reader holding an older snapshot cannot revive it after re-revoke.
            if !blocks(uid, app)?.contains(self) {
                return Ok(false);
            }
            let session = self.session(uid, app);
            let generation = super::generations::current(Some(uid), &session)?;
            let (capability, risk) =
                super::canonical_capability(self.cap.verb, self.cap.scope.clone())?;
            let expected = super::ApprovalAuthorization {
                owner_uid: Some(uid),
                session,
                capability,
                risk,
                context: None,
                execution: None,
                resume_request_id: None,
                resumable_until: None,
                operation_digest: None,
            };
            let entries = std::fs::read_dir(super::approved_dir())
                .map_err(|error| format!("read App permission restoration receipts: {error}"))?;
            let mut enabled = false;
            for entry in entries {
                let path = entry
                    .map_err(|error| format!("read approval entry: {error}"))?
                    .path();
                if path
                    .extension()
                    .is_some_and(|extension| extension == "json")
                {
                    enabled |= matching_receipt(&path, &expected, generation)?;
                }
            }
            Ok(enabled)
        })
    }
}

fn matching_receipt(
    path: &std::path::Path,
    expected: &super::ApprovalAuthorization,
    generation: u32,
) -> Result<bool, String> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| format!("read approval receipt {}: {error}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("inspect approval receipt: {error}"))?;
    if !metadata.is_file()
        || metadata.mode() & 0o022 != 0
        || metadata.nlink() != 1
        || (unsafe { libc::geteuid() } == 0 && metadata.uid() != 0)
    {
        return Err("App permission receipt must be a private, root-owned regular file".into());
    }
    let resolved: super::Resolved = serde_json::from_reader(file)
        .map_err(|error| format!("parse approval receipt {}: {error}", path.display()))?;
    if resolved.request.session != expected.session
        || resolved.request.owner_uid != expected.owner_uid
        || resolved.request.risk != Some(expected.risk)
        || resolved.decision.outcome != super::Outcome::Approved
        || resolved.decision.duration != Some(super::GrantDuration::Forever)
        || path.file_stem() != Some(std::ffi::OsStr::new(&resolved.request.id))
    {
        return Ok(false);
    }
    if super::authorization_for_request(&resolved.request).as_ref() != Ok(expected) {
        return Ok(false);
    }
    let (authorization, approved_generation) =
        match (&resolved.decision.restoration, &resolved.decision.grant) {
            (Some(binding), None) => (&binding.authorization, Some(binding.generation)),
            // Upgrade the interpretation, not the files: old Settings approvals
            // already bind the exact owner/App/capability and both generations.
            // Their execution TTL/use budget never described the deny gate.
            (None, Some(binding)) => {
                let Some(authorization) = &binding.authorization else {
                    return Ok(false);
                };
                (authorization, binding.generation)
            }
            _ => return Ok(false),
        };
    Ok(authorization == expected && approved_generation == Some(generation))
}

/// Recheck under the approval store lock as well as at the trusted broker
/// entry point, so a revocation cannot race a previously validated decision.
pub(crate) fn validate_request(request: &super::Request) -> Result<(u32, String), String> {
    let uid = request
        .owner_uid
        .ok_or("App permission approval has no owner")?;
    if request.context.is_some()
        || request.execution.is_some()
        || request.resumable_until.is_some()
        || request.operation_digest.is_some()
    {
        return Err("App permission restoration cannot carry execution authority".into());
    }
    let app = request
        .session
        .strip_prefix(SESSION_PREFIX)
        .and_then(|rest| rest.split(':').nth(1))
        .ok_or("invalid App permission session")?;
    if !blocks(uid, app)?.iter().any(|block| {
        supported(block.cap.verb)
            && block.session(uid, app) == request.session
            && block.cap.verb.as_str() == request.verb
            && block.cap.scope == request.scope
    }) {
        return Err(
            "App permission request is stale or does not match the owner/App policy".into(),
        );
    }
    Ok((uid, app.to_string()))
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
            | Verb::DESKTOP_CAPTURE
            | Verb::DESKTOP_MEDIA_OBSERVE
            | Verb::DESKTOP_MEDIA_CONTROL
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
        return Err("this permission requires containment of direct resources and cannot yet be managed".into());
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
