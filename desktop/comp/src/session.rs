// SPDX-License-Identifier: GPL-3.0-only

use anyhow::{Result, anyhow};
use std::collections::HashMap;

use crate::state::Common;

pub fn get_env(common: &Common) -> Result<HashMap<String, String>> {
    let mut env = HashMap::new();
    env.insert(
        String::from("WAYLAND_DISPLAY"),
        common
            .socket
            .clone()
            .into_string()
            .map_err(|_| anyhow!("wayland socket is no valid utf-8 string?"))?,
    );
    if let Some(display) = common.xwayland_state.as_ref().map(|s| s.display) {
        env.insert(String::from("DISPLAY"), format!(":{}", display));
    }
    Ok(env)
}
