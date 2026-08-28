// SPDX-License-Identifier: GPL-3.0-only

pub fn ask_claw(query: &str) -> Result<(), cos_runtime::ask_claw::LaunchError> {
    cos_runtime::ask_claw::launch_query(query)
}
