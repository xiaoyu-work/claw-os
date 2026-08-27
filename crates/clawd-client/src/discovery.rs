use std::path::{Path, PathBuf};

use crate::ClientError;

/// Canonical socket override, shared with `clawd`.
pub const SOCKET_ENV: &str = "CLAWD_SOCKET";
/// Compatibility alias used by older desktop consumers.
pub const COMPAT_SOCKET_ENV: &str = "COS_CLAWD_SOCKET";
pub const RUNTIME_ENV: &str = "COS_RUNTIME_DIR";
pub const DEFAULT_SOCKET_PATH: &str = "/run/cos/clawd.sock";

/// Resolve exactly one socket before connecting.
///
/// The canonical override wins over the compatibility alias, which wins over
/// `COS_RUNTIME_DIR`. Once a variable is present, an empty value is an error;
/// the client never silently falls through to another socket.
pub fn discover_socket() -> Result<PathBuf, ClientError> {
    if let Some(value) = std::env::var_os(SOCKET_ENV) {
        return configured_path(SOCKET_ENV, PathBuf::from(value));
    }
    if let Some(value) = std::env::var_os(COMPAT_SOCKET_ENV) {
        return configured_path(COMPAT_SOCKET_ENV, PathBuf::from(value));
    }
    if let Some(value) = std::env::var_os(RUNTIME_ENV) {
        let runtime = configured_path(RUNTIME_ENV, PathBuf::from(value))?;
        return Ok(runtime.join("clawd.sock"));
    }
    Ok(Path::new(DEFAULT_SOCKET_PATH).to_path_buf())
}

fn configured_path(variable: &'static str, path: PathBuf) -> Result<PathBuf, ClientError> {
    if path.as_os_str().is_empty() {
        Err(ClientError::EmptySocketConfiguration { variable })
    } else {
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/discovery.rs"
    ));
}
