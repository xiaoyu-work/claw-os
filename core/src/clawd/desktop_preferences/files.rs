use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use super::request::{Entry, Namespace, MAX_ENTRIES, MAX_REQUEST, MAX_VALUE};
use super::{Authorized, BrokerError, PreferenceStore};
use crate::clawd::client_identity::FsIdentityGuard;

pub(super) struct OwnerFiles {
    owner: u32,
    home: PathBuf,
}

impl OwnerFiles {
    pub(super) fn new(owner: u32, home: PathBuf) -> Self {
        Self { owner, home }
    }

    fn directory(&self, namespace: Namespace, create: bool) -> Result<Option<File>, BrokerError> {
        let mut components = if matches!(namespace, Namespace::BackgroundState) {
            vec![".local", "state"]
        } else {
            vec![".config"]
        };
        components.extend([
            "cosmic",
            namespace
                .config_id()
                .ok_or_else(|| unavailable("this resource has no writable configuration"))?,
            "v1",
        ]);
        self.owner_directory(&components, create)
    }

    fn owner_directory(
        &self,
        components: &[&str],
        create: bool,
    ) -> Result<Option<File>, BrokerError> {
        let mut directory =
            crate::clawd::owner_files::directory(&self.home).map_err(unavailable)?;
        self.check_directory(&directory)?;
        for component in components {
            let component = std::ffi::CString::new(*component).map_err(unavailable)?;
            let mut fd = unsafe {
                libc::openat(
                    directory.as_raw_fd(),
                    component.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
                if !create {
                    return Ok(None);
                }
                if unsafe { libc::mkdirat(directory.as_raw_fd(), component.as_ptr(), 0o700) } != 0
                    && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
                {
                    return Err(unavailable(std::io::Error::last_os_error()));
                }
                fd = unsafe {
                    libc::openat(
                        directory.as_raw_fd(),
                        component.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
            }
            if fd < 0 {
                return Err(unavailable(std::io::Error::last_os_error()));
            }
            directory = unsafe { File::from_raw_fd(fd) };
            self.check_directory(&directory)?;
        }
        Ok(Some(directory))
    }

    fn check_directory(&self, directory: &File) -> Result<(), BrokerError> {
        let metadata = directory.metadata().map_err(unavailable)?;
        if !metadata.is_dir() || metadata.uid() != self.owner || metadata.mode() & 0o022 != 0 {
            return Err(BrokerError::authorization(
                "desktop preference directory is not exclusively owner-managed",
            ));
        }
        Ok(())
    }

    fn read_key(&self, directory: &File, key: &str) -> Result<Option<String>, BrokerError> {
        let key = std::ffi::CString::new(key).map_err(unavailable)?;
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                key.as_ptr(),
                libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            return if error.kind() == std::io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(unavailable(error))
            };
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let metadata = file.metadata().map_err(unavailable)?;
        if !metadata.is_file()
            || metadata.uid() != self.owner
            || metadata.nlink() != 1
            || metadata.mode() & 0o022 != 0
            || metadata.len() > MAX_VALUE as u64
        {
            return Err(BrokerError::authorization(
                "desktop preference must be a bounded, single-link owner file",
            ));
        }
        let mut value = String::new();
        file.take(MAX_VALUE as u64 + 1)
            .read_to_string(&mut value)
            .map_err(unavailable)?;
        if value.len() > MAX_VALUE || value.contains('\0') {
            return Err(unavailable("desktop preference exceeds its value bound"));
        }
        Ok(Some(value))
    }

    fn write_key(&self, directory: &File, entry: &Entry) -> Result<(), BrokerError> {
        self.read_key(directory, &entry.key)?;
        let temporary = format!(".cos-preference-{}", uuid::Uuid::new_v4().simple());
        let temporary_c = std::ffi::CString::new(temporary).map_err(unavailable)?;
        let key = std::ffi::CString::new(entry.key.as_str()).map_err(unavailable)?;
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                temporary_c.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(unavailable(std::io::Error::last_os_error()));
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        let prepared = file
            .write_all(entry.value.as_bytes())
            .and_then(|_| file.sync_all());
        if let Err(error) = prepared {
            unsafe {
                libc::unlinkat(directory.as_raw_fd(), temporary_c.as_ptr(), 0);
            }
            return Err(unavailable(error));
        }
        if unsafe {
            libc::renameat(
                directory.as_raw_fd(),
                temporary_c.as_ptr(),
                directory.as_raw_fd(),
                key.as_ptr(),
            )
        } != 0
        {
            let error = std::io::Error::last_os_error();
            unsafe {
                libc::unlinkat(directory.as_raw_fd(), temporary_c.as_ptr(), 0);
            }
            return Err(unavailable(error));
        }
        directory.sync_all().map_err(|_| {
            BrokerError::indeterminate(
                "desktop preference was replaced but directory durability is unconfirmed; refresh before retrying",
            )
        })?;
        let observed = self.read_key(directory, &entry.key).map_err(|_| BrokerError::indeterminate(
            "desktop preference was replaced but readback failed; refresh before retrying",
        ))?;
        if observed.as_deref() != Some(entry.value.as_str()) {
            return Err(BrokerError::indeterminate(
                "desktop preference changed after replacement; refresh before retrying",
            ));
        }
        Ok(())
    }
}

impl PreferenceStore for OwnerFiles {
    fn read(&self, namespace: Namespace, _proof: &Authorized) -> Result<Vec<Entry>, BrokerError> {
        let _identity = FsIdentityGuard::enter(self.owner).map_err(unavailable)?;
        if matches!(namespace, Namespace::KeyboardCatalog) {
            let mut entries = Vec::new();
            for (key, components) in [
                ("xdg", &[".config", "xkb", "rules"][..]),
                ("legacy", &[".xkb", "rules"][..]),
            ] {
                if let Some(directory) = self.owner_directory(components, false)? {
                    if let Some(value) = self.read_key(&directory, "evdev.xml")? {
                        entries.push(Entry {
                            key: key.into(),
                            value,
                        });
                    }
                }
            }
            return Ok(entries);
        }
        let Some(directory) = self.directory(namespace, false)? else {
            return Ok(Vec::new());
        };
        let mut entries = Vec::new();
        let mut bytes = 0;
        for entry in std::fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))
            .map_err(unavailable)?
        {
            let entry = entry.map_err(unavailable)?;
            let Some(key) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !namespace.admits_key(&key) {
                continue;
            }
            let Some(value) = self.read_key(&directory, &key)? else {
                continue;
            };
            bytes += key.len() + value.len();
            if bytes > MAX_REQUEST / 2 || entries.len() == MAX_ENTRIES {
                return Err(unavailable("desktop preference snapshot exceeds its bound"));
            }
            entries.push(Entry { key, value });
        }
        entries.sort_by(|left, right| left.key.cmp(&right.key));
        Ok(entries)
    }

    fn write(
        &self,
        namespace: Namespace,
        entries: &[Entry],
        _proof: &Authorized,
    ) -> Result<(), BrokerError> {
        let _identity = FsIdentityGuard::enter(self.owner).map_err(unavailable)?;
        let directory = self
            .directory(namespace, true)?
            .ok_or_else(|| unavailable("desktop preference directory is unavailable"))?;
        for (index, entry) in entries.iter().enumerate() {
            if let Err(error) = self.write_key(&directory, entry) {
                return if index == 0 {
                    Err(error)
                } else {
                    Err(BrokerError::indeterminate(
                        "some desktop preferences were written before a later failure; refresh before retrying; no rollback was attempted",
                    ))
                };
            }
        }
        Ok(())
    }
}

fn unavailable(error: impl std::fmt::Display) -> BrokerError {
    BrokerError::unavailable(format!("owner desktop preferences unavailable: {error}"))
}
