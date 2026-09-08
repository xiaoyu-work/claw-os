//! Capability-scoped text files for App workers. No App dispatch or host mounts.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use serde_json::{json, Value};

use super::authority::Decision;
use super::client_identity::FsIdentityGuard;
use super::wire::requests::FilesystemOperation as Request;
use crate::caps::{Cap, Scope, Verb};

const MAX_TEXT_BYTES: usize = super::wire::bounded::FILE_TEXT_MAX_BYTES;
static FILE_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

impl Request {
    fn path(&self) -> &str {
        match self {
            Self::Read { path } | Self::Write { path, .. } | Self::Replace { path, .. } => {
                path.as_str()
            }
        }
    }
}

pub async fn access(params: Value, authority: &Decision, mutation: bool) -> Result<Value, String> {
    let request: Request = serde_json::from_value(params["request"].clone())
        .map_err(|error| format!("invalid filesystem request: {error}"))?;
    if matches!(request, Request::Read { .. }) == mutation {
        return Err("filesystem action does not match route".into());
    }
    match &request {
        Request::Write { content, .. } if content.as_str().len() > MAX_TEXT_BYTES => {
            return Err("file content exceeds the text limit".into());
        }
        Request::Replace { find, replace, .. }
            if find.as_str().is_empty()
                || find.as_str().len() > MAX_TEXT_BYTES
                || replace.as_str().len() > MAX_TEXT_BYTES =>
        {
            return Err("replacement requires bounded text and a nonempty find".into());
        }
        _ => {}
    }
    let path = {
        let _identity = FsIdentityGuard::enter(authority.owner_uid())?;
        resolve_path(request.path())?
    };
    crate::worker::derive::reject_forbidden(&path)?;
    let scope = Scope::path(path.to_str().ok_or("file path must be UTF-8")?);
    let caps = match &request {
        Request::Read { .. } => vec![Cap::new(Verb::FS_READ, scope)],
        Request::Write { .. } => vec![Cap::new(Verb::FS_WRITE, scope)],
        Request::Replace { .. } => vec![
            Cap::new(Verb::FS_READ, scope.clone()),
            Cap::new(Verb::FS_WRITE, scope),
        ],
    };
    // Serializes read/modify/replace with other broker file operations. Spend
    // the live grant only after waiting, so expired/cancelled calls cannot write.
    let _guard = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        FILE_LOCK.get_or_init(|| tokio::sync::Mutex::new(())).lock(),
    )
    .await
    .map_err(|_| "filesystem provider is busy")?;
    let _authorized = authority.require_all(&caps)?;
    let target = Target::open(&path, authority.owner_uid())?;
    match request {
        Request::Read { .. } => {
            let (bytes, _) = target.read()?.ok_or("file does not exist")?;
            let content = String::from_utf8(bytes).map_err(|_| "file is not valid UTF-8")?;
            Ok(json!({"path": path, "content": content}))
        }
        Request::Write { content, .. } => {
            let previous = target.read()?;
            let sid = mutation_session(authority)?;
            target.write(
                content.as_str().as_bytes(),
                previous,
                sid.as_ref(),
                authority.owner_uid(),
                false,
            )?;
            Ok(json!({"path": path, "bytes": content.as_str().len()}))
        }
        Request::Replace { find, replace, .. } => {
            let previous = target.read()?.ok_or("file does not exist")?;
            let body = std::str::from_utf8(&previous.0).map_err(|_| "file is not valid UTF-8")?;
            let content = replace_unique(body, find.as_str(), replace.as_str())?;
            let sid = mutation_session(authority)?;
            target.write(
                content.as_bytes(),
                Some(previous),
                sid.as_ref(),
                authority.owner_uid(),
                false,
            )?;
            Ok(json!({"replacements": 1}))
        }
    }
}

fn resolve_path(raw: &str) -> Result<PathBuf, String> {
    let path = Path::new(raw);
    if raw.is_empty() || raw.contains('\0') || !path.is_absolute() {
        return Err("filesystem path must be a nonempty absolute path without NUL".into());
    }
    match fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let name = path.file_name().ok_or("file path has no name")?;
            let parent = path.parent().ok_or("file path has no parent")?;
            Ok(fs::canonicalize(parent)
                .map_err(|e| e.to_string())?
                .join(name))
        }
        Err(error) => Err(error.to_string()),
    }
}

fn replace_unique(body: &str, find: &str, replace: &str) -> Result<String, String> {
    if find.is_empty() {
        return Err("find must be nonempty and occur exactly once; widen the context".into());
    }
    let first = body.find(find).ok_or("find is not present in the file")?;
    let next = first
        + body[first..]
            .chars()
            .next()
            .expect("nonempty match")
            .len_utf8();
    if body[next..].contains(find) {
        return Err("find occurs more than once; widen the context".into());
    }
    let size = body.len() - find.len() + replace.len();
    if size > MAX_TEXT_BYTES {
        return Err("replacement exceeds the text limit".into());
    }
    Ok(body.replacen(find, replace, 1))
}

// The task id comes from the broker-issued grant, never request metadata or
// the worker environment. Non-task GUI/CLI calls still get the broker journal.
fn mutation_session(authority: &Decision) -> Result<Option<crate::session::SessionId>, String> {
    let Some(task) = authority.task_id() else {
        return Ok(None);
    };
    let store = crate::agent::service::Store::open_default().map_err(|e| e.to_string())?;
    let (_, job) = store
        .locate_for_owner(task, Some(authority.owner_uid()))
        .map_err(|e| e.to_string())?
        .ok_or("filesystem task is unavailable")?;
    let sid = job
        .session_id
        .ok_or("filesystem task has no durable session")?
        .parse::<crate::session::SessionId>()
        .map_err(|e| e.to_string())?;
    let meta = crate::session::get_meta(&sid).map_err(|e| e.to_string())?;
    if meta.owner_uid != Some(authority.owner_uid()) {
        return Err("filesystem task session owner mismatch".into());
    }
    Ok(Some(sid))
}

