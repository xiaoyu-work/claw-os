//! Desktop GUI bootstrap for Claw OS apps (Rust edition).
//!
//! This is the Rust counterpart to [`crate::ai`] (gated model access)
//! and Python's `claw_os_sdk.gui`. It deliberately does **not** wrap a
//! UI toolkit: a Claw OS desktop app draws its own window in whatever
//! toolkit it likes ("World A"). This module only surfaces the small
//! amount of kernel context an app receives once it has been
//! kernel-spawned as a GUI, plus the one privileged action a GUI
//! commonly wants — summoning the system agent overlay.
//!
//! # How a GUI app is launched
//!
//! When an app declares a `desktop` block in `app.json`, `cos app
//! install` writes a launcher whose `Exec` is `cos app <id> --gui`.
//! Activating it routes the launch back through the kernel, which spawns
//! the app with `COS_APP_GUI=1` and `COS_APP_ID` set, so identity,
//! audit, and consent apply to the GUI exactly as to the headless path.
//!
//! # Usage
//!
//! ```no_run
//! use claw_os_sdk::gui;
//!
//! fn main() {
//!     if gui::is_gui_launch() {
//!         let ctx = gui::Context::from_env().expect("authenticated GUI context");
//!         // draw your own window using ctx.app_id / ctx.files,
//!         // call crate::ai / crate::tools for kernel-mediated work,
//!         // and ctx.open_agent_overlay(None) to summon "Ask Claw".
//!     }
//! }
//! ```

#[cfg(target_os = "linux")]
use std::io::{BufRead, BufReader, Read, Write};
#[cfg(target_os = "linux")]
use std::os::linux::net::SocketAddrExt;
#[cfg(target_os = "linux")]
use std::os::unix::net::{SocketAddr, UnixStream};
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};
#[cfg(target_os = "linux")]
use std::sync::mpsc;
#[cfg(target_os = "linux")]
use std::time::Duration;

#[cfg(target_os = "linux")]
const ASK_CLAW_LAUNCHER: &str = "/usr/local/bin/cos-ask-claw-launcher";
#[cfg(target_os = "linux")]
const ASK_CLAW_PROTOCOL: u32 = 1;
#[cfg(target_os = "linux")]
const ASK_CLAW_REQUEST_LIMIT: usize = 32 * 1024;
#[cfg(target_os = "linux")]
const ASK_CLAW_TIMEOUT: Duration = Duration::from_secs(5);

/// Returns `true` when the current process was spawned as a desktop GUI.
///
/// The bridge sets `COS_APP_GUI=1` for the authenticated desktop launch.
pub fn is_gui_launch() -> bool {
    std::env::var("COS_APP_GUI").as_deref() == Ok("1")
}

/// The kernel context handed to a desktop app at launch.
#[derive(Debug, Clone)]
pub struct Context {
    /// The kernel-assigned app identity (`COS_APP_ID`).
    pub app_id: String,
    /// File paths the launcher passed (`%F`), decoded from
    /// `COS_ARGS_JSON`. Empty when launched without file arguments.
    pub files: Vec<String>,
}

impl Context {
    /// Build the context from the environment the kernel set up.
    pub fn from_env() -> Result<Self, String> {
        let app_id = std::env::var("COS_APP_ID")
            .map_err(|_| "COS_APP_ID is required for a GUI launch".to_string())?;
        if app_id.is_empty() {
            return Err("COS_APP_ID is required for a GUI launch".to_string());
        }
        let files = match std::env::var("COS_ARGS_JSON") {
            Ok(raw) => serde_json::from_str::<Vec<String>>(&raw)
                .map_err(|error| format!("COS_ARGS_JSON must be an array of strings: {error}"))?,
            Err(std::env::VarError::NotPresent) => Vec::new(),
            Err(error) => return Err(format!("read COS_ARGS_JSON: {error}")),
        };
        Ok(Self { app_id, files })
    }

