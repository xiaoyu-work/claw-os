use std::path::PathBuf;

use crate::paths;

pub const SOCKET_ENV: &str = "CLAWD_SOCKET";

pub fn socket_path() -> PathBuf {
    if let Ok(path) = std::env::var(SOCKET_ENV) {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }

    paths::clawd_socket_path()
}
