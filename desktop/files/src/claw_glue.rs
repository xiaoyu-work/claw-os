//! Thin adapters that route fs/exec mutations through `claw-os-sdk`
//! while preserving the existing call sites' `Result<_, io::Error>`
//! shape.
//!
//! The previous code talked to the kernel directly:
//!
//! ```ignore
//! std::fs::write(&path, &data)?;
//! Command::new(&exe).spawn()?;
//! ```
//!
//! Those bypassed the capability gate, the structured audit log, and
//! checkpoint snapshots. The functions here funnel the same logical
//! operations through `cos app fs|exec <verb>`, picking up uniform
//! enforcement on the way. Call sites stay one line:
//!
//! ```ignore
//! claw_glue::write_bytes(&path, &data)?;
//! claw_glue::start_detached(&exe, &[path_arg])?;
//! ```
//!
//! When the kernel denies, we turn the `BridgeError::AppError {code:
//! "denied", ..}` into `io::Error::PermissionDenied` so existing
//! error-display paths surface a coherent message ("Permission
//! denied") rather than a generic IO failure.
//!
//! AI surfaces (summarise / explain / rewrite / search / …) live in
//! the [`ai`] submodule; they route through the same `cos app`
//! boundary but the public API is async + `Result<_, String>` shaped
//! for the dialog/sidebar layers that consume them.

pub mod ai;

use std::io;
use std::path::Path;

use cos_runtime::{ask_claw, BridgeError};
use serde::Serialize;

/// Convert a `BridgeError` to an `io::Error`, preserving the
/// "denied" signal as `ErrorKind::PermissionDenied`. Anything else
/// becomes `ErrorKind::Other`.
fn to_io(err: BridgeError) -> io::Error {
    if err.is_denied() {
        return io::Error::new(io::ErrorKind::PermissionDenied, err.to_string());
    }
    io::Error::other(err.to_string())
}

fn path_str(p: &Path) -> io::Result<&str> {
    p.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("non-UTF-8 path cannot cross the bridge: {p:?}"),
        )
    })
}

/// Write raw bytes to `path` via `apps/fs write_bytes`. Mirrors
/// `std::fs::write(path, &[u8])`.
pub fn write_bytes(path: &Path, content: &[u8]) -> io::Result<()> {
    let s = path_str(path)?;
    cos_runtime::fs::write_bytes(s, content).map(|_| ()).map_err(to_io)
}

/// Write UTF-8 text to `path` via `apps/fs write`. Mirrors
/// `std::fs::write(path, &str)`.
pub fn write_text(path: &Path, content: &str) -> io::Result<()> {
    let s = path_str(path)?;
    cos_runtime::fs::write(s, content).map(|_| ()).map_err(to_io)
}

/// Create `path` (and any missing parents). Mirrors
/// `std::fs::create_dir_all`.
pub fn mkdir_all(path: &Path) -> io::Result<()> {
    let s = path_str(path)?;
    cos_runtime::fs::mkdir(s).map(|_| ()).map_err(to_io)
}

/// Rename / move `src` to `dst`. Mirrors `std::fs::rename`.
pub fn rename(src: &Path, dst: &Path) -> io::Result<()> {
    let s = path_str(src)?;
    let d = path_str(dst)?;
    cos_runtime::fs::rename(s, d).map(|_| ()).map_err(to_io)
}

/// Copy `src` to `dst`. Handles both files (single copy) and
/// directories (recursive). Mirrors a high-level "copy" operation
/// the user just triggered.
pub fn copy(src: &Path, dst: &Path) -> io::Result<()> {
    let s = path_str(src)?;
    let d = path_str(dst)?;
    cos_runtime::fs::copy(s, d).map(|_| ()).map_err(to_io)
}

/// Remove `path`. Handles files and directories. Mirrors a final
/// "delete" the user just confirmed (Trash → empty, or
/// shift-delete).
pub fn remove(path: &Path) -> io::Result<()> {
    let s = path_str(path)?;
    cos_runtime::fs::rm(s).map(|_| ()).map_err(to_io)
}

/// Spawn a detached process via `apps/exec start`. The previous
/// pattern was `Command::new(program).args(args).spawn()` followed
/// by ignoring the child handle — same shape here, but the kernel
/// gets to gate the launch and the audit log gets a row.
pub fn start_detached<S: AsRef<str>>(program: &str, args: &[S]) -> io::Result<()> {
    let mut argv: Vec<&str> = Vec::with_capacity(args.len() + 1);
    argv.push(program);
    for a in args {
        argv.push(a.as_ref());
    }
    cos_runtime::exec::start(&argv).map(|_| ()).map_err(to_io)
}

/// Same as [`start_detached`] but with the program in a `Path` (the
/// common case where we just resolved an exe via `which`).
pub fn start_detached_path<S: AsRef<str>>(program: &Path, args: &[S]) -> io::Result<()> {
    let prog = path_str(program)?;
    start_detached(prog, args)
}

#[derive(Serialize)]
struct FilesContext<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    selection: Option<&'a str>,
}

impl ask_claw::Context for FilesContext<'_> {
    const APP_ID: &'static str = "cosmic-files";
}

/// Open Ask Claw with the current directory and optional selected path.
pub fn ask_claw(cwd: Option<&str>, selection: Option<&str>) -> Result<(), ask_claw::LaunchError> {
    ask_claw::launch(&FilesContext { cwd, selection })
}
