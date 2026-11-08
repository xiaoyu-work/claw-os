//! Descriptor-pinned local files. Callers supply identity and sharing policy.

use std::{
    fs::{File, OpenOptions},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    },
    path::{Component, Path},
};

pub fn open_at(directory: &File, name: &std::ffi::OsStr, flags: i32) -> Result<File, String> {
    let name = std::ffi::CString::new(name.as_bytes()).map_err(|e| e.to_string())?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

pub fn directory(path: &Path) -> Result<File, String> {
    if !path.is_absolute() {
        return Err("source directory must be absolute".into());
    }
    let mut handle = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open("/")
        .map_err(|e| e.to_string())?;
    for part in path.components() {
        match part {
            Component::RootDir => {}
            Component::Normal(name) => {
                handle = open_at(&handle, name, libc::O_DIRECTORY | libc::O_PATH)?;
            }
            _ => return Err("source directory must be normalized".into()),
        }
    }
    Ok(handle)
}

pub fn source(path: &Path) -> Result<File, String> {
    let parent = directory(path.parent().ok_or("source path has no parent")?)?;
    open_at(
        &parent,
        path.file_name().ok_or("source path has no name")?,
        libc::O_RDONLY | libc::O_NONBLOCK,
    )
}
