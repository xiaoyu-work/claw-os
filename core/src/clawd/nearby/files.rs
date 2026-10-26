//! Pinned owner file access and root-private transfer snapshots.

use super::super::client_identity::FsIdentityGuard;
use clawd_client::nearby::*;
use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        },
    },
    path::{Path, PathBuf},
};

pub(super) struct Store {
    pub root: PathBuf,
    pub owner: u32,
    pub home: PathBuf,
}
pub(super) struct Source {
    pub path: PathBuf,
    pub name: String,
}
pub(super) struct ImportError {
    pub message: String,
    pub paths: Vec<String>,
}
pub(super) struct Imported {
    pub paths: Vec<String>,
    pub bytes: u64,
}

fn error(value: impl std::fmt::Display) -> String {
    value.to_string()
}

pub(super) fn private(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(meta)
            if meta.is_dir()
                && !meta.file_type().is_symlink()
                && meta.uid() == 0
                && meta.mode() & 0o077 == 0 =>
        {
            Ok(())
        }
        Ok(_) => Err("nearby state directory is not private and root-owned".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(error)?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(error)
        }
        Err(e) => Err(error(e)),
    }
}

fn child(directory: &File, name: &str, flags: i32, mode: u32) -> Result<File, String> {
    let name = std::ffi::CString::new(name).map_err(error)?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            mode,
        )
    };
    if fd < 0 {
        return Err(error(std::io::Error::last_os_error()));
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

pub(super) fn directory(path: &Path) -> Result<File, String> {
    if !path.is_absolute() {
        return Err("directory must be absolute".into());
    }
    let mut handle = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY)
        .open("/")
        .map_err(error)?;
    for part in path.components() {
        match part {
            std::path::Component::RootDir => {}
            std::path::Component::Normal(name) => {
                handle = child(
                    &handle,
                    name.to_str().ok_or("directory must be UTF-8")?,
                    libc::O_DIRECTORY | libc::O_PATH,
                    0,
                )?;
            }
            _ => return Err("directory must be canonical".into()),
        }
    }
    Ok(handle)
}

impl Store {
    pub fn open(owner: u32) -> Result<Self, String> {
        if unsafe { libc::geteuid() } != 0 || owner == 0 {
            return Err("nearby sharing requires root clawd and a non-root owner".into());
        }
        let home = crate::paths::verified_home_for_uid(owner)?;
        let base = crate::paths::data_dir().join("nearby");
        private(&base)?;
        let root = base.join(owner.to_string());
        private(&root)?;
        for name in ["uploads", "sessions"] {
            private(&root.join(name))?;
        }
        Ok(Self { root, owner, home })
    }

    pub fn inbox(&self) -> PathBuf {
        self.home.join("Downloads").join("Claw Nearby")
    }

    pub fn save(&self, view: &NearbyView) -> Result<(), String> {
        view.validate(self.owner)?;
        crate::agent::util::atomic_write_with_fsync(
            &self.root.join("state.json"),
            &serde_json::to_vec(view).map_err(error)?,
        )
        .map_err(error)
    }

    pub fn load(&self) -> Result<Option<NearbyView>, String> {
        let path = self.root.join("state.json");
        let mut file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(error(e)),
        };
        let meta = file.metadata().map_err(error)?;
        if !meta.is_file()
            || meta.uid() != 0
            || meta.nlink() != 1
            || meta.len() > MAX_FRAME_BYTES as u64
        {
            return Err("nearby state file is unsafe or oversized".into());
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(error)?;
        let view: NearbyView =
            serde_json::from_slice(&bytes).map_err(|e| format!("corrupt nearby state: {e}"))?;
        view.validate(self.owner)?;
        Ok(Some(view))
    }

