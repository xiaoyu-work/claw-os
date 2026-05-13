// Copyright 2024 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use cosmic::Apply;

pub async fn set_username(connection_name: &str, username: &str) -> Result<(), String> {
    let connection_name = connection_name.to_owned();
    let username = username.to_owned();
    tokio::task::spawn_blocking(move || {
        crate::claw_glue::run_output(
            &[
                "nmcli",
                "con",
                "mod",
                &connection_name,
                "vpn.user-name",
                &username,
            ],
            Some(5),
        )
    })
    .await
    .unwrap_or_else(|e| Err(std::io::Error::other(e.to_string())))
    .apply(crate::utils::map_stderr_output)
}

pub async fn add_fallback(connection_name: &str) -> Result<(), String> {
    let connection_name = connection_name.to_owned();
    tokio::task::spawn_blocking(move || {
        crate::claw_glue::run_output(
            &[
                "nmcli",
                "con",
                "mod",
                &connection_name,
                "+vpn.data",
                "data-ciphers=AES-256-GCM:AES-128-GCM:CHACHA20-POLY1305:AES-256-CBC:AES-128-CBC",
            ],
            Some(5),
        )
    })
    .await
    .unwrap_or_else(|e| Err(std::io::Error::other(e.to_string())))
    .apply(crate::utils::map_stderr_output)
}

pub async fn connect(connection_name: &str) -> Result<(), String> {
    let connection_name = connection_name.to_owned();
    tokio::task::spawn_blocking(move || {
        crate::claw_glue::run_output(&["nmcli", "con", "up", &connection_name], Some(5))
    })
    .await
    .unwrap_or_else(|e| Err(std::io::Error::other(e.to_string())))
    .apply(crate::utils::map_stderr_output)
}
