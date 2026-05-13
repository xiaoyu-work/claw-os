//! Bridge to `apps/fs` — listing, reading, writing, deleting,
//! statting, and searching files. Mirrors
//! `apps/fs/main.py::run("ls"|"read"|"write"|"rm"|"mkdir"|"stat"|"search"|"recent")`.
//!
//! All side effects pass through `caps::require("fs.read" | "fs.write" | …)`
//! on the kernel side and are written to `caps.jsonl`. GUI apps that
//! still need the speed of a raw `std::fs::metadata` should add their
//! call site to the in-process fast path that lands in a follow-up
//! commit — until then, every desktop fs op flows through here.

use serde::{Deserialize, Serialize};

use super::{call_typed, BridgeError};

/// One row in a directory listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
}

/// Response from `apps/fs ls`.
#[derive(Debug, Clone, Deserialize)]
pub struct ListResult {
    pub path: String,
    pub files: Vec<DirEntry>,
}

/// Response from `apps/fs read`.
#[derive(Debug, Clone, Deserialize)]
pub struct ReadResult {
    pub path: String,
    pub content: String,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub total_size: Option<u64>,
    #[serde(default)]
    pub offset: Option<u64>,
}

/// Response from `apps/fs write`.
#[derive(Debug, Clone, Deserialize)]
pub struct WriteResult {
    pub path: String,
    pub bytes: u64,
}

/// Response from `apps/fs stat`.
#[derive(Debug, Clone, Deserialize)]
pub struct StatResult {
    pub path: String,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub is_dir: bool,
    #[serde(default)]
    pub is_file: bool,
    #[serde(default)]
    pub modified: Option<f64>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

/// List the entries under `path`.
pub fn ls(path: impl AsRef<str>) -> Result<ListResult, BridgeError> {
    call_typed("fs", "ls", [path.as_ref()], None)
}

/// Read a UTF-8 text file. Binary files are returned as a best-effort
/// `errors=replace` decode — callers that need raw bytes should add a
/// dedicated `fs.read_bytes` verb to `apps/fs` (not yet exposed).
pub fn read(path: impl AsRef<str>) -> Result<ReadResult, BridgeError> {
    call_typed("fs", "read", [path.as_ref()], None)
}

/// Write `content` to `path`, creating any missing parents.
pub fn write(path: impl AsRef<str>, content: &str) -> Result<WriteResult, BridgeError> {
    // The Python app reads stdin when `--content` is not given. We
    // always go through stdin to avoid argv quoting headaches and
    // long-content limits.
    call_typed("fs", "write", [path.as_ref()], Some(content.as_bytes()))
}

/// Remove a file or empty directory at `path`.
pub fn rm(path: impl AsRef<str>) -> Result<serde_json::Value, BridgeError> {
    super::call("fs", "rm", [path.as_ref()], None)
}

/// Create `path` (and any missing parents).
pub fn mkdir(path: impl AsRef<str>) -> Result<serde_json::Value, BridgeError> {
    super::call("fs", "mkdir", [path.as_ref()], None)
}

/// Stat a single path.
pub fn stat(path: impl AsRef<str>) -> Result<StatResult, BridgeError> {
    call_typed("fs", "stat", [path.as_ref()], None)
}
