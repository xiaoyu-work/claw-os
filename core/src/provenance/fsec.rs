//! Filesystem security primitives shared by the trust store and the
//! package verifier.
//!
//! Two properties matter here:
//!
//!   * **Ownership/mode gating** — a trust root is only a trust root
//!     when every path component from `/` down is a directory owned by
//!     an approved uid, is not a symlink, and is not group- or
//!     world-writable. Otherwise a non-root user could swap a
//!     directory under our feet and inject keys.
//!   * **TOCTOU-resistant reads** — once a package has been verified
//!     we never re-resolve its path by name. We keep the directory
//!     descriptor and walk it with `openat(…, O_NOFOLLOW)` so the
//!     bytes we hash are the bytes we later execute or disclose.
//!
//! Non-Unix builds fail closed: they cannot express these guarantees,
//! so every helper returns an error instead of a weaker approximation.

use std::io;
use std::path::Path;

#[cfg(unix)]
pub use unix::*;

/// Metadata subset the security checks care about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeMeta {
    pub uid: u32,
    pub gid: u32,
    /// Permission bits (`0o7777`).
    pub mode: u32,
    pub is_dir: bool,
    pub is_file: bool,
    pub is_symlink: bool,
    /// `S_IFSOCK`. A transport, never data: the worker sandbox binds
    /// one only under a kernel-side classification.
    pub is_socket: bool,
    pub nlink: u64,
    pub size: u64,
    pub dev: u64,
    pub ino: u64,
}

impl NodeMeta {
    /// Group- or world-writable. The sticky bit does not rescue a
    /// directory here: trust roots are never shared scratch space.
    pub fn is_group_or_world_writable(&self) -> bool {
        self.mode & 0o022 != 0
    }
}

/// Why a path was rejected as a security-sensitive location.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PathTrustError {
    #[error("{path}: {reason}")]
    Rejected { path: String, reason: String },
    #[error("{path}: cannot inspect: {reason}")]
    Unreadable { path: String, reason: String },
    #[error("secure path checks require a Unix host")]
    Unsupported,
}

fn rejected(path: &Path, reason: impl Into<String>) -> PathTrustError {
    PathTrustError::Rejected {
        path: path.display().to_string(),
        reason: reason.into(),
    }
}

/// Assert that `path` and every ancestor is a non-symlink directory
/// owned by one of `allowed_uids` and not group/world-writable.
///
/// `path` itself may be a directory or a regular file; ancestors must
/// all be directories.
pub fn require_secure_location(
    path: &Path,
    allowed_uids: &[u32],
) -> Result<NodeMeta, PathTrustError> {
    if !path.is_absolute() {
        return Err(rejected(path, "path must be absolute"));
    }
    let meta = lstat(path).map_err(|e| PathTrustError::Unreadable {
        path: path.display().to_string(),
        reason: e.to_string(),
    })?;
    if meta.is_symlink {
        return Err(rejected(path, "is a symlink"));
    }
    if !meta.is_dir && !meta.is_file {
        return Err(rejected(path, "is not a regular file or directory"));
    }
    if !allowed_uids.contains(&meta.uid) {
        return Err(rejected(
            path,
            format!(
                "owner uid {} is not in the approved set {allowed_uids:?}",
                meta.uid
            ),
        ));
    }
    if meta.is_group_or_world_writable() {
        return Err(rejected(
            path,
            format!("mode {:o} is group- or world-writable", meta.mode),
        ));
    }
    let mut cursor = path.parent();
    while let Some(dir) = cursor {
        let dir_meta = lstat(dir).map_err(|e| PathTrustError::Unreadable {
            path: dir.display().to_string(),
            reason: e.to_string(),
        })?;
        if dir_meta.is_symlink {
            return Err(rejected(dir, "ancestor is a symlink"));
        }
        if !dir_meta.is_dir {
            return Err(rejected(dir, "ancestor is not a directory"));
        }
        // An ancestor may be owned by root even when the leaf belongs to
        // an unprivileged owner: `/home/<user>` sits under root-owned
        // `/home`. Anyone else owning an ancestor could swap the subtree.
        if dir_meta.uid != 0 && !allowed_uids.contains(&dir_meta.uid) {
            return Err(rejected(
                dir,
                format!(
                    "ancestor owner uid {} is neither root nor in the approved set {allowed_uids:?}",
                    dir_meta.uid
                ),
            ));
        }
        // A shared directory is acceptable as an ancestor only when the
        // sticky bit is set: without it any user could rename our
        // subtree out from under us, with it only the owner can.
        let sticky = dir_meta.mode & 0o1000 != 0;
        if dir_meta.is_group_or_world_writable() && !sticky {
            return Err(rejected(
                dir,
                format!(
                    "ancestor mode {:o} is group- or world-writable",
                    dir_meta.mode
                ),
            ));
        }
        cursor = dir.parent();
    }
    Ok(meta)
}

#[cfg(not(unix))]
pub fn lstat(_path: &Path) -> io::Result<NodeMeta> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "provenance path checks require a Unix host",
    ))
}

