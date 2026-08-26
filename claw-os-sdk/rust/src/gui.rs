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

use std::process::{Command, Stdio};

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
    /// This raises the same `cos-agent-ui --overlay` window the global
    /// hotkey does. Pass `hint` to ground the agent's first response in
    /// the app's current state (e.g. the open document) without
    /// polluting the visible chat transcript. The overlay is detached —
    /// it outlives this call and is not tied to the app's stdio.
    ///
    /// Returns the spawn error if the overlay binary is missing (e.g. a
    /// headless box with no desktop shell).
    pub fn open_agent_overlay(&self, hint: Option<&str>) -> std::io::Result<()> {
        let bin = std::env::var("COS_AGENT_UI_BIN").unwrap_or_else(|_| "cos-agent-ui".to_string());
        let mut cmd = Command::new(bin);
        cmd.arg("--overlay");
        if let Some(h) = hint {
            cmd.arg("--context").arg(h);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/gui.rs"
    ));
}