    /// Summon the system "Ask Claw" agent overlay.
    ///
    /// This uses the fixed packaged Ask Claw launcher and its versioned
    /// authenticated Unix-socket protocol. Pass `hint` to ground the agent's first response in
    /// the app's current state (e.g. the open document) without
    /// polluting the visible chat transcript. The overlay is detached —
    /// it outlives this call and is not tied to the app's stdio.
    ///
    /// Returns the spawn error if the overlay binary is missing (e.g. a
    /// headless box with no desktop shell).
    pub fn open_agent_overlay(&self, hint: Option<&str>) -> std::io::Result<()> {
        #[cfg(not(target_os = "linux"))]
        {
            let _ = hint;
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "Ask Claw private activation requires Linux",
            ));
        }
        #[cfg(target_os = "linux")]
        {
            validate_launcher()?;
            let mut child = Command::new(ASK_CLAW_LAUNCHER)
                .args(["--protocol", &ASK_CLAW_PROTOCOL.to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()?;
            let Some(stdout) = child.stdout.take() else {
                return Err(stop_launcher(
                    &mut child,
                    std::io::Error::other("Ask Claw launcher readiness channel unavailable"),
                ));
            };
            let (sender, receiver) = mpsc::sync_channel(1);
            std::thread::spawn(move || {
                let mut ready = String::new();
                let result = BufReader::new(stdout)
                    .take(257)
                    .read_line(&mut ready)
                    .and_then(|_| {
                        if ready.len() > 256 || !ready.ends_with('\n') {
                            Err(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "invalid Ask Claw socket announcement",
                            ))
                        } else {
                            Ok(ready)
                        }
                    });
                let _ = sender.send(result);
            });
            let announcement = match receiver.recv_timeout(ASK_CLAW_TIMEOUT) {
                Ok(Ok(announcement)) => announcement,
                Ok(Err(error)) => return Err(stop_launcher(&mut child, error)),
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "Ask Claw launcher readiness timed out",
                    ));
                }
            };
            let endpoint = announcement
                .strip_prefix(&format!("SOCKET {ASK_CLAW_PROTOCOL} @"))
                .and_then(|value| value.strip_suffix('\n'))
                .filter(|value| !value.is_empty())
                .ok_or_else(|| std::io::Error::other("invalid Ask Claw socket announcement"));
            let endpoint = match endpoint {
                Ok(endpoint) => endpoint,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
            };
            let address = match SocketAddr::from_abstract_name(endpoint.as_bytes()) {
                Ok(address) => address,
                Err(error) => return Err(stop_launcher(&mut child, error)),
            };
            let mut socket = match UnixStream::connect_addr(&address) {
                Ok(socket) => socket,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
            };
            if let Err(error) = socket.set_read_timeout(Some(ASK_CLAW_TIMEOUT)) {
                return Err(stop_launcher(&mut child, error));
            }
            if let Err(error) = socket.set_write_timeout(Some(ASK_CLAW_TIMEOUT)) {
                return Err(stop_launcher(&mut child, error));
            }
            let mut ready = [0_u8; 8];
            if let Err(error) = socket.read_exact(&mut ready) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
            if ready != *b"READY 1\n" {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::other("invalid Ask Claw socket handshake"));
            }
            let request = serde_json::json!({
                "protocol": ASK_CLAW_PROTOCOL,
                "app": self.app_id,
                "hint": hint,
            });
            let payload = match serde_json::to_vec(&request) {
                Ok(payload) => payload,
                Err(error) => {
                    return Err(stop_launcher(
                        &mut child,
                        std::io::Error::new(std::io::ErrorKind::InvalidData, error),
                    ));
                }
            };
            if payload.len() > ASK_CLAW_REQUEST_LIMIT {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "Ask Claw request exceeds the protocol limit",
                ));
            }
            if let Err(error) = socket
                .write_all(&(payload.len() as u32).to_be_bytes())
                .and_then(|_| socket.write_all(&payload))
                .and_then(|_| socket.flush())
                .and_then(|_| socket.shutdown(std::net::Shutdown::Write))
            {
                return Err(stop_launcher(&mut child, error));
            }
            let mut accepted = [0_u8; 11];
            if let Err(error) = socket.read_exact(&mut accepted) {
                return Err(stop_launcher(&mut child, error));
            }
            if accepted != *b"ACCEPTED 1\n" {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::other(
                    "invalid Ask Claw acceptance response",
                ));
            }
            drop(socket);
            let child = std::sync::Arc::new(std::sync::Mutex::new(Some(child)));
            let reaper_child = std::sync::Arc::clone(&child);
            if let Err(error) = std::thread::Builder::new()
                .name("ask-claw-sdk-reaper".into())
                .spawn(move || {
                    if let Some(mut child) = reaper_child
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .take()
                    {
                        let _ = child.wait();
                    }
                })
            {
                if let Some(mut child) = child
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take()
                {
                    return Err(stop_launcher(&mut child, error));
                }
                return Err(error);
            }
            Ok(())
        }
    }
}

#[cfg(target_os = "linux")]
fn stop_launcher(child: &mut std::process::Child, error: std::io::Error) -> std::io::Error {
    let _ = child.kill();
    let _ = child.wait();
    error
}

#[cfg(target_os = "linux")]
fn validate_launcher() -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    for path in ["/usr", "/usr/local", "/usr/local/bin"] {
        let metadata = std::fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.uid() != 0
            || metadata.mode() & 0o022 != 0
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "untrusted Ask Claw launcher parent",
            ));
        }
    }
    let metadata = std::fs::symlink_metadata(ASK_CLAW_LAUNCHER)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o111 == 0
        || metadata.mode() & 0o022 != 0
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "untrusted Ask Claw launcher",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/gui.rs"));
}
