//! Per-decision, unprivileged polkit text agent for the suspended TUI.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::Child;

const AGENT_PATH: &str = "/usr/bin/pkttyagent";

pub(super) async fn with_agent<T>(
    operation: impl std::future::Future<Output = Result<T, String>>,
) -> Result<T, String> {
    let mut agent = start_agent(Path::new(AGENT_PATH)).await?;
    let result = tokio::select! {
        result = operation => result,
        signal = tokio::signal::ctrl_c() => {
            signal
                .map_err(|error| format!("wait for authentication cancellation: {error}"))
                .and_then(|()| Err("OS authentication was interrupted; approval was not confirmed".into()))
        }
    };
    let cleanup = stop_agent(&mut agent).await;
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(format!("authorization completed, but {error}")),
        (Err(error), Err(cleanup)) => Err(format!("{error}; {cleanup}")),
    }
}

async fn start_agent(path: &Path) -> Result<Child, String> {
    let pid = std::process::id();
    let start = crate::proc::read_start_time_ticks_pub(pid)
        .ok_or("cannot identify the terminal process for OS authentication")?;
    let mut agent = tokio::process::Command::new(path)
        // Polkit does not select --fallback agents for sessionless subjects.
        .arg("--process")
        .arg(format!("{pid},{start}"))
        // stdout is only the readiness channel; prompts use the controlling TTY.
        .arg("--notify-fd=1")
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| {
            format!(
                "start terminal authentication agent {}: {error}",
                path.display()
            )
        })?;
    let result = async {
        let mut ready = agent
            .stdout
            .take()
            .ok_or("terminal authentication agent has no readiness channel")?;
        let mut byte = [0];
        let count = tokio::time::timeout(Duration::from_secs(5), ready.read(&mut byte))
            .await
            .map_err(|_| "terminal authentication agent did not register within 5 seconds")?
            .map_err(|error| format!("read authentication agent readiness: {error}"))?;
        if count != 0 {
            return Err(
                "terminal authentication agent returned an invalid readiness signal".into(),
            );
        }
        if let Some(status) = agent
            .try_wait()
            .map_err(|error| format!("inspect terminal authentication agent: {error}"))?
        {
            return Err(format!(
                "terminal authentication agent exited during startup: {status}"
            ));
        }
        Ok(())
    }
    .await;
    if let Err(error) = result {
        return match stop_agent(&mut agent).await {
            Ok(()) => Err(error),
            Err(cleanup) => Err(format!("{error}; {cleanup}")),
        };
    }
    Ok(agent)
}

async fn stop_agent(agent: &mut Child) -> Result<(), String> {
    if agent
        .try_wait()
        .map_err(|error| format!("inspect authentication agent before stopping it: {error}"))?
        .is_some()
    {
        return Ok(());
    }
    let pid = agent
        .id()
        .ok_or("authentication agent lost its process identity")?;
    // pkttyagent's SIGTERM handler restores terminal attributes before exiting.
    if unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) } != 0 {
        return Err(format!(
            "stop terminal authentication agent: {}",
            std::io::Error::last_os_error()
        ));
    }
    match tokio::time::timeout(Duration::from_secs(5), agent.wait()).await {
        Ok(result) => result
            .map(|_| ())
            .map_err(|error| format!("wait for terminal authentication agent: {error}")),
        Err(_) => {
            agent
                .kill()
                .await
                .map_err(|error| format!("kill unresponsive authentication agent: {error}"))?;
            Err("terminal authentication agent did not stop within 5 seconds".into())
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/terminal/authorization.rs"
    ));
}
