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
//!         let ctx = gui::Context::from_env();
//!         // draw your own window using ctx.app_id / ctx.files,
//!         // call crate::ai / crate::tools for kernel-mediated work,
//!         // and ctx.open_agent_overlay(None) to summon "Ask Claw".
//!     }
//! }
//! ```

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const ASK_CLAW_LAUNCHER: &str = "/usr/local/bin/cos-ask-claw-launcher";

/// Command value the bridge passes (and the default `desktop.exec`) when
/// an app is launched as a GUI.
pub const GUI_COMMAND: &str = "--gui";

/// Returns `true` when the current process was spawned as a desktop GUI.
///
/// Detection prefers the `COS_APP_GUI` environment variable the bridge
/// sets for the long-lived GUI process. The `command` fallback (for apps
/// with a custom `desktop.exec`) is available via
/// [`is_gui_launch_for`].
pub fn is_gui_launch() -> bool {
    std::env::var("COS_APP_GUI").as_deref() == Ok("1")
}

/// Like [`is_gui_launch`], but also treats a `command` equal to
/// [`GUI_COMMAND`] as a GUI launch (for callers that route their own
/// argv and may use a custom `desktop.exec`).
pub fn is_gui_launch_for(command: &str) -> bool {
    is_gui_launch() || command == GUI_COMMAND
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
    pub fn from_env() -> Self {
        let app_id = std::env::var("COS_APP_ID").unwrap_or_else(|_| "unknown".to_string());
        let files = std::env::var("COS_ARGS_JSON")
            .ok()
            .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
            .unwrap_or_default();
        Self { app_id, files }
    }

    /// Summon the system "Ask Claw" agent overlay.
    ///
    /// This uses the fixed packaged Ask Claw launcher and its versioned
    /// READY/stdin protocol. Pass `hint` to ground the agent's first response in
    /// the app's current state (e.g. the open document) without
    /// polluting the visible chat transcript. The overlay is detached —
    /// it outlives this call and is not tied to the app's stdio.
    ///
    /// Returns the spawn error if the overlay binary is missing (e.g. a
    /// headless box with no desktop shell).
    pub fn open_agent_overlay(&self, hint: Option<&str>) -> std::io::Result<()> {
        validate_launcher()?;
        let mut child = Command::new(ASK_CLAW_LAUNCHER)
            .args(["--protocol", "1"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdout = child.stdout.take().ok_or_else(|| {
            std::io::Error::other("Ask Claw launcher readiness channel unavailable")
        })?;
        let (sender, receiver) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut ready = String::new();
            let result = BufReader::new(stdout).read_line(&mut ready).map(|_| ready);
            let _ = sender.send(result);
        });
        let ready = match receiver.recv_timeout(Duration::from_secs(5)) {
            Ok(result) => result?,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "Ask Claw launcher readiness timed out",
                ));
            }
        };
        if ready != "READY 1\n" {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::other(
                "Ask Claw launcher did not become ready",
            ));
        }
        let request = serde_json::json!({
            "protocol": 1,
            "app": self.app_id,
            "hint": hint,
        });
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("Ask Claw launcher stdin unavailable"))?;
        serde_json::to_writer(&mut stdin, &request)?;
        stdin.flush()?;
        drop(stdin);
        std::thread::Builder::new()
            .name("ask-claw-sdk-reaper".into())
            .spawn(move || {
                let _ = child.wait();
            })?;
        Ok(())
    }
}

fn validate_launcher() -> std::io::Result<()> {
    #[cfg(unix)]
    {
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
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/gui.rs"));
}
