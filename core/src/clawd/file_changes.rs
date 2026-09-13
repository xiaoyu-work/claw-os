//! Bounded, capability-gated file replacement, without lending a worker
//! authority over the target's parent directory.
//!
//! The parent is pinned and every target/staging operation is descriptor
//! relative. The final check and rename are not a filesystem compare-and-swap:
//! an uncooperative host writer can still race that narrow interval.
//! There is no rollback here; App plans retain their own baseline/snapshot.

use base64::Engine;
use serde_json::Value;

use super::authority::Decision;
use super::protocol::BrokerError;
use super::wire::requests::FileReplace;

pub const MAX_FILE_BYTES: usize = 65_536;

pub async fn replace(params: Value, authority: &Decision) -> Result<Value, BrokerError> {
    let request: FileReplace = serde_json::from_value(params).map_err(|_| {
        refusal(
            "file_replace_invalid",
            "invalid file replacement parameters",
        )
    })?;
    validate_path(request.path.as_str())?;
    let content = base64::engine::general_purpose::STANDARD
        .decode(request.content_base64.as_str())
        .map_err(|_| refusal("file_replace_invalid", "invalid replacement base64"))?;
    if content.len() > MAX_FILE_BYTES {
        return Err(refusal(
            "file_replace_invalid",
            "replacement exceeds 64 KiB",
        ));
    }
    if authority.session_id() != Some(request.session.as_str()) {
        return Err(BrokerError::authorization(
            "replacement session does not match its grant",
        ));
    }
    #[cfg(target_os = "linux")]
    {
        linux::run(request, content, authority).await
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (request, content, authority);
        Err(BrokerError::unavailable(
            "atomic file replacement requires Linux",
        ))
    }
}

pub(crate) fn validate_path(path: &str) -> Result<(), BrokerError> {
    if !path.starts_with('/')
        || path.len() > 4096
        || path == "/"
        || path
            .chars()
            .any(|c| c.is_control() || "*?[]{}<>|;&$`\\".contains(c))
        || path
            .split('/')
            .skip(1)
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(refusal(
            "file_replace_invalid",
            "replacement target must be an absolute literal file path without traversal, globbing or redirection",
        ));
    }
    Ok(())
}

fn refusal(class: &'static str, message: impl Into<String>) -> BrokerError {
    BrokerError::execution(message).classified(class)
}

