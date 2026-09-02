//! Owner-scoped token usage queries for direct CLI clients.

use std::fs::File;
use std::path::PathBuf;

use serde_json::Value;

use super::client_identity::ClientIdentity;

pub async fn query(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let client = client.clone();
    tokio::task::spawn_blocking(move || query_blocking(params, &client))
        .await
        .map_err(|error| format!("Agent usage query worker failed: {error}"))?
}

fn query_blocking(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let args = params
        .get("args")
        .and_then(Value::as_array)
        .ok_or_else(|| "usage args are required".to_string())?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| "usage args must be strings".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let path = usage_path(client)?;
    match open_usage_file(client)? {
        Some(file) => crate::agent::usage_cmd_from_reader(
            &args,
            path.display().to_string(),
            file,
            crate::agent::llm::usage::MAX_QUERY_BYTES,
        ),
        None => crate::agent::usage_cmd_from_reader(
            &args,
            path.display().to_string(),
            std::io::empty(),
            crate::agent::llm::usage::MAX_QUERY_BYTES,
        ),
    }
}

fn usage_path(client: &ClientIdentity) -> Result<PathBuf, String> {
    let uid = client.require_uid()?;
    if uid == 0 {
        return Ok(crate::paths::ai_run_log_path());
    }
    Ok(crate::paths::clawd_user_state_dir(uid)
        .join("logs")
        .join("ai.jsonl"))
}

#[cfg(target_os = "linux")]
fn open_usage_file(client: &ClientIdentity) -> Result<Option<File>, String> {
    use std::ffi::CString;
    use std::fs::OpenOptions;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    fn optional(error: io::Error) -> Result<Option<File>, String> {
        if error.kind() == io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(format!("open AI usage log: {error}"))
        }
    }

    fn openat(parent: &File, name: &str, flags: i32) -> io::Result<File> {
        let name = CString::new(name)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid path component"))?;
        let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    fn expected_owner(uid: u32) -> u32 {
        #[cfg(test)]
        {
            let _ = uid;
            unsafe { libc::geteuid() as u32 }
        }
        #[cfg(not(test))]
        {
            uid
        }
    }

    fn require_directory(file: &File, owner_uid: u32, label: &str) -> Result<(), String> {
        let metadata = file
            .metadata()
            .map_err(|error| format!("inspect {label}: {error}"))?;
        if !metadata.is_dir() || metadata.uid() != expected_owner(owner_uid) {
            return Err(format!(
                "{label} is not a directory owned by uid {owner_uid}"
            ));
        }
        Ok(())
    }

    fn require_usage_file(file: &File, owner_uid: u32) -> Result<(), String> {
        let metadata = file
            .metadata()
            .map_err(|error| format!("inspect AI usage log: {error}"))?;
        if !metadata.is_file() || metadata.uid() != expected_owner(owner_uid) {
            return Err(format!(
                "AI usage log is not a regular file owned by uid {owner_uid}"
            ));
        }
        if metadata.len() > crate::agent::llm::usage::MAX_QUERY_BYTES {
            return Err(format!(
                "AI usage log exceeds the {} byte query limit",
                crate::agent::llm::usage::MAX_QUERY_BYTES
            ));
        }
        Ok(())
    }

    let uid = client.require_uid()?;
    if uid == 0 {
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(crate::paths::ai_run_log_path())
        {
            Ok(file) => file,
            Err(error) => return optional(error),
        };
        require_usage_file(&file, 0)?;
        return Ok(Some(file));
    }

    let users = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(crate::paths::data_dir().join("users"))
    {
        Ok(file) => file,
        Err(error) => return optional(error),
    };
    require_directory(&users, 0, "user-state directory")?;
    let owner = match openat(
        &users,
        &uid.to_string(),
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
    ) {
        Ok(file) => file,
        Err(error) => return optional(error),
    };
    require_directory(&owner, uid, "owner usage directory")?;
    let logs = match openat(
        &owner,
        "logs",
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
    ) {
        Ok(file) => file,
        Err(error) => return optional(error),
    };
    require_directory(&logs, uid, "owner usage log directory")?;
    let file = match openat(
        &logs,
        "ai.jsonl",
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
    ) {
        Ok(file) => file,
        Err(error) => return optional(error),
    };
    require_usage_file(&file, uid)?;
    Ok(Some(file))
}

#[cfg(not(target_os = "linux"))]
fn open_usage_file(_client: &ClientIdentity) -> Result<Option<File>, String> {
    Err("owner-scoped Agent usage queries require Linux".to_string())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/usage.rs"
    ));
}
