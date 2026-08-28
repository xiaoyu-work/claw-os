// SPDX-License-Identifier: GPL-3.0-only
//
// Thin sync adapter over `claw-os-sdk` for cosmic-settings.
//
// User-intent mutations (write configs, rename connection profiles,
// remove desktop entries) and user-intent process spawns
// (`nm-connection-editor`, `gnome-language-selector`, `update-desktop-database`)
// are funnelled through `cos_runtime::{fs, exec}` so the kernel
// capability gate, the structured audit log (`caps.jsonl`), and
// checkpoint snapshots all apply uniformly.
//
// Hot-path read-only enumerations (listing `~/.local/share/applications/`,
// scanning font dirs, polling `/proc` via `sysinfo`, …) intentionally
// stay on `std::fs` / `tokio::process` and are flagged at the call site
// with `FIXME(claw)` — funnelling those through a subprocess would be
// far too expensive. See the per-app migration notes for the convention.
//
// `BridgeError::is_denied()` is surfaced as
// `io::ErrorKind::PermissionDenied` so existing error paths (e.g. the
// pkexec fallback patterns in other apps) keep working unchanged.

use std::io;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{ExitStatus, Output};

use cos_runtime::{BridgeError, ask_claw, exec, fs};
use serde::Serialize;

fn map_err(err: BridgeError) -> io::Error {
    if err.is_denied() {
        io::Error::new(io::ErrorKind::PermissionDenied, err.to_string())
    } else {
        io::Error::other(err.to_string())
    }
}

fn path_str(p: &Path) -> io::Result<&str> {
    p.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("non-UTF-8 path cannot cross the bridge: {p:?}"),
        )
    })
}

/// User-intent text write. Mirrors `std::fs::write(path, &str)`.
pub fn write_text(path: &Path, contents: &str) -> io::Result<()> {
    let s = path_str(path)?;
    fs::write(s, contents).map(|_| ()).map_err(map_err)
}

/// Create `path` and any missing parents. Mirrors `std::fs::create_dir_all`.
pub fn mkdir_all(path: &Path) -> io::Result<()> {
    let s = path_str(path)?;
    fs::mkdir(s).map(|_| ()).map_err(map_err)
}

/// Remove a file or directory. The bridge's `fs::rm` is recursive, so this
/// matches both `std::fs::remove_file` and `std::fs::remove_dir_all` call
/// sites — settings only removes individual `.desktop` entries today.
pub fn remove(path: &Path) -> io::Result<()> {
    let s = path_str(path)?;
    fs::rm(s).map(|_| ()).map_err(map_err)
}

/// Rename / move `src` to `dst`. Mirrors `std::fs::rename`.
pub fn rename(src: &Path, dst: &Path) -> io::Result<()> {
    let s = path_str(src)?;
    let d = path_str(dst)?;
    fs::rename(s, d).map(|_| ()).map_err(map_err)
}

/// Spawn a detached process via `apps/exec start`. Equivalent in
/// intent to `Command::new(argv[0]).args(&argv[1..]).spawn()` —
/// the kernel gates the launch and records an audit row.
pub fn start(argv: &[&str]) -> io::Result<()> {
    exec::start(argv).map(|_| ()).map_err(map_err)
}

#[derive(Serialize)]
struct SettingsPageContext<'a> {
    page: &'a str,
    title: &'a str,
}

impl ask_claw::Context for SettingsPageContext<'_> {
    const APP_ID: &'static str = "cosmic-settings";
}

#[derive(Serialize)]
struct SettingsSearchContext<'a> {
    mode: &'static str,
    query: &'a str,
}

impl ask_claw::Context for SettingsSearchContext<'_> {
    const APP_ID: &'static str = "cosmic-settings";
}

pub fn ask_claw_page(page: &str, title: &str) -> Result<(), ask_claw::LaunchError> {
    ask_claw::launch(&SettingsPageContext { page, title }).map(|_| ())
}

pub fn ask_claw_search(query: &str) -> Result<(), ask_claw::LaunchError> {
    ask_claw::launch(&SettingsSearchContext {
        mode: "search",
        query,
    })
    .map(|_| ())
}

/// Run `argv` synchronously and return stdout on a clean exit.
///
/// On a non-zero exit status this returns `io::Error::other(stderr)`,
/// matching the most common consumer pattern in cosmic-settings
/// (queries like `xdg-mime query default …` and `locale -a`).
pub fn run_capture(argv: &[&str], timeout_secs: Option<u32>) -> io::Result<String> {
    let r = exec::run(argv, timeout_secs).map_err(map_err)?;
    if r.exit_code != 0 {
        let msg = if r.stderr.is_empty() {
            format!("`{}` exited with code {}", argv.join(" "), r.exit_code)
        } else {
            r.stderr
        };
        return Err(io::Error::other(msg));
    }
    Ok(r.stdout)
}

/// Run `argv` synchronously and return a synthesized `std::process::Output`.
///
/// Lets call sites that previously chained `.output().await.apply(map_stderr_output)`
/// keep their existing post-processing helpers (which inspect
/// `output.status` / `output.stderr` directly).
pub fn run_output(argv: &[&str], timeout_secs: Option<u32>) -> io::Result<Output> {
    let r = exec::run(argv, timeout_secs).map_err(map_err)?;
    let raw = (r.exit_code & 0xff) << 8;
    Ok(Output {
        status: ExitStatus::from_raw(raw),
        stdout: r.stdout.into_bytes(),
        stderr: r.stderr.into_bytes(),
    })
}
