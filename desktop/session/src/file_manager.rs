// SPDX-License-Identifier: GPL-3.0-only

use freedesktop_desktop_entry::DesktopEntry;
use rustix::process::{Pid, Signal, kill_process};
use std::env;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::{Child, Command};

pub const AUTOSTART_ID: &str = "claw-file-manager";
const HELPER: &str = "/usr/lib/cos/bin/claw-file-manager";

pub struct FileManager(Child);

fn arguments(entry: &DesktopEntry) -> Result<Option<Vec<String>>, &'static str> {
	if entry.desktop_entry("Hidden") == Some("true")
		|| !entry
			.only_show_in()
			.is_some_and(|desktops| desktops.contains(&"COSMIC"))
		|| entry
			.not_show_in()
			.is_some_and(|desktops| desktops.contains(&"COSMIC"))
	{
		return Ok(None);
	}
	if entry.desktop_entry("Type") != Some("Application")
		|| entry.desktop_entry("X-systemd-skip") != Some("true")
		|| entry
			.try_exec()
			.is_some_and(|executable| executable != HELPER)
	{
		return Err("file-manager autostart must be owned by the COSMIC session");
	}
	let argv = entry
		.parse_exec()
		.map_err(|_| "invalid file-manager autostart arguments")?;
	let Some((executable, args)) = argv.split_first() else {
		return Err("missing file-manager autostart executable");
	};
	if executable != HELPER || (!args.is_empty() && (args.len() != 2 || args[0] != "--app")) {
		return Err("file-manager autostart must select only the installed OS helper");
	}
	Ok(Some(args.to_vec()))
}

fn configuration_directories() -> Vec<PathBuf> {
	let mut directories = Vec::new();
	let config_home = env::var_os("XDG_CONFIG_HOME")
		.filter(|path| !path.is_empty())
		.map(PathBuf::from)
		.filter(|path| path.is_absolute())
		.or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
	if let Some(home) = config_home.filter(|path| path.is_absolute()) {
		directories.push(home);
	}
	let config_dirs = env::var_os("XDG_CONFIG_DIRS")
		.filter(|paths| !paths.is_empty())
		.unwrap_or_else(|| "/etc/xdg".into());
	directories.extend(env::split_paths(&config_dirs).filter(|path| path.is_absolute()));
	directories
}

pub fn start(environment: &[(String, String)]) -> Option<FileManager> {
	for directory in configuration_directories() {
		let path = directory
			.join("autostart")
			.join(format!("{AUTOSTART_ID}.desktop"));
		match std::fs::symlink_metadata(&path) {
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
			Ok(_) => {}
			_ => {
				warn!("file-manager autostart resource is unreadable");
				return None;
			}
		}
		if !std::fs::metadata(&path)
			.is_ok_and(|metadata| metadata.is_file() && metadata.len() <= 16 * 1024)
		{
			warn!("file-manager autostart resource is invalid or oversized");
			return None;
		}
		// The first resource also owns Hidden/invalid overrides: never fall back past it.
		let entry = match DesktopEntry::from_path::<&str>(path, None) {
			Ok(entry) => entry,
			Err(_) => {
				warn!("file-manager autostart resource is invalid");
				return None;
			}
		};
		let args = match arguments(&entry) {
			Ok(Some(args)) => args,
			Ok(None) => return None,
			Err(error) => {
				warn!("{error}");
				return None;
			}
		};
		return match Command::new(HELPER)
			.args(args)
			.envs(environment.iter().cloned())
			.stdin(Stdio::null())
			.stdout(Stdio::inherit())
			.stderr(Stdio::inherit())
			.kill_on_drop(true)
			.spawn()
		{
			Ok(child) => Some(FileManager(child)),
			Err(_) => {
				warn!("could not start the installed file-manager intent transport");
				None
			}
		};
	}
	None
}

impl FileManager {
	pub async fn stop(mut self) {
		if let Some(pid) = self.0.id().and_then(|pid| Pid::from_raw(pid as i32)) {
			let _ = kill_process(pid, Signal::TERM);
		}
		match tokio::time::timeout(Duration::from_secs(5), self.0.wait()).await {
			Ok(Ok(_)) => return,
			Ok(Err(_)) => warn!("file-manager intent transport exit was not confirmed"),
			Err(_) => warn!("file-manager intent transport shutdown timed out"),
		}
		let _ = self.0.start_kill();
		if self.0.wait().await.is_err() {
			warn!("file-manager intent transport cleanup was not confirmed");
		}
	}
}

#[cfg(test)]
mod tests {
	include!(concat!(
		env!("CARGO_MANIFEST_DIR"),
		"/test/unit/file_manager.rs"
	));
}
