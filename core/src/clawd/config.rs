use std::path::PathBuf;

use crate::paths;

pub const SOCKET_ENV: &str = "CLAWD_SOCKET";
pub const SOCKET_MODE_ENV: &str = "CLAWD_SOCKET_MODE";
pub const SOCKET_GROUP_ENV: &str = "CLAWD_SOCKET_GROUP";

pub fn socket_path() -> PathBuf {
    if let Ok(path) = std::env::var(SOCKET_ENV) {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }

    paths::clawd_socket_path()
}

pub fn socket_mode() -> u32 {
    std::env::var(SOCKET_MODE_ENV)
        .ok()
        .and_then(|raw| u32::from_str_radix(raw.trim_start_matches("0o"), 8).ok())
        .unwrap_or(0o600)
}

pub fn socket_group() -> Option<String> {
    std::env::var(SOCKET_GROUP_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
