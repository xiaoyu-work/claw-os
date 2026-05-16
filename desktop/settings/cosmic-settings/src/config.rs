// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use cosmic::cosmic_config::{self, ConfigGet, ConfigSet};

const NAME: &str = "com.clawos.Settings";

const ACTIVE_PAGE: &str = "active_page";

#[must_use]
#[derive(Debug, Clone)]
pub struct Config {
    state: cosmic_config::Config,
}

impl Default for Config {
    fn default() -> Self {
        Self::new()
    }
}

impl Config {
    pub fn new() -> Self {
        let state = match cosmic_config::Config::new_state(NAME, 1) {
            Ok(state) => state,
            Err(why) => {
                panic!("failed to get {NAME} state: {:?}", why);
            }
        };

        Self { state }
    }

    pub fn active_page(&self) -> Box<str> {
        self.state
            .get::<Box<str>>(ACTIVE_PAGE)
            .unwrap_or_else(|_| Box::from("desktop"))
    }

    pub fn set_active_page(&self, page: Box<str>) {
        if let Err(why) = self.state.set::<Box<str>>(ACTIVE_PAGE, page.clone()) {
            tracing::error!(?why, "failed to store active page ID");
        }
    }
}
