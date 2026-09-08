// SPDX-License-Identifier: GPL-3.0-only

mod app;
pub use claw_applet_services::{calendar, policy};
mod localize;
mod system;

use localize::localize;

pub fn run() -> cosmic::iced::Result {
    localize();
    cosmic::applet::run::<app::WidgetRail>(())
}
