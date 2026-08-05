// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt().with_env_filter("warn").init();
    let _ = tracing_log::LogTracer::init();

    let mut args = std::env::args();
    let Some(executable) = args.next() else {
        return Ok(());
    };

    let start = executable.rfind('/').map_or(0, |v| v + 1);
    let invoked_as = &executable[start..];
    let requested;
    let cmd = if invoked_as == "cosmic-applets" {
        requested = args.next();
        requested.as_deref().unwrap_or(invoked_as)
    } else {
        invoked_as
    };

    tracing::info!("Starting `{cmd}` with version {VERSION}");

    match cmd {
        "cosmic-app-list" => cosmic_app_list::run(),
        "cosmic-applet-a11y" => cosmic_applet_a11y::run(),
        "cosmic-applet-audio" => cosmic_applet_audio::run(),
        "cosmic-applet-battery" => cosmic_applet_battery::run(),
        "cosmic-applet-bluetooth" => cosmic_applet_bluetooth::run(),
        "cosmic-applet-minimize" => cosmic_applet_minimize::run(),
        "cosmic-applet-network" => cosmic_applet_network::run(),
        "cosmic-applet-notifications" => cosmic_applet_notifications::run(),
        "cosmic-applet-power" => cosmic_applet_power::run(),
        "cosmic-applet-status-area" => cosmic_applet_status_area::run(),
        "cosmic-applet-tiling" => cosmic_applet_tiling::run(),
        "cosmic-applet-time" => cosmic_applet_time::run(),
        "cosmic-applet-workspaces" => cosmic_applet_workspaces::run(),
        "cosmic-applet-input-sources" => cosmic_applet_input_sources::run(),
        "claw-applet-approval-gate" => claw_applet_approval_gate::run(),
        "claw-applet-agent-activity" => claw_applet_agent_activity::run(),
        "claw-applet-calendar" => claw_applet_calendar::run(),
        "claw-applet-clipboard" => claw_applet_clipboard::run(),
        "claw-applet-widget-rail" => claw_applet_widget_rail::run(),
        "cosmic-panel-button" => cosmic_panel_button::run(),
        _ => Ok(()),
    }
}
