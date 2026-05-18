//! Legacy session-tier reader.
//!
//! All of the old `cos policy` CLI (elevate / drop / status / check),
//! the [`OpType`]-based [`require`] gate, scope helpers, and the
//! elevation-grant machinery were retired alongside the kernel-call-site
//! migration to [`crate::caps`]. The only surviving function is
//! [`current_tier`], which is still consulted by
//! [`crate::credential`] for the per-credential `min_tier` filter — a
//! separate concern that will be reworked when the credential store moves
//! onto [`crate::caps::Cap`] grants.
//!
//! Once `credential.rs` no longer reads `current_tier`, this module can
//! be deleted outright.

use serde::Deserialize;
use std::fs;
use std::path::PathBuf;

#[derive(Deserialize, Default)]
struct SessionInfo {
    session_id: String,
    #[serde(default)]
    tier: Option<u8>,
}

#[derive(Deserialize, Default)]
struct Registry {
    sessions: Vec<SessionInfo>,
}

fn proc_registry_path() -> PathBuf {
    crate::paths::data_dir()
        .join("proc")
        .join("registry.json")
}

fn load_proc_registry(path: &PathBuf) -> Registry {
    fs::read_to_string(path)
        .ok()
        .and_then(|data| serde_json::from_str(&data).ok())
        .unwrap_or_default()
}

/// Returns the current `COS_SESSION`'s legacy tier, or `None` if no
/// session context is set or the session has no tier assigned.
pub fn current_tier() -> Option<u8> {
    let sid = std::env::var("COS_SESSION").ok()?;
    let reg = load_proc_registry(&proc_registry_path());
    reg.sessions
        .iter()
        .find(|s| s.session_id == sid)
        .and_then(|s| s.tier)
}