#[cfg(not(unix))]
pub mod stub {
    //! Non-Unix builds have no `openat`; every verified-read helper
    //! fails closed so no code path silently degrades to a
    //! path-reopen.
    use super::*;

    #[derive(Debug)]
    pub struct DirHandle;

    impl DirHandle {
        pub fn open(_path: &Path) -> io::Result<Self> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "provenance verified reads require a Unix host",
            ))
        }
    }
}

#[cfg(not(unix))]
pub use stub::DirHandle;

#[cfg(unix)]
mod unix {
    use super::{NodeMeta, PathTrustError};
    use std::ffi::CString;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};

    fn cstr(path: &Path) -> io::Result<CString> {
        CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))
    }

    fn meta_from_stat(st: &libc::stat) -> NodeMeta {
        let fmt = st.st_mode & libc::S_IFMT;
        NodeMeta {
            uid: st.st_uid,
            gid: st.st_gid,
            mode: st.st_mode & 0o7777,
            is_dir: fmt == libc::S_IFDIR,
            is_file: fmt == libc::S_IFREG,
            is_symlink: fmt == libc::S_IFLNK,
            is_socket: fmt == libc::S_IFSOCK,
            nlink: st.st_nlink,
            size: st.st_size.max(0) as u64,
            dev: st.st_dev,
            ino: st.st_ino,
        }
    }

    /// `lstat(2)` — never follows a final symlink.
    pub fn lstat(path: &Path) -> io::Result<NodeMeta> {
        let c = cstr(path)?;
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::lstat(c.as_ptr(), &mut st) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(meta_from_stat(&st))
    }

    fn fstat(fd: i32) -> io::Result<NodeMeta> {
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::fstat(fd, &mut st) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(meta_from_stat(&st))
    }

    /// A pinned directory. Once opened, every child lookup goes
    /// through `openat` on this descriptor, so renaming or replacing
    /// the original path cannot redirect our reads.
    #[derive(Debug)]
    pub struct DirHandle {
        fd: OwnedFd,
        display: PathBuf,
    }

    impl DirHandle {
        pub fn open(path: &Path) -> io::Result<Self> {
            let c = cstr(path)?;
            let fd = unsafe {
                libc::open(
                    c.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                fd: unsafe { OwnedFd::from_raw_fd(fd) },
                display: path.to_path_buf(),
            })
        }

        pub fn path(&self) -> &Path {
            &self.display
        }

        pub fn as_raw_fd(&self) -> i32 {
            self.fd.as_raw_fd()
        }

        fn openat_dir(&self, name: &str) -> io::Result<OwnedFd> {
            openat_dir_raw(self.fd.as_raw_fd(), name)
        }

        /// Resolve `rel` (slash-separated, already validated) without
        /// traversing a single symlink, and return an open read-only
        /// descriptor on the final regular file.
        pub fn open_file(&self, rel: &str) -> io::Result<VerifiedFd> {
            let mut segments = rel.split('/').collect::<Vec<_>>();
            let last = segments
                .pop()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty path"))?;
            let mut cursor: Option<OwnedFd> = None;
            for segment in segments {
                let parent = cursor
                    .as_ref()
                    .map(|f| f.as_raw_fd())
                    .unwrap_or_else(|| self.fd.as_raw_fd());
                cursor = Some(openat_dir_raw(parent, segment)?);
            }
            let parent = cursor
                .as_ref()
                .map(|f| f.as_raw_fd())
                .unwrap_or_else(|| self.fd.as_raw_fd());
            let c = CString::new(last)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in path"))?;
            let fd = unsafe {
                libc::openat(
                    parent,
                    c.as_ptr(),
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let owned = unsafe { OwnedFd::from_raw_fd(fd) };
            let meta = fstat(owned.as_raw_fd())?;
            if !meta.is_file {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("`{rel}` is not a regular file"),
                ));
            }
            Ok(VerifiedFd {
                fd: owned,
                meta,
                rel: rel.to_string(),
            })
        }

        /// Sorted directory listing that reports the raw node type of
        /// each child without following symlinks.
        pub fn entries(&self, rel: Option<&str>) -> io::Result<Vec<(String, NodeMeta)>> {
            let dir_fd = match rel {
                None => dup_fd(self.fd.as_raw_fd())?,
                Some(path) => {
                    let mut cursor: Option<OwnedFd> = None;
                    for segment in path.split('/') {
                        let parent = cursor
                            .as_ref()
                            .map(|f| f.as_raw_fd())
                            .unwrap_or_else(|| self.fd.as_raw_fd());
                        cursor = Some(openat_dir_raw(parent, segment)?);
                    }
                    cursor.expect("non-empty relative path yields a descriptor")
                }
            };
            let raw = dir_fd.as_raw_fd();
            let dup = dup_fd(raw)?;
            let dirp = unsafe { libc::fdopendir(dup.as_raw_fd()) };
            if dirp.is_null() {
                return Err(io::Error::last_os_error());
            }
            // `closedir` takes ownership of the descriptor.
            std::mem::forget(dup);
            let mut out = Vec::new();
            loop {
                // `readdir` returns NULL both at end-of-stream and on
                // error; distinguish via errno.
                unsafe { *libc::__errno_location() = 0 };
                let entry = unsafe { libc::readdir(dirp) };
                if entry.is_null() {
                    let err = io::Error::last_os_error();
                    unsafe { libc::closedir(dirp) };
                    if err.raw_os_error().unwrap_or(0) != 0 {
                        return Err(err);
                    }
                    break;
                }
                let name_ptr = unsafe { (*entry).d_name.as_ptr() };
                let name = unsafe { std::ffi::CStr::from_ptr(name_ptr) }
                    .to_string_lossy()
                    .into_owned();
                if name == "." || name == ".." {
                    continue;
                }
                let c = match CString::new(name.clone()) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                let mut st: libc::stat = unsafe { std::mem::zeroed() };
                let rc =
                    unsafe { libc::fstatat(raw, c.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) };
                if rc != 0 {
                    let err = io::Error::last_os_error();
                    unsafe { libc::closedir(dirp) };
                    return Err(err);
                }
                out.push((name, meta_from_stat(&st)));
            }
            out.sort_by(|a, b| a.0.cmp(&b.0));
            Ok(out)
        }
    }

    fn dup_fd(fd: i32) -> io::Result<OwnedFd> {
        let new = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
        if new < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { OwnedFd::from_raw_fd(new) })
    }

    fn openat_dir_raw(parent: i32, name: &str) -> io::Result<OwnedFd> {
        let c = CString::new(name)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in path"))?;
        let fd = unsafe {
            libc::openat(
                parent,
                c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// An open descriptor on a file inside a pinned package tree.
    ///
    /// The descriptor — not the path — is the identity of the file.
    /// `proc_path` yields `/proc/self/fd/N`, which the worker sandbox
    /// already accepts as a pinned mount source, so an executable can
    /// be launched from exactly the inode we hashed.
    #[derive(Debug)]
    pub struct VerifiedFd {
        fd: OwnedFd,
        meta: NodeMeta,
        rel: String,
    }

    impl VerifiedFd {
        pub fn meta(&self) -> NodeMeta {
            self.meta
        }

        pub fn rel(&self) -> &str {
            &self.rel
        }

        pub fn as_raw_fd(&self) -> i32 {
            self.fd.as_raw_fd()
        }

        pub fn proc_path(&self) -> PathBuf {
            PathBuf::from(format!("/proc/self/fd/{}", self.fd.as_raw_fd()))
        }

        /// Read at most `cap` bytes. Returns an error when the file is
        /// larger, so an attacker cannot turn a verified read into an
        /// unbounded allocation by growing the file after `fstat`.
        ///
        /// Reads are positional (`pread`) so repeated calls on the same
        /// descriptor always see the file from byte zero, regardless of
        /// any shared file offset.
        pub fn read_bounded(&self, cap: u64) -> io::Result<Vec<u8>> {
            use std::os::unix::fs::FileExt;
            let dup = dup_fd(self.fd.as_raw_fd())?;
            let file = std::fs::File::from(dup);
            let mut buf = vec![0u8; (cap + 1).min(1 << 22) as usize];
            let mut out: Vec<u8> = Vec::new();
            let mut offset = 0u64;
            loop {
                let read = file.read_at(&mut buf, offset)?;
                if read == 0 {
                    break;
                }
                offset += read as u64;
                out.extend_from_slice(&buf[..read]);
                if out.len() as u64 > cap {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("`{}` exceeds the {cap}-byte read cap", self.rel),
                    ));
                }
            }
            Ok(out)
        }
    }

    /// `require_secure_location` for an already-open descriptor.
    pub fn require_secure_fd(fd: i32, allowed_uids: &[u32]) -> Result<NodeMeta, PathTrustError> {
        let meta = fstat(fd).map_err(|e| PathTrustError::Unreadable {
            path: format!("fd:{fd}"),
            reason: e.to_string(),
        })?;
        if !allowed_uids.contains(&meta.uid) {
            return Err(PathTrustError::Rejected {
                path: format!("fd:{fd}"),
                reason: format!("owner uid {} is not approved", meta.uid),
            });
        }
        if meta.is_group_or_world_writable() {
            return Err(PathTrustError::Rejected {
                path: format!("fd:{fd}"),
                reason: format!("mode {:o} is group- or world-writable", meta.mode),
            });
        }
        Ok(meta)
    }

    pub fn effective_uid() -> u32 {
        unsafe { libc::geteuid() }
    }
}

#[cfg(not(unix))]
pub fn effective_uid() -> u32 {
    u32::MAX
}

/// Best-effort `fsync` on a directory so a rename is durable.
pub fn sync_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let handle = DirHandle::open(path)?;
        let rc = unsafe { libc::fsync(handle.as_raw_fd()) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/provenance/fsec.rs"
    ));
}
