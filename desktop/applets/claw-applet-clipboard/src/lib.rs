// SPDX-License-Identifier: GPL-3.0-only

mod app;
mod copyq;
mod localize;

pub fn run() -> cosmic::iced::Result {
    localize::localize();
    cosmic::applet::run::<app::ClipboardApplet>(())
}
