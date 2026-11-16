//! Capability-gated controls for the same live, managed native playback UI.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(test)]
use serde_json::json;
use serde_json::Value;

use super::authority::{Authorized, Decision};
use super::client_identity::ClientIdentity;
use super::wire::requests::{MediaPlayerAction, MediaPlayerControl};
use crate::caps::{Cap, Scope, Verb};

pub(crate) mod gui;
#[cfg(test)]
mod mpris;

const MAX_CALL: Duration = Duration::from_secs(5);
const MAX_OUTPUT: usize = 64 * 1024;

fn required_cap(action: MediaPlayerAction) -> Cap {
    Cap::new(
        if action == MediaPlayerAction::Status {
            Verb::DESKTOP_MEDIA_OBSERVE
        } else {
            Verb::DESKTOP_MEDIA_CONTROL
        },
        Scope::name("cosmic-player"),
    )
}

fn authorize(
    authority: &Decision,
    action: MediaPlayerAction,
    owner: u32,
) -> Result<Authorized, String> {
    if authority.owner_uid() != owner {
        return Err("Media Player owner mismatch".into());
    }
    authority.require(required_cap(action))
}

fn remaining(deadline: u64) -> Result<Duration, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock precedes the Unix epoch")?
        .as_millis();
    let left = u128::from(deadline)
        .checked_sub(now)
        .filter(|left| *left > 0)
        .ok_or("Media Player deadline expired")?;
    Ok(Duration::from_millis(left.min(MAX_CALL.as_millis()) as u64))
}

pub async fn control(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, String> {
    let request: MediaPlayerControl = serde_json::from_value(params)
        .map_err(|error| format!("invalid Media Player request: {error}"))?;
    let uid = client.require_uid()?;
    let _authorized = authorize(authority, request.action, uid)?;
    let timeout = remaining(request.deadline_unix_ms)?;
    if unsafe { libc::geteuid() } != 0 || uid == 0 {
        return Err("Media Player requires root clawd and a non-root owner session".into());
    }
    let window = super::gui::Manager::get()?.media_window(uid, "cosmic-player")?;
    window
        .execute(request.action, timeout, || {
            authorize(authority, request.action, uid).map(|_| ())
        })
        .await
}

pub fn helper(_args: &[String]) -> Result<Value, String> {
    Err("the owner-bus Media Player helper is retired; use the managed playback service".into())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/media_player.rs"
    ));
}
