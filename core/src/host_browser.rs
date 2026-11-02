use std::io::Write;
use std::process::Stdio;
use std::time::{Duration, Instant};

const WINDOWS_OPEN_SCRIPT: &str =
    "$ErrorActionPreference = 'Stop'; Start-Process -FilePath ([Console]::In.ReadToEnd())";

pub(crate) fn is_wsl() -> bool {
    cfg!(target_os = "linux")
        && (std::env::var_os("WSL_INTEROP").is_some()
            || std::env::var_os("WSL_DISTRO_NAME").is_some()
            || std::fs::read_to_string("/proc/sys/kernel/osrelease")
                .is_ok_and(|release| release.to_ascii_lowercase().contains("microsoft")))
}

pub(crate) fn open_windows_browser(url: &str) -> Result<(), String> {
    let parsed = url::Url::parse(url).map_err(|_| "Browser URL is invalid.".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || url.len() > 4096
    {
        return Err(
            "The browser URL must be an HTTP(S) address without credentials, at most 4096 bytes."
                .into(),
        );
    }
    let program = if cfg!(target_os = "windows") {
        "powershell.exe"
    } else {
        "/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe"
    };
    let mut child = std::process::Command::new(program)
        .args(["-NoProfile", "-NonInteractive", "-Command", WINDOWS_OPEN_SCRIPT])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            format!("Windows browser could not be started: {error}. Open the displayed URL in Windows instead.")
        })?;
    let sent = child
        .stdin
        .take()
        .ok_or_else(|| "Windows browser launcher has no URL input.".to_string())
        .and_then(|mut input| {
            input
                .write_all(url.as_bytes())
                .map_err(|error| format!("Send browser URL to Windows: {error}"))
        });
    if let Err(error) = sent {
        let cleanup = child.kill().and_then(|()| child.wait().map(|_| ()));
        return Err(match cleanup {
            Ok(()) => error,
            Err(cleanup) => format!("{error}; browser launcher cleanup failed: {cleanup}"),
        });
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(_)) => {
                return Err("Windows could not open its default browser. Open the displayed URL in Windows instead.".into());
            }
            Err(error) => {
                let cleanup = child.kill().and_then(|()| child.wait().map(|_| ()));
                return Err(match cleanup {
                    Ok(()) => format!("Inspect Windows browser launcher: {error}"),
                    Err(cleanup) => format!(
                        "Inspect Windows browser launcher: {error}; cleanup failed: {cleanup}"
                    ),
                });
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                child
                    .kill()
                    .and_then(|()| child.wait().map(|_| ()))
                    .map_err(|error| format!("Stop timed-out Windows browser launcher: {error}"))?;
                return Err(
                    "Windows browser launch timed out. Open the displayed URL in Windows instead."
                        .into(),
                );
            }
        }
    }
}
