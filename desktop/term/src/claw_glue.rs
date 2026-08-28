// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use cos_runtime::ask_claw;
use serde::Serialize;

#[derive(Serialize)]
struct TerminalContext<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<&'a str>,
}

impl ask_claw::Context for TerminalContext<'_> {
    const APP_ID: &'static str = "cosmic-term";
}

#[derive(Serialize)]
struct ExplainOutputContext<'a> {
    mode: &'static str,
    output: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<&'a str>,
}

impl ask_claw::Context for ExplainOutputContext<'_> {
    const APP_ID: &'static str = "cosmic-term";
}

pub fn ask_claw(cwd: Option<&str>) -> Result<(), ask_claw::LaunchError> {
    ask_claw::launch(&TerminalContext { cwd }).map(|_| ())
}

pub fn explain_output(output: &str, cwd: Option<&str>) -> Result<(), ask_claw::LaunchError> {
    ask_claw::launch(&ExplainOutputContext {
        mode: "explain_output",
        output,
        cwd,
    })
    .map(|_| ())
}
