// SPDX-License-Identifier: GPL-3.0-only

//! Approval-gate applet — a tiny panel button that surfaces the live
//! approval queue from clawd and lets the user approve or deny each
//! request without leaving the desktop.
//!
//! Storage protocol is owned by `core/src/approvals.rs` (a Rust
//! module in the `cos` kernel). The design ethos is dark-terminal +
//! brand-blue accent (see `desktop/agent/docs/design-system.md`).

mod app;
mod localize;
mod queue;

use crate::localize::localize;

pub fn run() -> cosmic::iced::Result {
    localize();
    app::run()
}
