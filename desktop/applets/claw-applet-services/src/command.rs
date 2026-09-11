// SPDX-License-Identifier: GPL-3.0-only

use std::{io, process::Output, process::Stdio, time::Duration};

use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    time::timeout,
};

pub(crate) const OUTPUT_BYTES: usize = 1024 * 1024;
const STDERR_BYTES: usize = 16 * 1024;

pub(crate) fn cos_binary() -> String {
    #[cfg(any(test, not(feature = "provider")))]
    if let Ok(binary) = std::env::var("COS_BIN") {
        return binary;
    }
    "/usr/local/bin/cos".to_string()
}

async fn bounded(reader: impl AsyncRead + Unpin, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "OS service output exceeded its limit.",
        ));
    }
    Ok(bytes)
}

pub(crate) async fn output(
    command: &mut Command,
    limit: usize,
    deadline: Duration,
) -> io::Result<Output> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("OS service stdout unavailable."))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("OS service stderr unavailable."))?;
    let result = timeout(deadline, async {
        let (stdout, stderr) =
            tokio::try_join!(bounded(stdout, limit), bounded(stderr, STDERR_BYTES))?;
        let status = child.wait().await?;
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    })
    .await
    .unwrap_or_else(|_| {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "OS service timed out.",
        ))
    });
    if let Err(error) = result {
        child.start_kill().map_err(|cleanup| {
            io::Error::other(format!("{error}; could not stop OS service: {cleanup}"))
        })?;
        timeout(Duration::from_secs(1), child.wait())
            .await
            .map_err(|_| {
                io::Error::other(format!("{error}; OS service termination was not confirmed"))
            })?
            .map_err(|cleanup| {
                io::Error::other(format!("{error}; could not reap OS service: {cleanup}"))
            })?;
        return Err(error);
    }
    result
}
