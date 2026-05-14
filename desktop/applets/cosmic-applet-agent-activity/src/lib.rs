// SPDX-License-Identifier: GPL-3.0-only

//! Agent-activity applet — a tiny panel button that surfaces the live
//! contents of `cos agent ls` and lets the user stop / undo / resume
//! a task without leaving the desktop.
//!
//! Storage protocol is owned by `core/src/session/` (a Rust module
//! in the `cos` kernel). This applet does **not** read or parse
//! anything outside what the JSON envelope returned by
//! `cos agent ls` and `cos agent show <id>` already gives it.
//! Mutations go through `cos agent stop|undo|resume <id>`. The
//! design ethos is dark-terminal + emerald accent (see
//! `desktop/agent/docs/design-system.md`).

mod app;
mod localize;
mod tasks;

use crate::localize::localize;

pub fn run() -> cosmic::iced::Result {
    localize();
    app::run()
}