    pub fn session(&self, id: &str) -> Result<PathBuf, String> {
        if !valid_id(id) {
            return Err("invalid session identity".into());
        }
        let root = self.root.join("sessions").join(id);
        private(&root)?;
        for name in ["incoming", "outgoing"] {
            let path = root.join(name);
            if !path.exists() {
                fs::create_dir(&path).map_err(error)?;
                let raw = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(error)?;
                let gid = super::super::client_identity::owner_groups(self.owner)?.0;
                if unsafe { libc::chown(raw.as_ptr(), self.owner, gid) } != 0 {
                    return Err(error(std::io::Error::last_os_error()));
                }
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).map_err(error)?;
            }
            let meta = fs::symlink_metadata(&path).map_err(error)?;
            if !meta.is_dir() || meta.file_type().is_symlink() || meta.uid() != self.owner {
                return Err("unsafe nearby transport directory".into());
            }
        }
        Ok(root)
    }

    pub fn source(&self, raw: &str) -> Result<PathBuf, String> {
        let path = {
            let _owner = FsIdentityGuard::enter(self.owner)?;
            fs::canonicalize(raw).map_err(error)?
        };
        if !path.starts_with(&self.home) {
            return Err("send files must be inside the authenticated owner's home".into());
        }
        crate::worker::derive::reject_forbidden_for_owner(&path, &self.home)?;
        let _owner = FsIdentityGuard::enter(self.owner)?;
        let parent = directory(path.parent().ok_or("file has no parent")?)?;
        let file = child(
            &parent,
            path.file_name()
                .and_then(|s| s.to_str())
                .ok_or("invalid file name")?,
            libc::O_RDONLY | libc::O_NONBLOCK,
            0,
        )?;
        let meta = file.metadata().map_err(error)?;
        if !meta.is_file() || meta.len() > MAX_FILE_BYTES || meta.nlink() != 1 {
            return Err("send source must be a single-link regular file of at most 128 MiB".into());
        }
        Ok(path)
    }

    pub fn snapshot(
        &self,
        session: &str,
        transfer: &str,
        sources: &[Source],
        uploaded: bool,
    ) -> Result<(Vec<String>, Vec<SharedFile>), String> {
        let outgoing = self.session(session)?.join("outgoing");
        let root = outgoing.join(transfer);
        private(&root)?;
        let mut names = std::collections::HashSet::new();
        let mut metadata = Vec::new();
        let mut assigned = Vec::new();
        let mut total = 0u64;
        for source in sources {
            let path = &source.path;
            let name = source.name.as_str();
            if !file_name(name) || !names.insert(name.to_string()) {
                return Err("file names must be safe and distinct".into());
            }
            let mut input = {
                let _owner = if uploaded {
                    if !path.starts_with(self.root.join("uploads")) {
                        return Err("upload snapshot escaped its private root".into());
                    }
                    None
                } else {
                    Some(FsIdentityGuard::enter(self.owner)?)
                };
                let parent = directory(path.parent().ok_or("file has no parent")?)?;
                child(
                    &parent,
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .ok_or("invalid source file name")?,
                    libc::O_RDONLY | libc::O_NONBLOCK,
                    0,
                )?
            };
            let before = input.metadata().map_err(error)?;
            if !before.is_file()
                || before.nlink() != 1
                || before.len() > MAX_FILE_BYTES
                || uploaded && before.uid() != 0
            {
                return Err("source changed or exceeds size bound".into());
            }
            total = total
                .checked_add(before.len())
                .ok_or("source size overflow")?;
            if total > MAX_TOTAL_BYTES {
                return Err("files exceed 512 MiB".into());
            }
            let target = root.join(name);
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o400)
                .open(&target)
                .map_err(error)?;
            let copied = std::io::copy(
                &mut Read::by_ref(&mut input).take(MAX_FILE_BYTES + 1),
                &mut output,
            )
            .map_err(error)?;
            let after = input.metadata().map_err(error)?;
            if copied != before.len()
                || before.len() != after.len()
                || before.mtime() != after.mtime()
                || before.mtime_nsec() != after.mtime_nsec()
            {
                return Err("source changed while creating the send snapshot".into());
            }
            output.sync_all().map_err(error)?;
            let gid = super::super::client_identity::owner_groups(self.owner)?.0;
            if unsafe { libc::fchown(output.as_raw_fd(), self.owner, gid) } != 0 {
                return Err(error(std::io::Error::last_os_error()));
            }
            metadata.push(SharedFile {
                name: name.into(),
                bytes: Some(copied),
            });
            assigned.push(format!("/data/outgoing/{transfer}/{name}"));
        }
        fs::set_permissions(&root, fs::Permissions::from_mode(0o555)).map_err(error)?;
        crate::agent::util::sync_dir(&root).map_err(error)?;
        Ok((assigned, metadata))
    }

    pub fn upload_path(&self, id: &str) -> Result<PathBuf, String> {
        if !valid_id(id) {
            return Err("invalid upload identity".into());
        }
        Ok(self.root.join("uploads").join(id))
    }

    pub fn upload(&self, id: &str) -> Result<UploadView, String> {
        let root = self.upload_path(id)?;
        let bytes = fs::read(root.with_extension("json")).map_err(error)?;
        if bytes.len() > 4096 {
            return Err("upload metadata exceeds its bound".into());
        }
        let view: UploadView = serde_json::from_slice(&bytes).map_err(error)?;
        if view.id != id
            || !file_name(&view.name)
            || view.bytes > MAX_FILE_BYTES
            || view.written > view.bytes
            || view.ready && view.written != view.bytes
        {
            return Err("invalid upload metadata".into());
        }
        Ok(view)
    }

    pub fn uploads(&self) -> Result<Vec<UploadView>, String> {
        let mut uploads = Vec::new();
        for entry in fs::read_dir(self.root.join("uploads")).map_err(error)? {
            let entry = entry.map_err(error)?;
            if entry.path().extension().and_then(|s| s.to_str()) == Some("json") {
                let id = entry
                    .path()
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .ok_or("invalid upload name")?
                    .to_string();
                uploads.push(self.upload(&id)?);
                if uploads.len() > MAX_FILES {
                    return Err("upload metadata exceeds quota".into());
                }
            }
        }
        uploads.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(uploads)
    }

    pub fn upload_sources(&self, ids: &[String]) -> Result<Vec<Source>, String> {
        let mut paths = Vec::new();
        let mut names = std::collections::HashSet::new();
        for id in ids {
            let view = self.upload(id)?;
            if !view.ready {
                return Err("finish the upload before sending".into());
            }
            let source = self.upload_path(id)?;
            if !names.insert(view.name.clone()) {
                return Err("selected upload names are duplicated".into());
            }
            paths.push(Source {
                path: source,
                name: view.name,
            });
        }
        Ok(paths)
    }

    pub fn save_upload(&self, view: &UploadView) -> Result<(), String> {
        crate::agent::util::atomic_write_with_fsync(
            &self.upload_path(&view.id)?.with_extension("json"),
            &serde_json::to_vec(view).map_err(error)?,
        )
        .map_err(error)
    }

    pub fn delete_upload(&self, id: &str) -> Result<(), String> {
        self.upload(id)?;
        let data = self.upload_path(id)?;
        fs::rename(data.with_extension("json"), data.with_extension("deleting")).map_err(error)?;
        crate::agent::util::sync_dir(&self.root.join("uploads")).map_err(error)?;
        self.finish_upload_delete(id)
    }

    fn finish_upload_delete(&self, id: &str) -> Result<(), String> {
        let data = self.upload_path(id)?;
        match fs::remove_file(&data) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Upload deletion needs recovery: {error}")),
        }
        fs::remove_file(data.with_extension("deleting")).map_err(error)?;
        crate::agent::util::sync_dir(&self.root.join("uploads")).map_err(error)
    }

    pub fn recover_uploads(&self) -> Result<(), String> {
        for entry in fs::read_dir(self.root.join("uploads")).map_err(error)? {
            let path = entry.map_err(error)?.path();
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or("invalid private upload name")?;
            if path.extension().and_then(|value| value.to_str()) == Some("deleting") {
                let id = path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .ok_or("invalid deletion identity")?;
                self.finish_upload_delete(id)?;
            } else if valid_id(name) && !path.with_extension("json").exists() {
                let meta = match fs::symlink_metadata(&path) {
                    Ok(meta) => meta,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.to_string()),
                };
                if !meta.is_file()
                    || meta.file_type().is_symlink()
                    || meta.uid() != 0
                    || meta.nlink() != 1
                    || meta.len() > MAX_FILE_BYTES
                {
                    return Err(
                        "unsafe uncommitted upload; administrator inspection required".into(),
                    );
                }
                fs::remove_file(path).map_err(error)?;
            }
        }
        crate::agent::util::sync_dir(&self.root.join("uploads")).map_err(error)
    }

    pub fn retire_send(&self, session: &str, transfer: &str) -> Result<(), String> {
        if !valid_id(session) || !valid_id(transfer) {
            return Err("invalid retired transfer identity".into());
        }
        let root = self
            .root
            .join("sessions")
            .join(session)
            .join("outgoing")
            .join(transfer);
        match fs::symlink_metadata(&root) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() && meta.uid() == 0 => {
                fs::remove_dir_all(root).map_err(error)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            _ => Err("unsafe transfer staging directory; administrator inspection required".into()),
        }
    }

    pub fn retire_session(&self, id: &str) -> Result<(), String> {
        if !valid_id(id) {
            return Err("invalid retired window identity".into());
        }
        let root = self.root.join("sessions").join(id);
        let meta = fs::symlink_metadata(&root).map_err(error)?;
        if !meta.is_dir() || meta.file_type().is_symlink() || meta.uid() != 0 {
            return Err("unsafe window staging directory".into());
        }
        fs::remove_dir_all(root).map_err(error)?;
        crate::agent::util::sync_dir(&self.root.join("sessions")).map_err(error)
    }

    pub fn recover_sessions(&self) -> Result<(), String> {
        for entry in fs::read_dir(self.root.join("sessions")).map_err(error)? {
            let entry = entry.map_err(error)?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "invalid private window name")?;
            self.retire_session(&name)?;
        }
        Ok(())
    }

    pub fn import(
        &self,
        session: &str,
        transfer: &str,
        files: &[SharedFile],
        names: &[String],
    ) -> Result<Imported, ImportError> {
        let mut paths = Vec::new();
        let mut total = 0;
        let result = (|| {
            validate_files(files)?;
            let distinct: std::collections::HashSet<_> = names.iter().collect();
            if names.len() != files.len()
                || distinct.len() != names.len()
                || !names
                    .iter()
                    .all(|name| files.iter().any(|f| f.name == *name))
            {
                return Err("received files do not match the exact accepted offer".into());
            }
            let incoming = directory(&self.session(session)?.join("incoming"))?;
            let _owner = FsIdentityGuard::enter(self.owner)?;
            let home = directory(&self.home)?;
            let mkdir = |parent: &File, name: &str| -> Result<File, String> {
                let name_c = std::ffi::CString::new(name).map_err(error)?;
                if unsafe { libc::mkdirat(parent.as_raw_fd(), name_c.as_ptr(), 0o700) } != 0
                    && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
                {
                    return Err(error(std::io::Error::last_os_error()));
                }
                child(parent, name, libc::O_RDONLY | libc::O_DIRECTORY, 0)
            };
            let downloads = mkdir(&home, "Downloads")?;
            let inbox = mkdir(&downloads, "Claw Nearby")?;
            let destination = mkdir(&inbox, transfer)?;
            for name in names {
                if !file_name(name) {
                    return Err("unsafe received name".into());
                }
                let mut input = child(&incoming, name, libc::O_RDONLY | libc::O_NONBLOCK, 0)?;
                let meta = input.metadata().map_err(error)?;
                if !meta.is_file() || meta.nlink() != 1 || meta.len() > MAX_FILE_BYTES {
                    return Err("unsafe received file".into());
                }
                total += meta.len();
                if total > MAX_TOTAL_BYTES {
                    return Err("received files exceed 512 MiB".into());
                }
                let mut output = child(
                    &destination,
                    name,
                    libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                    0o600,
                )?;
                paths.push(
                    self.inbox()
                        .join(transfer)
                        .join(name)
                        .to_string_lossy()
                        .into_owned(),
                );
                let count = std::io::copy(
                    &mut Read::by_ref(&mut input).take(MAX_FILE_BYTES + 1),
                    &mut output,
                )
                .map_err(error)?;
                if count != meta.len() {
                    return Err("received file changed during import".into());
                }
                output.sync_all().map_err(error)?;
                let name_c = std::ffi::CString::new(name.as_str()).map_err(error)?;
                if unsafe { libc::unlinkat(incoming.as_raw_fd(), name_c.as_ptr(), 0) } != 0 {
                    return Err(format!(
                        "received file was persisted, but staging retirement failed: {}",
                        std::io::Error::last_os_error()
                    ));
                }
            }
            destination.sync_all().map_err(error)?;
            inbox.sync_all().map_err(error)?;
            Ok::<(), String>(())
        })();
        match result {
            Ok(()) => Ok(Imported {
                paths,
                bytes: total,
            }),
            Err(message) => Err(ImportError { message, paths }),
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/nearby/files.rs"
    ));
}