pub(super) struct Target {
    path: PathBuf,
    directory: File,
    pinned: PathBuf,
    owner: u32,
}

impl Target {
    pub(super) fn open(path: &Path, owner: u32) -> Result<Self, String> {
        let _identity = FsIdentityGuard::enter(owner)?;
        let parent = path.parent().ok_or("file path has no parent")?;
        let mut directory = File::open("/").map_err(|e| e.to_string())?;
        // Walk canonical components through pinned directory descriptors: an
        // ancestor swapped for a symlink cannot redirect privileged IO.
        for component in parent.components() {
            if let Component::Normal(name) = component {
                directory = open_at(
                    &directory,
                    name,
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
                    0,
                )
                .map_err(|e| e.to_string())?;
            }
        }
        let pinned = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()))
            .join(path.file_name().ok_or("file path has no name")?);
        Ok(Self {
            path: path.into(),
            directory,
            pinned,
            owner,
        })
    }

    fn read(&self) -> Result<Option<(Vec<u8>, fs::Metadata)>, String> {
        let _identity = FsIdentityGuard::enter(self.owner)?;
        let file = match open_at(
            &self.directory,
            self.path.file_name().ok_or("file has no name")?,
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
        ) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file() {
            return Err("target is not a regular file".into());
        }
        let mut bytes = Vec::new();
        file.take((MAX_TEXT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > MAX_TEXT_BYTES {
            return Err("file exceeds the text limit; no partial content returned".into());
        }
        Ok(Some((bytes, metadata)))
    }

    fn write(
        &self,
        data: &[u8],
        previous: Option<(Vec<u8>, fs::Metadata)>,
        sid: Option<&crate::session::SessionId>,
        owner: u32,
        create_only: bool,
    ) -> Result<(), String> {
        let parent = self.pinned.parent().ok_or("file path has no parent")?;
        let parent_meta = self.directory.metadata().map_err(|e| e.to_string())?;
        if parent_meta.mode() & libc::S_ISVTX != 0
            && owner != 0
            && owner != parent_meta.uid()
            && previous
                .as_ref()
                .is_some_and(|(_, meta)| meta.uid() != owner)
        {
            return Err("cannot replace another owner's file in a sticky directory".into());
        }
        // Creation under owner credentials proves directory write access
        // without granting the worker a writable parent mount.
        let stage_name = format!(".cos-file-{}", uuid::Uuid::new_v4().simple());
        let file = {
            let _identity = FsIdentityGuard::enter(self.owner)?;
            open_at(
                &self.directory,
                std::ffi::OsStr::new(&stage_name),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
                0o600,
            )
            .map_err(|e| e.to_string())?
        };
        let mut staged = tempfile::NamedTempFile::from_parts(
            file,
            tempfile::TempPath::try_from_path(parent.join(stage_name))
                .map_err(|e| e.to_string())?,
        );
        staged.write_all(data).map_err(|e| e.to_string())?;
        let staged_gid = staged
            .as_file()
            .metadata()
            .map_err(|e| e.to_string())?
            .gid();
        let (gid, mode) = match previous.as_ref() {
            Some((_, meta)) => (
                if meta.uid() == owner {
                    meta.gid()
                } else {
                    staged_gid
                },
                meta.mode() & 0o777,
            ),
            None => (staged_gid, 0o600),
        };
        // Atomic replacement never manufactures another user's ownership.
        if unsafe { libc::fchown(staged.as_file().as_raw_fd(), owner, gid) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        staged
            .as_file()
            .set_permissions(fs::Permissions::from_mode(mode))
            .map_err(|e| e.to_string())?;
        staged.as_file().sync_all().map_err(|e| e.to_string())?;
        let current = self.read()?;
        let unchanged = match (&previous, &current) {
            (None, None) => true,
            (Some((a, am)), Some((b, bm))) => {
                a == b && am.dev() == bm.dev() && am.ino() == bm.ino()
            }
            _ => false,
        };
        if !unchanged {
            return Err("file changed while preparing the write".into());
        }
        if let Some(sid) = sid {
            crate::session::record_fs_write_bytes(
                sid,
                &self.path,
                previous.as_ref().map(|(bytes, _)| bytes.as_slice()),
            )
            .map_err(|e| e.to_string())?;
        }
        if create_only {
            staged.persist_noclobber(&self.pinned).map_err(|e| e.to_string())?;
        } else {
            staged.persist(&self.pinned).map_err(|e| e.to_string())?;
        }
        self.directory.sync_all().map_err(|e| e.to_string())?;
        Ok(())
    }

    pub(super) fn write_new(&self, bytes: &[u8], authority: &Decision) -> Result<(), String> {
        let _identity = FsIdentityGuard::enter(self.owner)?;
        let parent = self.path.parent().ok_or("file has no parent")?;
        let current = fs::metadata(parent).map_err(|e| e.to_string())?;
        let pinned = self.directory.metadata().map_err(|e| e.to_string())?;
        if current.dev() != pinned.dev() || current.ino() != pinned.ino() {
            return Err("capture directory changed during the request".into());
        }
        drop(_identity);
        let sid = mutation_session(authority)?;
        self.write(bytes, None, sid.as_ref(), self.owner, true)
    }
}

fn open_at(
    directory: &File,
    name: &std::ffi::OsStr,
    flags: i32,
    mode: libc::mode_t,
) -> std::io::Result<File> {
    let name = std::ffi::CString::new(name.as_bytes()).map_err(std::io::Error::other)?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC,
            mode,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/filesystem.rs"
    ));
}
