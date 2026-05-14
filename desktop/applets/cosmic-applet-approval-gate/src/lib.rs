// SPDX-License-Identifier: GPL-3.0-only

//! Approval-gate applet — a tiny panel button that surfaces the live
//! contents of `$COS_DATA_DIR/approvals/pending/` and lets the user
//! approve or deny each request without leaving the desktop.
//!
//! Storage protocol is owned by `core/src/approvals.rs` (a Rust
//! module in the `cos` kernel). This applet does **not** read or
//! parse anything outside what the JSON envelope returned by
//! `cos perms pending` already gives it. Mutations go through
//! `cos perms approve <id> [--duration ...]` and `cos perms deny
//! <id>`. The design ethos is dark-terminal + emerald accent (see
//! `desktop/agent/web/docs/design-system.md`).

mod app;
mod localize;
mod queue;

use crate::localize::localize;

pub fn run() -> cosmic::iced::Result {
    localize();
    app::run()
}
