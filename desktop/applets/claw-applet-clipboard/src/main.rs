// SPDX-License-Identifier: GPL-3.0-only

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt().with_env_filter("warn").init();
    let _ = tracing_log::LogTracer::init();
    claw_applet_clipboard::run()
}