#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::CString;
    use std::fs::{File, Metadata, OpenOptions};
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::os::fd::{AsRawFd, FromRawFd, RawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    use std::path::{Component, Path, PathBuf};

    use serde_json::json;
    use sha2::{Digest, Sha256};

    use super::*;
    use crate::caps::{Cap, Scope, Verb};
    use crate::clawd::authority::Authorized;
    use crate::clawd::client_identity::ClientIdentity;
    use crate::clawd::protocol::{BrokerErrorKind, Response};
    use crate::clawd::routes::Command;
    use crate::clawd::wire::requests::FileState;
    use crate::clawd::wire::RequestId;

    static REPLACEMENTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    pub(super) async fn run(
        request: FileReplace,
        content: Vec<u8>,
        authority: &Decision,
    ) -> Result<Value, BrokerError> {
        // Cooperating broker writers are serialized; external writers are not.
        let _lock = REPLACEMENTS.lock().await;
        let owner = Owner::resolve(authority.owner_uid())?;
        let target = Target::pin(request.path.as_str(), &owner.home)?;
        let authorized = authority
            .require_all(&requirements(&target.path))
            .map_err(BrokerError::authorization)?;
        target.verify_parent()?;

        // Keep a proposal-level identity on both entry paths, independent of
        // the launcher's outer relay correlation id. The key remains stable
        // for the same session, baseline and proposal, so a retry cannot
        // evade an unresolved effect by choosing a fresh transport id.
        let identity = serde_json::to_vec(&json!([
            authority.session_id(),
            target.path,
            request.expected,
            sha256(&content)
        ]))
        .map_err(|_| BrokerError::unavailable("cannot encode replacement identity"))?;
        let id = RequestId::parse(&hex::encode(Sha256::digest(identity)))
            .map_err(BrokerError::unavailable)?;
        let guard = crate::clawd::journal::begin(
            Command::SystemFileReplace.route(),
            &id,
            Some(authority),
            &ClientIdentity::unknown(),
        )
        .map_err(BrokerError::fault)?
        .ok_or_else(|| BrokerError::unavailable("replacement route is not journal-bracketed"))?;

        let result = target.apply(&authorized, request.expected.as_ref(), &content, &owner);
        let response = match &result {
            Ok(value) => Response::ok(id.clone(), value.clone()),
            Err(error) => Response::handler_error(id.clone(), error.clone()),
        };
        if let Some(unresolved) = crate::clawd::journal::finish(guard, &id, &response) {
            return Err(indeterminate(
                unresolved
                    .error
                    .map(|error| error.message)
                    .unwrap_or_else(|| "replacement outcome is unresolved".to_string()),
            ));
        }
        result
    }

    fn requirements(path: &Path) -> [Cap; 2] {
        let scope = Scope::path(path.to_string_lossy().into_owned());
        [
            Cap::new(Verb::FS_READ, scope.clone()),
            Cap::new(Verb::FS_WRITE, scope),
        ]
    }

    struct Owner {
        uid: u32,
        gid: u32,
        home: PathBuf,
    }

    impl Owner {
        fn resolve(uid: u32) -> Result<Self, BrokerError> {
            let home =
                crate::paths::verified_home_for_uid(uid).map_err(BrokerError::authorization)?;
            let mut buffer = vec![0 as libc::c_char; 16 * 1024];
            let mut passwd: libc::passwd = unsafe { std::mem::zeroed() };
            let mut found = std::ptr::null_mut();
            let code = unsafe {
                libc::getpwuid_r(
                    uid,
                    &mut passwd,
                    buffer.as_mut_ptr(),
                    buffer.len(),
                    &mut found,
                )
            };
            if code != 0 || found.is_null() || passwd.pw_uid != uid {
                return Err(BrokerError::authorization(
                    "replacement owner account is unavailable",
                ));
            }
            Ok(Self {
                uid,
                gid: passwd.pw_gid,
                home,
            })
        }
    }

    struct Target {
        path: PathBuf,
        requested_path: String,
        parent_path: PathBuf,
        parent: File,
        name: CString,
    }

    impl Target {
        fn pin(raw: &str, owner_home: &Path) -> Result<Self, BrokerError> {
            validate_path(raw)?;
            let path = Path::new(raw);
            crate::worker::derive::reject_forbidden_for_owner(path, owner_home)
                .map_err(BrokerError::authorization)?;
            let parent_path = path
                .parent()
                .ok_or_else(|| refusal("file_replace_invalid", "replacement has no parent"))?
                .canonicalize()
                .map_err(|error| io_error("resolve replacement parent", error))?;
            let name = path
                .file_name()
                .ok_or_else(|| refusal("file_replace_invalid", "replacement has no file name"))?;
            let canonical = parent_path.join(name);
            validate_path(canonical.to_str().ok_or_else(|| {
                refusal(
                    "file_replace_invalid",
                    "canonical replacement path is not UTF-8",
                )
            })?)?;
            crate::worker::derive::reject_forbidden_for_owner(&canonical, owner_home)
                .map_err(BrokerError::authorization)?;
            let parent = open_directory(&parent_path)?;
            Ok(Self {
                path: canonical,
                requested_path: raw.to_string(),
                parent_path,
                parent,
                name: cstring(name.as_bytes())?,
            })
        }

        fn verify_parent(&self) -> Result<(), BrokerError> {
            let current = open_directory(&self.parent_path)?;
            let held = self
                .parent
                .metadata()
                .map_err(|error| io_error("inspect pinned parent", error))?;
            let now = current
                .metadata()
                .map_err(|error| io_error("inspect current parent", error))?;
            if (held.dev(), held.ino()) != (now.dev(), now.ino()) {
                return Err(conflict());
            }
            Ok(())
        }

        fn open_target(&self) -> Result<Option<File>, BrokerError> {
            match open_at(
                self.parent.as_raw_fd(),
                &self.name,
                libc::O_PATH | libc::O_NOFOLLOW,
                0,
            ) {
                Ok(file) => Ok(Some(file)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(io_error(
                    "open replacement target without following links",
                    error,
                )),
            }
        }

        fn baseline(
            &self,
            _authorized: &Authorized,
            expected: Option<&FileState>,
        ) -> Result<Option<Snapshot>, BrokerError> {
            let held = self.open_target()?;
            match (expected, held) {
                (None, None) => Ok(None),
                (None, Some(_)) | (Some(_), None) => Err(conflict()),
                (Some(expected), Some(held)) => {
                    let metadata = held
                        .metadata()
                        .map_err(|error| io_error("inspect target", error))?;
                    supported(&metadata)?;
                    // Reopen only the already-pinned regular inode, not the
                    // mutable directory entry (which could now name a device).
                    let mut file = OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK)
                        .open(format!("/proc/self/fd/{}", held.as_raw_fd()))
                        .map_err(|error| io_error("read pinned target", error))?;
                    let snapshot = Snapshot::read(&mut file)?;
                    if !same_stat(&metadata, &snapshot.metadata) || &snapshot.state != expected {
                        return Err(conflict());
                    }
                    Ok(Some(snapshot))
                }
            }
        }

        fn recheck(
            &self,
            authorized: &Authorized,
            expected: Option<&FileState>,
            initial: Option<&Snapshot>,
        ) -> Result<(), BrokerError> {
            self.verify_parent()?;
            let current = self.baseline(authorized, expected)?;
            if let (Some(initial), Some(current)) = (initial, current.as_ref()) {
                if !same_stat(&initial.metadata, &current.metadata) {
                    return Err(conflict());
                }
            }
            Ok(())
        }

        fn apply(
            &self,
            authorized: &Authorized,
            expected: Option<&FileState>,
            content: &[u8],
            owner: &Owner,
        ) -> Result<Value, BrokerError> {
            let initial = self.baseline(authorized, expected)?;
            let hash = sha256(content);
            if initial
                .as_ref()
                .is_some_and(|snapshot| snapshot.content == content)
            {
                self.recheck(authorized, expected, initial.as_ref())?;
                return Ok(
                    json!({"path": self.requested_path, "bytes": content.len(), "sha256": hash, "changed": false}),
                );
            }

            let mut stage = Stage::create(authorized, &self.parent)?;
            stage
                .file
                .write_all(content)
                .map_err(|error| io_error("write replacement stage", error))?;
            let (uid, gid, mode) = match initial.as_ref() {
                Some(snapshot) => (
                    snapshot.metadata.uid(),
                    snapshot.metadata.gid(),
                    snapshot.metadata.mode() & 0o777,
                ),
                None => (owner.uid, owner.gid, 0o600),
            };
            stage.preserve(uid, gid, mode)?;
            stage
                .file
                .sync_all()
                .map_err(|error| io_error("sync replacement stage", error))?;
            let staged = stage.verify(content, uid, gid, mode)?;

            #[cfg(test)]
            faults::after_stage();

            self.recheck(authorized, expected, initial.as_ref())?;
            if !same_stat(
                &staged,
                &stage
                    .file
                    .metadata()
                    .map_err(|error| io_error("recheck staged metadata", error))?,
            ) {
                return Err(conflict());
            }
            stage.verify_name()?;

            #[cfg(test)]
            faults::before_commit();

            stage.commit(&self.name, expected.is_some())?;

            #[cfg(test)]
            if faults::fail_parent_sync() {
                return Err(indeterminate("injected parent fsync failure"));
            }

            self.parent
                .sync_all()
                .map_err(|error| indeterminate(format!("sync replacement parent: {error}")))?;
            Ok(
                json!({"path": self.requested_path, "bytes": content.len(), "sha256": hash, "changed": true}),
            )
        }
    }

    struct Snapshot {
        state: FileState,
        metadata: Metadata,
        content: Vec<u8>,
    }

    impl Snapshot {
        fn read(file: &mut File) -> Result<Self, BrokerError> {
            let before = file
                .metadata()
                .map_err(|error| io_error("inspect file", error))?;
            supported(&before)?;
            no_attributes(file)?;
            file.seek(SeekFrom::Start(0))
                .map_err(|error| io_error("seek file", error))?;
            let mut content = Vec::new();
            file.take((MAX_FILE_BYTES + 1) as u64)
                .read_to_end(&mut content)
                .map_err(|error| io_error("read bounded file", error))?;
            let after = file
                .metadata()
                .map_err(|error| io_error("recheck file", error))?;
            if content.len() > MAX_FILE_BYTES
                || content.len() as u64 != after.size()
                || !same_stat(&before, &after)
            {
                return Err(conflict());
            }
            Ok(Self {
                state: FileState {
                    sha256: sha256(&content),
                    size: after.size(),
                    device: after.dev(),
                    inode: after.ino(),
                    mode: after.mode(),
                    modified_ns: timestamp(after.mtime(), after.mtime_nsec())?,
                    changed_ns: timestamp(after.ctime(), after.ctime_nsec())?,
                },
                metadata: after,
                content,
            })
        }
    }

    fn same_stat(left: &Metadata, right: &Metadata) -> bool {
        (
            left.dev(),
            left.ino(),
            left.mode(),
            left.size(),
            left.nlink(),
            left.uid(),
            left.gid(),
            left.mtime(),
            left.mtime_nsec(),
            left.ctime(),
            left.ctime_nsec(),
        ) == (
            right.dev(),
            right.ino(),
            right.mode(),
            right.size(),
            right.nlink(),
            right.uid(),
            right.gid(),
            right.mtime(),
            right.mtime_nsec(),
            right.ctime(),
            right.ctime_nsec(),
        )
    }

    fn timestamp(seconds: i64, nanos: i64) -> Result<i64, BrokerError> {
        i64::try_from(i128::from(seconds) * 1_000_000_000 + i128::from(nanos))
            .map_err(|_| unsupported("file timestamp cannot be represented in nanoseconds"))
    }

    fn supported(metadata: &Metadata) -> Result<(), BrokerError> {
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(unsupported(
                "replacement requires a regular single-link file",
            ));
        }
        if metadata.mode() & 0o7000 != 0 {
            return Err(unsupported(
                "setuid, setgid and sticky modes are unsupported",
            ));
        }
        if metadata.size() > MAX_FILE_BYTES as u64 {
            return Err(unsupported("replacement target exceeds 64 KiB"));
        }
        Ok(())
    }

    fn no_attributes(file: &File) -> Result<(), BrokerError> {
        let size = unsafe { libc::flistxattr(file.as_raw_fd(), std::ptr::null_mut(), 0) };
        if size > 0 {
            return Err(unsupported(
                "extended attributes cannot be preserved by file replacement",
            ));
        }
        if size < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EOPNOTSUPP) {
                return Err(unsupported("filesystem cannot report extended attributes"));
            }
            return Err(io_error("inspect file extended attributes", error));
        }
        Ok(())
    }

    struct Stage<'a> {
        parent: &'a File,
        name: CString,
        file: File,
        removed: bool,
    }

    impl<'a> Stage<'a> {
        fn create(_authorized: &Authorized, parent: &'a File) -> Result<Self, BrokerError> {
            let name =
                cstring(format!(".cos-replace-{}", uuid::Uuid::new_v4().simple()).as_bytes())?;
            let file = open_at(
                parent.as_raw_fd(),
                &name,
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
                0o600,
            )
            .map_err(|error| io_error("exclusively create replacement stage", error))?;
            Ok(Self {
                parent,
                name,
                file,
                removed: false,
            })
        }

        fn preserve(&self, uid: u32, gid: u32, mode: u32) -> Result<(), BrokerError> {
            let current = self
                .file
                .metadata()
                .map_err(|error| io_error("inspect stage owner", error))?;
            if (current.uid(), current.gid()) != (uid, gid)
                && unsafe { libc::fchown(self.file.as_raw_fd(), uid, gid) } != 0
            {
                return Err(io_error(
                    "preserve file owner",
                    std::io::Error::last_os_error(),
                ));
            }
            if unsafe { libc::fchmod(self.file.as_raw_fd(), mode) } != 0 {
                return Err(io_error(
                    "preserve file mode",
                    std::io::Error::last_os_error(),
                ));
            }
            no_attributes(&self.file)
        }

        fn verify(
            &mut self,
            content: &[u8],
            uid: u32,
            gid: u32,
            mode: u32,
        ) -> Result<Metadata, BrokerError> {
            let snapshot = Snapshot::read(&mut self.file)?;
            if snapshot.content != content
                || (
                    snapshot.metadata.uid(),
                    snapshot.metadata.gid(),
                    snapshot.metadata.mode() & 0o777,
                ) != (uid, gid, mode)
            {
                return Err(conflict());
            }
            self.verify_name()?;
            Ok(snapshot.metadata)
        }

        fn verify_name(&self) -> Result<(), BrokerError> {
            let current = open_at(
                self.parent.as_raw_fd(),
                &self.name,
                libc::O_PATH | libc::O_NOFOLLOW,
                0,
            )
            .map_err(|error| io_error("recheck replacement stage entry", error))?;
            let held = self
                .file
                .metadata()
                .map_err(|error| io_error("inspect stage", error))?;
            let now = current
                .metadata()
                .map_err(|error| io_error("inspect stage entry", error))?;
            if !same_stat(&held, &now) {
                return Err(conflict());
            }
            Ok(())
        }

        fn commit(&mut self, target: &CString, existing: bool) -> Result<(), BrokerError> {
            let dir = self.parent.as_raw_fd();
            let result = if existing {
                unsafe { libc::renameat(dir, self.name.as_ptr(), dir, target.as_ptr()) }
            } else {
                // linkat is atomic and refuses any intervening create. Never
                // use ordinary rename for an absent-target precondition.
                unsafe { libc::linkat(dir, self.name.as_ptr(), dir, target.as_ptr(), 0) }
            };
            if result != 0 {
                let error = std::io::Error::last_os_error();
                if !existing && error.raw_os_error() == Some(libc::EEXIST) {
                    return Err(conflict());
                }
                return Err(indeterminate(format!(
                    "file replacement commit returned: {error}"
                )));
            }
            if existing {
                self.removed = true;
            } else {
                self.cleanup().map_err(|error| {
                    indeterminate(format!("remove linked replacement stage: {error}"))
                })?;
            }
            Ok(())
        }

        fn cleanup(&mut self) -> Result<(), BrokerError> {
            if self.removed {
                return Ok(());
            }
            self.verify_name()?;
            if unsafe { libc::unlinkat(self.parent.as_raw_fd(), self.name.as_ptr(), 0) } != 0 {
                return Err(io_error(
                    "remove own replacement stage",
                    std::io::Error::last_os_error(),
                ));
            }
            self.removed = true;
            Ok(())
        }
    }

    impl Drop for Stage<'_> {
        fn drop(&mut self) {
            if let Err(error) = self.cleanup() {
                tracing::warn!(
                    class = error.audit_class,
                    "replacement stage cleanup failed"
                );
            }
        }
    }

    fn open_directory(path: &Path) -> Result<File, BrokerError> {
        let mut directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open("/")
            .map_err(|error| io_error("pin filesystem root", error))?;
        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => {
                    directory = open_at(
                        directory.as_raw_fd(),
                        &cstring(name.as_bytes())?,
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
                        0,
                    )
                    .map_err(|error| io_error("pin canonical replacement parent", error))?;
                }
                _ => {
                    return Err(refusal(
                        "file_replace_invalid",
                        "noncanonical replacement parent",
                    ))
                }
            }
        }
        Ok(directory)
    }

    fn open_at(dir: RawFd, name: &CString, flags: i32, mode: u32) -> std::io::Result<File> {
        let fd = unsafe { libc::openat(dir, name.as_ptr(), flags | libc::O_CLOEXEC, mode) };
        if fd < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(unsafe { File::from_raw_fd(fd) })
        }
    }

    fn cstring(bytes: &[u8]) -> Result<CString, BrokerError> {
        CString::new(bytes).map_err(|_| refusal("file_replace_invalid", "path contains a NUL byte"))
    }

    fn sha256(content: &[u8]) -> String {
        format!("sha256:{}", hex::encode(Sha256::digest(content)))
    }

    fn io_error(operation: &str, error: std::io::Error) -> BrokerError {
        refusal("file_replace_io", format!("{operation}: {error}"))
    }

    fn unsupported(message: &str) -> BrokerError {
        refusal("file_replace_unsupported", message)
    }

    fn conflict() -> BrokerError {
        refusal(
            "file_replace_conflict",
            "file replacement precondition no longer matches",
        )
    }

    fn indeterminate(message: impl Into<String>) -> BrokerError {
        BrokerError {
            kind: BrokerErrorKind::Indeterminate,
            message: message.into(),
            data: None,
            audit_class: Some("file_replace_indeterminate"),
        }
    }

    #[cfg(test)]
    mod faults {
        use std::cell::RefCell;

        #[derive(Default)]
        struct Hooks {
            after_stage: Option<Box<dyn FnOnce()>>,
            before_commit: Option<Box<dyn FnOnce()>>,
            fail_parent_sync: bool,
        }

        thread_local! {
            static HOOKS: RefCell<Hooks> = RefCell::new(Hooks::default());
        }

        pub(super) fn on_after_stage(hook: impl FnOnce() + 'static) {
            HOOKS.with(|hooks| hooks.borrow_mut().after_stage = Some(Box::new(hook)));
        }

        pub(super) fn on_before_commit(hook: impl FnOnce() + 'static) {
            HOOKS.with(|hooks| hooks.borrow_mut().before_commit = Some(Box::new(hook)));
        }

        pub(super) fn arm_parent_sync() {
            HOOKS.with(|hooks| hooks.borrow_mut().fail_parent_sync = true);
        }

        pub(super) fn after_stage() {
            let hook = HOOKS.with(|hooks| hooks.borrow_mut().after_stage.take());
            if let Some(hook) = hook {
                hook();
            }
        }

        pub(super) fn before_commit() {
            let hook = HOOKS.with(|hooks| hooks.borrow_mut().before_commit.take());
            if let Some(hook) = hook {
                hook();
            }
        }

        pub(super) fn fail_parent_sync() -> bool {
            HOOKS.with(|hooks| std::mem::take(&mut hooks.borrow_mut().fail_parent_sync))
        }
    }

    #[cfg(test)]
    mod tests {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/test/unit/clawd/file_changes.rs"
        ));
    }
}
