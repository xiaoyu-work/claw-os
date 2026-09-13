//! Owner-scoped Calendar observation. Only the OS reader receives database
//! descriptors; callers receive bounded records, never paths or file access.

use std::time::Duration;

use serde_json::Value;
use tokio::io::AsyncReadExt;

use super::authority::Decision;
use super::client_identity::ClientIdentity;
use super::protocol::BrokerError;
use super::wire::requests::CalendarDay;
use crate::caps::{Cap, Scope, Verb};

mod files;

const READER: &str = "/usr/lib/cos/bin/claw-calendar-reader";
const MAX_OUTPUT: usize = 1024 * 1024;
const MAX_CALL: Duration = Duration::from_secs(5);

pub(crate) fn validate_day(year: i16, month: u8, day: u8) -> Result<(), String> {
    if !(-9999..=9999).contains(&year)
        || chrono::NaiveDate::from_ymd_opt(year.into(), month.into(), day.into()).is_none()
    {
        return Err("Calendar requires a valid Gregorian date in years -9999..9999".into());
    }
    Ok(())
}

fn authorize(
    authority: &Decision,
    owner: u32,
) -> Result<super::authority::Authorized, BrokerError> {
    if authority.owner_uid() != owner {
        return Err(BrokerError::authorization(
            "Calendar authority belongs to another owner",
        ));
    }
    authority
        .require(Cap::new(Verb::DATA_DB_READ, Scope::name("calendar")))
        .map_err(BrokerError::authorization)
}

pub async fn day(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, BrokerError> {
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    let request: CalendarDay = serde_json::from_value(params)
        .map_err(|error| BrokerError::execution(format!("invalid Calendar request: {error}")))?;
    validate_day(request.year, request.month, request.day).map_err(BrokerError::execution)?;
    let owner = client.require_uid().map_err(BrokerError::authorization)?;
    if unsafe { libc::geteuid() } != 0 || owner == 0 {
        return Err(BrokerError::authorization(
            "Calendar requires root clawd and a non-root owner",
        ));
    }
    let _authorized = authorize(authority, owner)?;
    crate::agentd::spawn::validate_root_owned_executable(std::path::Path::new(READER)).map_err(
        |error| BrokerError::unavailable(format!("Calendar reader is unavailable: {error}")),
    )?;
    crate::update::runtime::enforce_component_binary(
        "claw-calendar-reader",
        std::path::Path::new(READER),
    )
    .map_err(|error| BrokerError::unavailable(error.message))?;
    let home = crate::paths::verified_home_for_uid(owner).map_err(BrokerError::unavailable)?;
    let root = crate::paths::RoutedPathContext::for_owner(owner, home)
        .scope_sync(crate::paths::user_data_dir);
    let Some(data) = files::Database::open(&root, owner).map_err(BrokerError::execution)? else {
        return Ok(serde_json::json!({"events": []}));
    };
    let runtime = files::QueryView::prepare(data).map_err(BrokerError::unavailable)?;
    let group = super::client_identity::owner_groups(owner)
        .map_err(BrokerError::unavailable)?
        .0;
    let (parent, child_gate) =
        UnixStream::pair().map_err(|error| BrokerError::execution(error.to_string()))?;
    let gate = child_gate.as_raw_fd();
    let expected_parent = unsafe { libc::getpid() };
    let mut command = std::process::Command::new(READER);
    command
        .args([
            "--query-v1",
            &request.year.to_string(),
            &request.month.to_string(),
            &request.day.to_string(),
            &gate.to_string(),
        ])
        .env_clear()
        .env("LC_ALL", "C.UTF-8")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(timezone) = std::env::var_os("TZ") {
        command.env("TZ", timezone);
    }
    let mount = runtime.mount();
    let directory = runtime.directory();
    unsafe {
        command.pre_exec(move || {
            files::install(mount, &directory)?;
            crate::agentd::spawn::mark_inherited_descriptors_cloexec_except(3, gate);
            if libc::setgroups(0, std::ptr::null()) != 0
                || libc::setgid(group) != 0
                || libc::setuid(owner) != 0
                || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
                || libc::fcntl(gate, libc::F_SETFD, 0) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != expected_parent {
                return Err(std::io::Error::from_raw_os_error(libc::EPIPE));
            }
            for (resource, limit) in [
                (libc::RLIMIT_AS, 256 * 1024 * 1024),
                (libc::RLIMIT_CPU, 4),
                (libc::RLIMIT_CORE, 0),
                (libc::RLIMIT_FSIZE, 0),
            ] {
                let limit = libc::rlimit {
                    rlim_cur: limit,
                    rlim_max: limit,
                };
                if libc::setrlimit(resource, &limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let mut child = tokio::process::Command::from(command)
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| BrokerError::unavailable(format!("start Calendar reader: {error}")))?;
    drop(child_gate);
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| BrokerError::execution("Calendar stdout unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| BrokerError::execution("Calendar stderr unavailable"))?;
    let result = tokio::time::timeout(MAX_CALL, async {
        let mut output = Vec::new();
        let mut errors = Vec::new();
        let read = async {
            stdout
                .take((MAX_OUTPUT + 1) as u64)
                .read_to_end(&mut output)
                .await?;
            if output.len() > MAX_OUTPUT {
                return Err(std::io::Error::other("Calendar result exceeds one MiB"));
            }
            Ok(())
        };
        let diagnostics = async {
            stderr.take(8193).read_to_end(&mut errors).await?;
            if errors.len() > 8192 {
                return Err(std::io::Error::other(
                    "Calendar diagnostics exceed their byte limit",
                ));
            }
            Ok(())
        };
        tokio::try_join!(read, diagnostics).map_err(|error| error.to_string())?;
        let status = child.wait().await.map_err(|error| error.to_string())?;
        if !status.success() {
            return Err(format!(
                "Calendar reader failed ({status}): {}",
                String::from_utf8_lossy(&errors).trim()
            ));
        }
        let value: Value = serde_json::from_slice(&output)
            .map_err(|error| format!("invalid Calendar result: {error}"))?;
        if !value.as_object().is_some_and(|object| {
            object.len() == 1 && object.get("events").is_some_and(Value::is_array)
        }) {
            return Err("Calendar reader returned an invalid result shape".into());
        }
        Ok(value)
    })
    .await
    .map_err(|_| "Calendar reader timed out".to_string())
    .and_then(|result| result);
    if result.is_err() {
        child
            .kill()
            .await
            .map_err(|error| BrokerError::execution(format!("stop Calendar reader: {error}")))?;
        child
            .wait()
            .await
            .map_err(|error| BrokerError::execution(format!("reap Calendar reader: {error}")))?;
    }
    drop(parent);
    result.map_err(BrokerError::execution)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/calendar.rs"
    ));
}
