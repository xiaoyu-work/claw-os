use std::ffi::CString;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use crate::{Connection, Error};

pub struct Listener {
    fd: OwnedFd,
    parent: OwnedFd,
    name: CString,
    socket_identity: (u64, u64),
}

impl Listener {
    pub fn bind(path: &Path, owner_uid: u32) -> Result<Self, Error> {
        if unsafe { libc::geteuid() } != 0 || unsafe { libc::getuid() } != 0 {
            return Err(Error::Identity);
        }
        let parent_path = path
            .parent()
            .ok_or(Error::Protocol("missing socket parent"))?;
        let name = path
            .file_name()
            .ok_or(Error::Protocol("missing socket name"))?
            .as_bytes();
        if name.is_empty()
            || !name
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(Error::Protocol("invalid socket name"));
        }
        let name = CString::new(name).map_err(|_| Error::Protocol("invalid socket name"))?;
        let parent = open_directory(parent_path)?;
        let metadata = std::fs::File::from(parent.try_clone()?).metadata()?;
        if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err(Error::Protocol(
                "socket parent must be protected and Root-owned",
            ));
        }
        let pinned_path = format!(
            "/proc/self/fd/{}/{}",
            parent.as_raw_fd(),
            name.to_str()
                .map_err(|_| Error::Protocol("invalid socket name"))?
        );
        let (address, length) = address(Path::new(&pinned_path))?;
        let fd = socket()?;
        let enabled: i32 = 1;
        if unsafe {
            libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PASSCRED,
                (&enabled as *const i32).cast(),
                std::mem::size_of::<i32>() as libc::socklen_t,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        if unsafe {
            libc::bind(
                fd.as_raw_fd(),
                (&address as *const libc::sockaddr_un).cast(),
                length,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        let metadata = std::fs::symlink_metadata(&pinned_path)?;
        let listener = Self {
            fd,
            parent,
            name,
            socket_identity: (metadata.dev(), metadata.ino()),
        };
        if unsafe {
            libc::fchownat(
                listener.parent.as_raw_fd(),
                listener.name.as_ptr(),
                owner_uid,
                u32::MAX,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
            || unsafe {
                libc::fchmodat(
                    listener.parent.as_raw_fd(),
                    listener.name.as_ptr(),
                    0o600,
                    0,
                )
            } != 0
            || unsafe { libc::listen(listener.fd.as_raw_fd(), 16) } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(listener)
    }

    pub fn accept(&self) -> Result<Option<Connection>, Error> {
        let raw = unsafe {
            libc::accept4(
                self.fd.as_raw_fd(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            )
        };
        if raw < 0 {
            let error = std::io::Error::last_os_error();
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            ) {
                return Ok(None);
            }
            return Err(error.into());
        }
        Connection::from_owned(unsafe { OwnedFd::from_raw_fd(raw) }).map(Some)
    }

    pub fn close(self) -> Result<(), Error> {
        self.unlink()
    }

    fn unlink(&self) -> Result<(), Error> {
        let mut metadata: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe {
            libc::fstatat(
                self.parent.as_raw_fd(),
                self.name.as_ptr(),
                &mut metadata,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            let error = std::io::Error::last_os_error();
            return if error.kind() == std::io::ErrorKind::NotFound {
                Ok(())
            } else {
                Err(error.into())
            };
        }
        if (metadata.st_dev, metadata.st_ino) != self.socket_identity {
            return Err(Error::Protocol("display listener pathname was replaced"));
        }
        if unsafe { libc::unlinkat(self.parent.as_raw_fd(), self.name.as_ptr(), 0) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
}

impl AsFd for Listener {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        if let Err(error) = self.unlink() {
            eprintln!("display listener cleanup failed: {error}");
        }
    }
}

impl Connection {
    pub fn connect(path: &Path) -> Result<Self, Error> {
        let (address, length) = address(path)?;
        let fd = socket()?;
        // Unix seqpacket connect has no remote network wait; a full accept queue
        // is refused rather than blocking a PAM or compositor thread.
        if unsafe {
            libc::connect(
                fd.as_raw_fd(),
                (&address as *const libc::sockaddr_un).cast(),
                length,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        Self::from_owned(fd)
    }
}

fn socket() -> Result<OwnedFd, Error> {
    let raw = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        )
    };
    if raw < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

fn address(path: &Path) -> Result<(libc::sockaddr_un, libc::socklen_t), Error> {
    let bytes = path.as_os_str().as_bytes();
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if !path.is_absolute() || bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
        return Err(Error::Protocol("invalid Unix control socket path"));
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (output, byte) in address.sun_path.iter_mut().zip(bytes) {
        *output = *byte as libc::c_char;
    }
    let length = std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1;
    Ok((address, length as libc::socklen_t))
}

fn open_directory(path: &Path) -> Result<OwnedFd, Error> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| Error::Protocol("invalid directory"))?;
    let raw = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}
