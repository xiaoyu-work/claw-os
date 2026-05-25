// SPDX-License-Identifier: GPL-3.0-only
//
// Thin sync adapter over `claw-os-sdk` for cosmic-edit.
//
// Each function turns a `BridgeError` into a plain `io::Error` so call
// sites can keep their `match … { Ok / Err(io::Error) }` shape. A
// kernel "denied" decision surfaces as `io::ErrorKind::PermissionDenied`
// so the existing pkexec fallback in `tab.rs::save` keeps working
// unchanged.
//
// AI helpers (`summarize` / `explain` / `rewrite`) live in the [`ai`]
// submodule. They are `async` and return `Result<_, String>` because
// the MCP server and any future UI surface want a flat
// human-presentable error, not an `io::Error`.

pub mod ai;

use std::io;
use std::path::Path;

use cos_runtime::{exec, fs, BridgeError};

fn map_err(err: BridgeError) -> io::Error {
    let kind = if err.is_denied() {
        io::ErrorKind::PermissionDenied
    } else {
        io::ErrorKind::Other
    };
    io::Error::new(kind, err.to_string())
}

fn path_str(path: &Path) -> io::Result<&str> {
    path.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "path is not valid UTF-8 (claw-os-sdk requires UTF-8 paths)",
        )
    })
}

/// User-intent file read (reload tab from disk after an external change).
///
/// Hot-path reads (syntax highlighting, project tree population, etc.)
/// MUST keep using `std::fs` directly — those happen on every keystroke
/// and routing each one through a subprocess would tank the editor.
pub fn read_to_string(path: &Path) -> io::Result<String> {
    let p = path_str(path)?;
    fs::read(p).map(|r| r.content).map_err(map_err)
}

/// User-intent file save.
///
/// Returns `io::ErrorKind::PermissionDenied` on a kernel denial — the
/// caller may then prompt the user for elevation (pkexec) exactly as
/// it did before claw-os-sdk existed.
pub fn write_text(path: &Path, contents: &str) -> io::Result<()> {
    let p = path_str(path)?;
    fs::write(p, contents).map(|_| ()).map_err(map_err)
}

/// Spawn a detached process (e.g. `cosmic-edit` re-launching itself for
/// "New Window"). The bridge daemonises the child via `cos app exec`.
pub fn start_detached(program: &Path, args: &[&str]) -> io::Result<()> {
    let prog = path_str(program)?;
    let mut argv: Vec<&str> = Vec::with_capacity(args.len() + 1);
    argv.push(prog);
    argv.extend_from_slice(args);
    exec::start(&argv).map(|_| ()).map_err(map_err)
}
