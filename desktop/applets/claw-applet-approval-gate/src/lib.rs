// SPDX-License-Identifier: GPL-3.0-only

//! OS review presentation for graphical and headless App subjects alike.
//! The shared `clawd_client::system_review` DTO terminates broker models here.
//! No capability, consent, approval store or privileged provider belongs to
//! this GPL renderer; all decisions pass through the trusted OS helper.

mod app;
mod localize;
mod queue;
pub mod review;
pub mod review_card;

use crate::localize::localize;

pub fn run() -> cosmic::iced::Result {
    localize();
    app::run()
}

#[cfg(test)]
mod test_support {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/support/reviews.rs"
    ));
}
