// SPDX-License-Identifier: GPL-3.0-only
use claw_display_control::session::Subscription;
use color_eyre::eyre::{Result, WrapErr};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub async fn attach_display(
	token: CancellationToken,
) -> Result<(HashMap<String, String>, JoinHandle<Result<()>>)> {
	let subscription = tokio::task::spawn_blocking(Subscription::connect)
		.await
		.wrap_err("display attachment worker failed")?
		.wrap_err("no authenticated Root display is available for this login")?;
	let environment = HashMap::from([
		(
			"WAYLAND_DISPLAY".to_string(),
			subscription.display().to_string(),
		),
		("XDG_RUNTIME_DIR".to_string(), subscription.runtime_dir()),
		(
			"DBUS_SESSION_BUS_ADDRESS".to_string(),
			subscription.bus_address(),
		),
	]);
	let watcher = tokio::task::spawn_blocking(move || {
		while !token.is_cancelled() {
			if subscription
				.ended(Instant::now() + Duration::from_millis(100))
				.wrap_err("authenticated display control was lost")?
			{
				return Err(color_eyre::eyre::eyre!(
					"authenticated display ended during the desktop session"
				));
			}
		}
		Ok(())
	});
	Ok((environment, watcher))
}
