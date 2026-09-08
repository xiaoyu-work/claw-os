//! Typed, non-interactive screenshot service. No App call or desktop socket.

use crate::BridgeError;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Serialize)]
struct Request<'a> {
    directory: &'a str,
    modal: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Outcome {
    pub cancelled: bool,
    pub path: Option<String>,
}

pub fn screenshot(directory: &str, modal: bool) -> Result<Outcome, BridgeError> {
    request(directory, modal, false)
}

/// A direct human CLI may retain its existing terminal for OS session
/// bootstrap. MCP always uses `screenshot`; this does not grant authority.
pub fn screenshot_from_terminal(directory: &str, modal: bool) -> Result<Outcome, BridgeError> {
    request(directory, modal, true)
}

fn request(directory: &str, modal: bool, terminal: bool) -> Result<Outcome, BridgeError> {
    if directory.is_empty() || directory.contains('\0') || !Path::new(directory).is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "capture directory must be an absolute path without NUL",
        )
        .into());
    }
    let input = serde_json::to_vec(&Request { directory, modal }).map_err(std::io::Error::other)?;
    let args = ["__capture", "screenshot", "--request-stdin"];
    let value = if terminal {
        claw_os_sdk::cos_call_json_with_stdin_terminal("capture", "screenshot", args, &input)
    } else {
        claw_os_sdk::cos_call_json_with_stdin("capture", "screenshot", args, &input)
    }?;
    let outcome: Outcome = serde_json::from_value(value).map_err(|error| BridgeError::Decode {
        app: "capture".into(),
        verb: "screenshot".into(),
        message: error.to_string(),
    })?;
    let valid = match (outcome.cancelled, outcome.path.as_deref()) {
        (true, None) => true,
        (false, Some(path)) => {
            Path::new(path).parent() == Some(Path::new(directory))
                && Path::new(path)
                    .extension()
                    .is_some_and(|extension| extension == "png")
                && !path.contains('\0')
        }
        _ => false,
    };
    if !valid {
        return Err(BridgeError::Decode {
            app: "capture".into(),
            verb: "screenshot".into(),
            message: "capture provider returned an inconsistent outcome or destination".into(),
        });
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/capture.rs"));
}
