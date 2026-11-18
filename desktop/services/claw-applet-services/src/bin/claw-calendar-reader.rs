// SPDX-License-Identifier: GPL-3.0-only

use std::{
    io::{self, Write},
    process::ExitCode,
};

fn main() -> ExitCode {
    let result =
        claw_applet_services::calendar::reader::run(&std::env::args().skip(1).collect::<Vec<_>>())
            .and_then(|value| {
                let mut output = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
                output.push(b'\n');
                if output.len() > 1024 * 1024 {
                    return Err("Calendar response exceeds its one-MiB byte limit".into());
                }
                io::stdout()
                    .write_all(&output)
                    .map_err(|error| error.to_string())
            });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
