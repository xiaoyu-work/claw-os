// SPDX-License-Identifier: GPL-3.0-only

mod app;
pub mod calendar;
mod localize;
pub mod policy;
mod system;

use localize::localize;

pub fn run() -> cosmic::iced::Result {
    localize();
    cosmic::applet::run::<app::WidgetRail>(())
}
