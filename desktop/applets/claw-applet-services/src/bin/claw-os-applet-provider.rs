// SPDX-License-Identifier: GPL-3.0-only

use std::{io, os::fd::AsFd, process::ExitCode};
use tokio::{
    io::BufReader,
    net::unix::pipe::{Receiver, Sender},
};

async fn serve() -> io::Result<()> {
    // Tokio's generic stdin/stdout use uncancellable blocking workers.
    // Private nonblocking pipes let frame deadlines also bound process exit.
    let input = Receiver::from_owned_fd(io::stdin().as_fd().try_clone_to_owned()?)?;
    let output = Sender::from_owned_fd(io::stdout().as_fd().try_clone_to_owned()?)?;
    claw_applet_services::service::serve(BufReader::new(input), output).await
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    if !std::env::args()
        .skip(1)
        .eq([claw_os_sdk::applet::protocol::PROVIDER_ARGUMENT])
    {
        eprintln!("usage: claw-os-applet-provider --stdio-v1");
        return ExitCode::from(2);
    }
    match serve().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
