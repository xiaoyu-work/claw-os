//! Bounded OS-private selection data; possession never supplies authority.

use crate::Error;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_ENCODED: u64 = 16 * 1024 * 1024 + 65536;
const SEALS: i32 = libc::F_SEAL_WRITE | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL;
static TRANSFERS: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub mime: String,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Payload {
    pub entries: Vec<Entry>,
}

impl Payload {
    pub fn validate(&self) -> Result<(), Error> {
        let mut size = 0usize;
        if self.entries.is_empty() || self.entries.len() > 64 {
            return Err(Error::Protocol("invalid selection MIME count"));
        }
        for (index, entry) in self.entries.iter().enumerate() {
            if entry.mime.len() > 256
                || !entry.mime.contains('/')
                || !entry.mime.bytes().all(|byte| (33..=126).contains(&byte))
                || self.entries[..index]
                    .iter()
                    .any(|previous| previous.mime == entry.mime)
            {
                return Err(Error::Protocol("invalid selection MIME"));
            }
            size = size.checked_add(entry.data.len()).ok_or(Error::Identity)?;
            if size > MAX_BYTES {
                return Err(Error::Protocol("selection data exceeds its bound"));
            }
        }
        Ok(())
    }

    pub fn sealed(&self) -> Result<OwnedFd, Error> {
        self.validate()?;
        if unsafe { libc::geteuid() } != 0 {
            return Err(Error::Identity);
        }
        let fd = unsafe {
            libc::memfd_create(
                c"claw-selection".as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut file = File::from(unsafe { OwnedFd::from_raw_fd(fd) });
        serde_json::to_writer(&mut file, self)
            .map_err(|_| Error::Protocol("encode selection data"))?;
        if file.metadata()?.len() > MAX_ENCODED {
            return Err(Error::Protocol("encoded selection data exceeds its bound"));
        }
        file.seek(SeekFrom::Start(0))?;
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, SEALS) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(file.into())
    }

    pub fn receive(fd: OwnedFd) -> Result<Self, Error> {
        let file = File::from(fd);
        let info = file.metadata()?;
        let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
        if !info.is_file()
            || info.uid() != 0
            || info.len() > MAX_ENCODED
            || seals < 0
            || seals & SEALS != SEALS
        {
            return Err(Error::Protocol(
                "selection descriptor is not bounded and sealed",
            ));
        }
        let mut bytes = Vec::new();
        file.take(MAX_ENCODED + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != info.len() {
            return Err(Error::Protocol("selection descriptor changed size"));
        }
        let payload: Self = serde_json::from_slice(&bytes)
            .map_err(|_| Error::Protocol("invalid selection data"))?;
        payload.validate()?;
        Ok(payload)
    }

    pub fn mime_types(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|entry| entry.mime.clone())
            .collect()
    }

    pub fn send(self: &Arc<Self>, mime: &str, fd: OwnedFd) {
        let Some(index) = self.entries.iter().position(|entry| entry.mime == mime) else {
            return;
        };
        if TRANSFERS.fetch_add(1, Ordering::AcqRel) >= 32 {
            TRANSFERS.fetch_sub(1, Ordering::AcqRel);
            return;
        }
        struct Transfer;
        impl Drop for Transfer {
            fn drop(&mut self) {
                TRANSFERS.fetch_sub(1, Ordering::AcqRel);
            }
        }
        let transfer = Transfer;
        let payload = self.clone();
        let result = std::thread::Builder::new()
            .name("selection-send".into())
            .spawn(move || {
                let _transfer = transfer;
                let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
                if flags < 0
                    || unsafe {
                        libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK)
                    } < 0
                {
                    return;
                }
                let deadline = Instant::now() + Duration::from_secs(2);
                let bytes = &payload.entries[index].data;
                let mut offset = 0;
                while offset < bytes.len() && Instant::now() < deadline {
                    let mut poll = libc::pollfd {
                        fd: fd.as_raw_fd(),
                        events: libc::POLLOUT,
                        revents: 0,
                    };
                    let left = deadline
                        .saturating_duration_since(Instant::now())
                        .as_millis();
                    let ready = unsafe { libc::poll(&mut poll, 1, left.min(2000) as i32) };
                    if ready < 0
                        && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR)
                    {
                        continue;
                    }
                    if ready <= 0 {
                        return;
                    }
                    let count = unsafe {
                        libc::write(
                            fd.as_raw_fd(),
                            bytes[offset..].as_ptr().cast(),
                            bytes.len() - offset,
                        )
                    };
                    if count > 0 {
                        offset += count as usize;
                    } else if count < 0
                        && matches!(
                            std::io::Error::last_os_error().raw_os_error(),
                            Some(libc::EINTR | libc::EAGAIN)
                        )
                    {
                        continue;
                    } else {
                        return;
                    }
                }
            });
        drop(result);
    }
}
