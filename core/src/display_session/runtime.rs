use std::ffi::CString;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

use claw_display_control::{Epoch, Error, InstanceId};

const ROOT: &str = "/run/cos/gui";

#[derive(Clone, Copy)]
pub(crate) enum Socket {
    Wayland,
    WaylandTransport,
    Broker,
    Egress,
    EgressTransport,
}

impl Socket {
    fn name(self) -> &'static str {
        match self {
            Self::Wayland => "wayland.sock",
            Self::WaylandTransport => "display-transport.sock",
            Self::Broker => "broker.sock",
            Self::Egress => "egress.sock",
            Self::EgressTransport => "egress-transport.sock",
        }
    }
}

pub(crate) struct Runtime {
    path: PathBuf,
    identity: (u64, u64),
    sockets: Vec<(PathBuf, u64, u64)>,
    private_created: bool,
    removed: bool,
}

impl Runtime {
    pub fn create(instance: InstanceId) -> Result<Self, Error> {
        if unsafe { libc::geteuid() } != 0 {
            return Err(Error::Identity);
        }
        for parent in [Path::new("/run"), Path::new("/run/cos")] {
            check_root_directory(parent, false)?;
        }
        match std::fs::DirBuilder::new().mode(0o700).create(ROOT) {
            Ok(()) => std::fs::set_permissions(ROOT, std::fs::Permissions::from_mode(0o711))?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        check_root_directory(Path::new(ROOT), true)?;
        let path = Path::new(ROOT).join(Epoch(instance.0).label());
        std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        let metadata = std::fs::symlink_metadata(&path)?;
        let mut runtime = Self {
            path,
            identity: (metadata.dev(), metadata.ino()),
            sockets: Vec::new(),
            private_created: false,
            removed: false,
        };
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(runtime.path.join("private"))?;
        runtime.private_created = true;
        // Bubblewrap resolves each pinned front endpoint after dropping uid.
        // Traversal is not admission: every front validates its instance writer.
        std::fs::set_permissions(&runtime.path, std::fs::Permissions::from_mode(0o711))?;
        Ok(runtime)
    }

    pub fn socket(&self, socket: Socket) -> PathBuf {
        if matches!(socket, Socket::Wayland | Socket::Egress) {
            self.path.join("private").join(socket.name())
        } else {
            self.path.join(socket.name())
        }
    }

    pub fn bind(&mut self, socket: Socket, owner: u32) -> Result<UnixListener, Error> {
        if owner == 0 || self.removed {
            return Err(Error::Identity);
        }
        let path = self.socket(socket);
        let listener = UnixListener::bind(&path)?;
        self.adopt(socket, owner)?;
        Ok(listener)
    }

    pub fn adopt(&mut self, socket: Socket, owner: u32) -> Result<(), Error> {
        if owner == 0 || self.removed {
            return Err(Error::Identity);
        }
        let path = self.socket(socket);
        let metadata = std::fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_socket() || metadata.uid() != 0 || metadata.nlink() != 1 {
            return Err(Error::Identity);
        }
        self.sockets
            .push((path.clone(), metadata.dev(), metadata.ino()));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let path =
            CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| Error::Identity)?;
        if unsafe { libc::chown(path.as_ptr(), owner, u32::MAX) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }

    pub fn remove(&mut self) -> Result<(), Error> {
        if self.removed {
            return Ok(());
        }
        let metadata = std::fs::symlink_metadata(&self.path)?;
        if (metadata.dev(), metadata.ino()) != self.identity || metadata.uid() != 0 {
            return Err(Error::Identity);
        }
        for (path, dev, ino) in &self.sockets {
            match std::fs::symlink_metadata(path) {
                Ok(metadata) => {
                    if metadata.dev() != *dev
                        || metadata.ino() != *ino
                        || !metadata.file_type().is_socket()
                    {
                        return Err(Error::Identity);
                    }
                    std::fs::remove_file(path)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        if self.private_created {
            let path = self.path.join("private");
            let metadata = std::fs::symlink_metadata(&path)?;
            if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o077 != 0 {
                return Err(Error::Protocol(
                    "GUI backend directory is not protected by Root",
                ));
            }
            std::fs::remove_dir(path)?;
            self.private_created = false;
        }
        std::fs::remove_dir(&self.path)?;
        self.removed = true;
        Ok(())
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        if let Err(error) = self.remove() {
            tracing::error!(%error, "GUI runtime endpoint cleanup failed");
        }
    }
}

fn check_root_directory(path: &Path, traverse_only: bool) -> Result<(), Error> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || metadata.uid() != 0
        || if traverse_only {
            metadata.mode() & 0o077 != 0o011
        } else {
            metadata.mode() & 0o022 != 0
        }
    {
        return Err(Error::Protocol(
            "GUI runtime ancestor is not protected by Root",
        ));
    }
    Ok(())
}
