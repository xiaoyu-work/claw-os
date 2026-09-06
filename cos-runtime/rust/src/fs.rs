//! Bridge to `apps/fs` — listing, reading, writing, deleting,
//! statting, and searching files through its manifest-bound MCP tools.
//! The human CLI bridge resolves each command from `app.json.mcp.tools`.
//! This is not an App-to-App interface: App MCP handlers and App-owned agents
//! cannot use it to invoke `fs`; they need a controlled system primitive.
//!
//! All side effects pass through `caps::require("fs.read" | "fs.write" | …)`
//! on the kernel side and are written to `caps.jsonl`. GUI apps that
//! still need the speed of a raw `std::fs::metadata` should add their
//! call site to the in-process fast path that lands in a follow-up
//! commit.

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
/// `errors=replace` decode — callers that need raw bytes should use [`read_bytes`].
pub fn read(path: impl AsRef<str>) -> Result<ReadResult, BridgeError> {
    call_typed("fs", "read", [path.as_ref()], None)
}

/// Write `content` to `path`, creating any missing parents. The serialized JSON
/// arguments must fit the CLI's 1008 KiB budget (including escapes/base64).
pub fn write(path: impl AsRef<str>, content: &str) -> Result<WriteResult, BridgeError> {
    write_content("write", path.as_ref(), content)
}

/// Write raw bytes to `path`, creating any missing parents.
///
/// Binary-safe counterpart to [`write`]. The bridge base64-encodes the
/// payload before handing it to `apps/fs write_bytes`, which decodes
/// it on the other side. Use this for clipboard image paste, drag-drop
/// of binary content, archive extraction, and anywhere else where
/// callers have `&[u8]` rather than `&str`.
pub fn write_bytes(path: impl AsRef<str>, content: &[u8]) -> Result<WriteResult, BridgeError> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let encoded = STANDARD.encode(content);
    write_content("write_bytes", path.as_ref(), &encoded)
}

fn write_content(verb: &str, path: &str, content: &str) -> Result<WriteResult, BridgeError> {
    let arguments = serde_json::to_vec(&serde_json::json!({"path": path, "content": content}))
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    call_typed("fs", verb, ["--args-stdin"], Some(&arguments))
}

/// Rename / move a path. Composes `fs.delete` on src + `fs.write` on dst.
pub fn rename(
    src: impl AsRef<str>,
    dst: impl AsRef<str>,
) -> Result<serde_json::Value, BridgeError> {
    super::call("fs", "rename", [src.as_ref(), dst.as_ref()], None)
}

/// Alias for [`rename`] — same semantics, different name for callers
/// whose UX speaks of "move".
pub fn r#move(
    src: impl AsRef<str>,
    dst: impl AsRef<str>,
) -> Result<serde_json::Value, BridgeError> {
    super::call("fs", "move", [src.as_ref(), dst.as_ref()], None)
}

/// Copy a file or directory tree. Composes `fs.read` on src + `fs.write` on dst.
pub fn copy(src: impl AsRef<str>, dst: impl AsRef<str>) -> Result<serde_json::Value, BridgeError> {
    super::call("fs", "copy", [src.as_ref(), dst.as_ref()], None)
}

/// Read a complete file as raw bytes (binary-safe).
///
/// On the wire the file content travels base64-encoded; this wrapper
/// decodes it for the caller and returns the cleartext bytes. A truncated App
/// response (currently files above 4 MiB) is an error, never a partial file.
pub fn read_bytes(path: impl AsRef<str>) -> Result<Vec<u8>, BridgeError> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    #[derive(Deserialize)]
    struct ReadBytesEnvelope {
        base64: String,
        #[serde(default)]
        truncated: bool,
    }
    let env: ReadBytesEnvelope = call_typed("fs", "read_bytes", [path.as_ref()], None)?;
    if env.truncated {
        return Err(BridgeError::Decode {
            app: "fs".into(),
            verb: "read_bytes".into(),
            message: "file exceeds the binary read limit; response was truncated".into(),
        });
    }
    STANDARD
        .decode(env.base64)
        .map_err(|e| BridgeError::Decode {
            app: "fs".into(),
            verb: "read_bytes".into(),
            message: format!("bridge: invalid base64 in read_bytes response: {e}"),
        })
}

/// Remove a file or directory tree at `path`.
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

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/fs.rs"));
}
