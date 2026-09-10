//! Protected presentation revisions. Comparison state is never grant authority.

use std::path::Path;

use serde::{de::DeserializeOwned, Deserialize, Serialize};

const MAX_STAMP_BYTES: u64 = 4096;
pub(crate) const MAX_APPROVAL_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stamp {
    owner_uid: u32,
    request_id: String,
    fingerprint: String,
    revision: u64,
}

pub(crate) fn revision(owner_uid: u32, request_id: &str, fingerprint: &str) -> Result<u64, String> {
    if request_id.is_empty()
        || request_id.len() > 128
        || !request_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("invalid system review presentation id".into());
    }
    if fingerprint.len() != 64
        || !fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("system review presentation fingerprint must be lowercase SHA-256".into());
    }
    let path = super::root()
        .join("presentation")
        .join(format!("{owner_uid}-{request_id}.json"));
    super::ensure_dirs().map_err(|error| format!("approvals directory: {error}"))?;
    crate::filelock::with_exclusive_path_lock(&super::grant_lock_path(), || {
        let previous = read_owned_json::<Stamp>(&path, MAX_STAMP_BYTES)?;
        if let Some(previous) = &previous {
            if previous.owner_uid != owner_uid
                || previous.request_id != request_id
                || previous.revision == 0
            {
                return Err("system review presentation ownership is invalid".into());
            }
            if previous.fingerprint == fingerprint {
                return Ok(previous.revision);
            }
        }
        let revision = previous.map_or(Ok(1), |stamp| {
            stamp
                .revision
                .checked_add(1)
                .ok_or("system review presentation revision exhausted")
        })?;
        let stamp = Stamp {
            owner_uid,
            request_id: request_id.to_string(),
            fingerprint: fingerprint.to_string(),
            revision,
        };
        let bytes = serde_json::to_vec(&stamp)
            .map_err(|error| format!("encode system review presentation revision: {error}"))?;
        super::write_atomic_with(&path, &bytes, super::Durability::Committed)
            .map_err(|error| format!("persist system review presentation revision: {error}"))?;
        Ok(revision)
    })
}

#[cfg(unix)]
pub(crate) fn read_owned_json<T: DeserializeOwned>(
    path: &Path,
    maximum: u64,
) -> Result<Option<T>, String> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("read protected approval: {error}")),
    };
    let metadata = file
        .metadata()
        .map_err(|error| format!("inspect protected approval: {error}"))?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.mode() & 0o077 != 0
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.len() > maximum
    {
        return Err("approval must be a private, owned, bounded regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read protected approval bytes: {error}"))?;
    if bytes.len() as u64 > maximum {
        return Err("protected approval exceeds its byte limit".into());
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| format!("decode protected approval: {error}"))
}

#[cfg(not(unix))]
pub(crate) fn read_owned_json<T: DeserializeOwned>(
    _path: &Path,
    _maximum: u64,
) -> Result<Option<T>, String> {
    Err("protected approval presentation requires a Unix host".into())
}

pub(crate) fn legacy_pending(owner_uid: u32) -> Result<Vec<super::Request>, String> {
    let entries = match std::fs::read_dir(super::pending_dir()) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("list protected approvals: {error}")),
    };
    let mut requests = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|error| format!("read approval directory entry: {error}"))?
            .path();
        if path.extension() != Some(std::ffi::OsStr::new("json")) {
            continue;
        }
        let Some(request) = read_owned_json::<super::Request>(&path, MAX_APPROVAL_BYTES)? else {
            continue;
        };
        super::validate_approval_id(&request.id)?;
        if path.file_stem() != Some(std::ffi::OsStr::new(&request.id)) {
            return Err("approval filename does not match its request".into());
        }
        if request.owner_uid == Some(owner_uid) {
            requests.push(request);
        }
    }
    requests.sort_by_key(|request| request.requested_at);
    Ok(requests)
}

pub(crate) enum LegacyReview {
    Pending(Box<super::Request>),
    Resolved(Box<super::Resolved>),
}

pub(crate) fn legacy_get(owner_uid: u32, id: &str) -> Result<LegacyReview, String> {
    super::validate_approval_id(id)?;
    let name = format!("{id}.json");
    if let Some(request) =
        read_owned_json::<super::Request>(&super::pending_dir().join(&name), MAX_APPROVAL_BYTES)?
    {
        if request.id != id || request.owner_uid != Some(owner_uid) {
            return Err("approval is not owned by this user".into());
        }
        return Ok(LegacyReview::Pending(Box::new(request)));
    }
    for directory in [
        super::approved_dir(),
        super::consumed_dir(),
        super::denied_dir(),
    ] {
        if let Some(resolved) =
            read_owned_json::<super::Resolved>(&directory.join(&name), MAX_APPROVAL_BYTES)?
        {
            if resolved.request.id != id || resolved.request.owner_uid != Some(owner_uid) {
                return Err("approval is not owned by this user".into());
            }
            return Ok(LegacyReview::Resolved(Box::new(resolved)));
        }
    }
    Err("approval is unavailable or changing; refresh its state".into())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/approvals/presentation.rs"
    ));
}
