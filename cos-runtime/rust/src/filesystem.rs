//! Controlled filesystem service for App handlers; never dispatches an App.

use crate::BridgeError;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum Request<'a> {
    Read {
        path: &'a str,
    },
    Write {
        path: &'a str,
        content: &'a str,
    },
    Replace {
        path: &'a str,
        find: &'a str,
        replace: &'a str,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadResult {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteResult {
    pub path: String,
    pub bytes: u64,
}

fn request<T: serde::de::DeserializeOwned>(
    route: &str,
    input: Request<'_>,
) -> Result<T, BridgeError> {
    let input = serde_json::to_vec(&input).map_err(std::io::Error::other)?;
    let value = claw_os_sdk::cos_call_json_with_stdin(
        "filesystem",
        route,
        ["__filesystem", route, "--request-stdin"],
        &input,
    )?;
    serde_json::from_value(value).map_err(|error| BridgeError::Decode {
        app: "filesystem".into(),
        verb: route.into(),
        message: error.to_string(),
    })
}

fn absolute(path: &str) -> Result<String, BridgeError> {
    if path.is_empty() || path.contains('\0') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "file path must be nonempty and contain no NUL",
        )
        .into());
    }
    let path = std::path::absolute(path)?;
    path.to_str().map(str::to_string).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "file path must be UTF-8").into()
    })
}

/// Read a complete, bounded UTF-8 file. Oversize and invalid UTF-8 are errors.
pub fn read(path: &str) -> Result<ReadResult, BridgeError> {
    request(
        "read",
        Request::Read {
            path: &absolute(path)?,
        },
    )
}

/// Atomically replace a file, recording its previous bytes in the task session.
/// The parent must exist; no broader directory capability is synthesized.
pub fn write(path: &str, content: &str) -> Result<WriteResult, BridgeError> {
    request(
        "write",
        Request::Write {
            path: &absolute(path)?,
            content,
        },
    )
}

/// Replace exactly one match in a complete UTF-8 file, within one broker call.
pub fn replace(path: &str, find: &str, replacement: &str) -> Result<u64, BridgeError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Result {
        replacements: u64,
    }
    let result: Result = request(
        "write",
        Request::Replace {
            path: &absolute(path)?,
            find,
            replace: replacement,
        },
    )?;
    if result.replacements != 1 {
        return Err(BridgeError::Decode {
            app: "filesystem".into(),
            verb: "replace".into(),
            message: "provider did not confirm one replacement".into(),
        });
    }
    Ok(result.replacements)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/filesystem.rs"
    ));
}
