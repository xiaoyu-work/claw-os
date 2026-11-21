//! Closed Root desktop effects and immutable compositor response data.

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{FileExt, MetadataExt};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Error;

pub const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_VISUAL_EDGE: u32 = 640;
pub const MAX_WINDOW_EDGE: u32 = 4096;
pub const VISUAL_FRAME_TTL_MS: u64 = 90_000;
const SEALS: i32 = libc::F_SEAL_WRITE | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PixelRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl PixelRegion {
    pub fn valid(self) -> bool {
        (1..=MAX_VISUAL_EDGE).contains(&self.width)
            && (1..=MAX_VISUAL_EDGE).contains(&self.height)
            && self.x <= MAX_WINDOW_EDGE - self.width
            && self.y <= MAX_WINDOW_EDGE - self.height
    }
}

pub fn valid_observation(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Command {
    List,
    Observe,
    Focus {
        identifier: u64,
    },
    Close {
        identifier: u64,
    },
    CloseApp {
        identifier: u64,
        app_id: String,
    },
    FocusWorkspace {
        workspace: String,
    },
    ToggleSpecial {
        workspace: String,
    },
    MoveWindow {
        identifier: u64,
        workspace: String,
    },
    ToggleFloating {
        identifier: u64,
    },
    TogglePin {
        identifier: u64,
    },
    VisualCapture {
        identifier: u64,
        observation: String,
        region: Option<PixelRegion>,
    },
    VisualClick {
        identifier: u64,
        observation: String,
        x: u32,
        y: u32,
    },
}

pub fn valid_workspace(value: &str) -> bool {
    if let Some(name) = value
        .strip_prefix("special:")
        .or_else(|| value.strip_prefix("name:"))
    {
        return !name.is_empty()
            && value.len() < 80
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'));
    }
    matches!(value, "r-1" | "r+1")
        || (value.len() <= 10
            && !value.starts_with('0')
            && value.bytes().all(|b| b.is_ascii_digit())
            && value.parse::<u32>().is_ok())
}

impl Command {
    pub fn validate(&self) -> Result<(), Error> {
        let identifier = match self {
            Self::List | Self::Observe => return Ok(()),
            Self::Focus { identifier }
            | Self::Close { identifier }
            | Self::ToggleFloating { identifier }
            | Self::TogglePin { identifier } => *identifier,
            Self::VisualCapture {
                identifier,
                observation,
                region,
            } if valid_observation(observation) && region.is_none_or(PixelRegion::valid) => {
                *identifier
            }
            Self::VisualClick {
                identifier,
                observation,
                x,
                y,
            } if valid_observation(observation) && *x < MAX_VISUAL_EDGE && *y < MAX_VISUAL_EDGE => {
                *identifier
            }
            Self::VisualCapture { .. } | Self::VisualClick { .. } => {
                return Err(Error::Protocol("invalid desktop visual request"));
            }
            Self::FocusWorkspace { workspace } if valid_workspace(workspace) => return Ok(()),
            Self::ToggleSpecial { workspace }
                if workspace.starts_with("special:") && valid_workspace(workspace) =>
            {
                return Ok(());
            }
            Self::MoveWindow {
                identifier,
                workspace,
            } if !matches!(workspace.as_str(), "r-1" | "r+1") && valid_workspace(workspace) => {
                *identifier
            }
            Self::FocusWorkspace { .. } | Self::ToggleSpecial { .. } | Self::MoveWindow { .. } => {
                return Err(Error::Protocol("invalid desktop workspace"));
            }
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
