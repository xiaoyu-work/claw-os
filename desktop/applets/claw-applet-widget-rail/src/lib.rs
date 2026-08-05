// SPDX-License-Identifier: GPL-3.0-only

mod app;
mod calendar;
mod localize;
mod policy;
mod system;

use localize::localize;

pub fn run() -> cosmic::iced::Result {
    localize();
    cosmic::applet::run::<app::WidgetRail>(())
}
