//! Closed Root desktop effects and immutable compositor response data.

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{FileExt, MetadataExt};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Error;

pub const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const SEALS: i32 = libc::F_SEAL_WRITE | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Command {
    List,
    Focus { identifier: u64 },
    Close { identifier: u64 },
    CloseApp { identifier: u64, app_id: String },
}

impl Command {
    pub fn validate(&self) -> Result<(), Error> {
        let identifier = match self {
            Self::List => return Ok(()),
            Self::Focus { identifier } | Self::Close { identifier } => *identifier,
            Self::CloseApp { identifier, app_id } => {
                if app_id.is_empty()
                    || app_id.len() > 255
                    || !app_id.as_bytes()[0].is_ascii_alphanumeric()
                    || !app_id.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
                    })
                {
                    return Err(Error::Protocol("invalid desktop AppID"));
                }
                *identifier
            }
        };
        if identifier == 0 {
            return Err(Error::Protocol("invalid desktop window identifier"));
        }
        Ok(())
    }
}

pub fn seal_response(bytes: &[u8]) -> Result<OwnedFd, Error> {
    validate_response(bytes)?;
    let raw = unsafe {
        libc::memfd_create(
            c"claw-desktop-response".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if raw < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut file = File::from(unsafe { OwnedFd::from_raw_fd(raw) });
    file.write_all(bytes)?;
    file.seek(SeekFrom::Start(0))?;
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, SEALS) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(file.into())
}

pub fn receive_response(descriptor: OwnedFd, owner: u32) -> Result<Value, Error> {
    let file = File::from(descriptor);
    let info = file.metadata()?;
    let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
    if !info.is_file()
        || info.uid() != owner
        || info.len() == 0
        || info.len() > MAX_RESPONSE_BYTES as u64
        || seals < 0
        || seals & SEALS != SEALS
    {
        return Err(Error::Protocol(
            "desktop response is not bounded and sealed",
        ));
    }
    let mut bytes = vec![0; info.len() as usize];
    file.read_exact_at(&mut bytes, 0)?;
    validate_response(&bytes)
}

fn validate_response(bytes: &[u8]) -> Result<Value, Error> {
    if bytes.is_empty() || bytes.len() > MAX_RESPONSE_BYTES {
        return Err(Error::Protocol("desktop response exceeds its bound"));
    }
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| Error::Protocol("invalid desktop response JSON"))?;
    if !value.is_object() {
        return Err(Error::Protocol("desktop response must be an object"));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/desktop.rs"));
}
