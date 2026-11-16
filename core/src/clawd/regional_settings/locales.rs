use crate::clawd::protocol::BrokerError;
use std::os::unix::process::CommandExt;
use std::process::Stdio;
use std::time::Duration;

pub(super) async fn installed(owner: u32) -> Result<Vec<String>, BrokerError> {
    let (gid, groups) =
        crate::clawd::client_identity::owner_groups(owner).map_err(BrokerError::unavailable)?;
    let mut command = tokio::process::Command::new("/usr/bin/locale");
    command
        .arg("-a")
        .env_clear()
        .env("LC_ALL", "C")
        .env("PATH", "/usr/bin:/bin")
        .current_dir("/")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    unsafe {
        command.as_std_mut().pre_exec(move || {
            if libc::setgroups(groups.len(), groups.as_ptr()) != 0
                || libc::setgid(gid) != 0
                || libc::setuid(owner) != 0
                || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let output = tokio::time::timeout(Duration::from_secs(5), command.output())
        .await
        .map_err(|_| BrokerError::unavailable("installed locale observation timed out"))?
        .map_err(|_| BrokerError::unavailable("installed locale reader is unavailable"))?;
    if !output.status.success() || output.stdout.len() > 128 * 1024 {
        return Err(BrokerError::unavailable(
            "installed locale observation failed or exceeded its bound",
        ));
    }
    let text = std::str::from_utf8(&output.stdout)
        .map_err(|_| BrokerError::unavailable("installed locale observation was not UTF-8"))?;
    let values: Vec<String> = text.lines().map(str::to_owned).collect();
    if values.len() > 4096
        || values
            .iter()
            .any(|value| super::request::validate_locale(value).is_err())
    {
        return Err(BrokerError::unavailable(
            "invalid installed locale observation",
        ));
    }
    Ok(values)
}
