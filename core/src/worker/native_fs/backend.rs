use std::collections::BTreeMap;
use std::ffi::{CStr, OsStr, OsString};
use std::fs::File;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use fuser::{
    FileAttr, FileType, ReplyAttr, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen,
    Request,
};

use super::Shared;
use crate::clawd::client_identity::FsIdentityGuard;

const MAX_NODES: usize = 4096;
const MAX_HANDLES: usize = 128;
const MAX_READ: u32 = 131072;
const TTL: Duration = Duration::ZERO;

struct Node {
    relative: PathBuf,
    pin: File,
}

enum Handle {
    File {
        inode: u64,
        file: File,
    },
    Directory {
        inode: u64,
        entries: Vec<(u64, FileType, OsString)>,
    },
}

pub(super) struct Filesystem {
    shared: Arc<Shared>,
    nodes: BTreeMap<u64, Node>,
    names: BTreeMap<PathBuf, u64>,
    handles: BTreeMap<u64, Handle>,
    next_inode: u64,
    next_handle: u64,
}

fn errno(error: std::io::Error) -> i32 {
    error.raw_os_error().unwrap_or(libc::EIO)
}

fn identity(file: &File) -> Result<(u64, u64), i32> {
    file.metadata()
        .map(|metadata| (metadata.dev(), metadata.ino()))
        .map_err(errno)
}

fn fd_path(file: &File) -> Result<PathBuf, i32> {
    std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).map_err(errno)
}

fn checked_object(shared: &Shared, file: &File) -> Result<(), i32> {
    let actual = fd_path(file)?;
    if !actual.starts_with(&shared.root_path) {
        return Err(libc::EACCES);
    }
    crate::worker::derive::reject_forbidden_for_owner(&actual, &shared.owner_home)
        .map_err(|_| libc::EACCES)?;
    let metadata = file.metadata().map_err(errno)?;
    if metadata.file_type().is_symlink()
        || !(metadata.is_file() || metadata.is_dir())
        || (metadata.is_file() && metadata.nlink() != 1)
    {
        return Err(libc::EACCES);
    }
    Ok(())
}

fn relative_parts(path: &Path) -> Result<Vec<&OsStr>, i32> {
    if path.as_os_str().as_bytes().len() > 4096 {
        return Err(libc::ENAMETOOLONG);
    }
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(name) if !name.as_bytes().contains(&0) => parts.push(name),
            _ => return Err(libc::EINVAL),
        }
    }
    if parts.len() > 64 {
        return Err(libc::ENAMETOOLONG);
    }
    Ok(parts)
}

/// Every component is opened relative to retained descriptors under owner
/// credentials. No caller path is continued by the kernel after inspection.
pub(super) fn open_relative(shared: &Shared, path: &Path, readable: bool) -> Result<File, i32> {
    walk(shared, &shared.root, path, readable)
}

fn walk(shared: &Shared, base: &File, path: &Path, readable: bool) -> Result<File, i32> {
    shared.live()?;
    let _identity = FsIdentityGuard::enter(shared.owner).map_err(|_| libc::EACCES)?;
    if fd_path(&shared.root)? != shared.root_path {
        return Err(libc::ESTALE);
    }
    checked_object(shared, base)?;
    let parts = relative_parts(path)?;
    let mut file = base.try_clone().map_err(errno)?;
    if parts.is_empty() && readable {
        return reopen(shared, &file);
    }
    for (index, name) in parts.iter().enumerate() {
        let last = index + 1 == parts.len();
        let flags = if last {
            if readable {
                libc::O_RDONLY | libc::O_NONBLOCK
            } else {
                libc::O_PATH
            }
        } else {
            libc::O_PATH | libc::O_DIRECTORY
        };
        file = crate::clawd::filesystem::open_at(&file, name, flags | libc::O_NOFOLLOW, 0)
            .map_err(errno)?;
        checked_object(shared, &file)?;
    }
    shared.live()?;
    Ok(file)
}

fn reopen(shared: &Shared, pin: &File) -> Result<File, i32> {
    shared.live()?;
    let _identity = FsIdentityGuard::enter(shared.owner).map_err(|_| libc::EACCES)?;
    checked_object(shared, pin)?;
    // This is a Root-retained descriptor, never an App-provided proc path.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(format!("/proc/self/fd/{}", pin.as_raw_fd()))
        .map_err(errno)?;
    if identity(pin)? != identity(&file)? {
        return Err(libc::ESTALE);
    }
    checked_object(shared, &file)?;
    Ok(file)
}

impl Filesystem {
    pub(super) fn new(shared: Arc<Shared>) -> Result<Self, String> {
        let root = shared.root.try_clone().map_err(|error| error.to_string())?;
        Ok(Self {
            shared,
            nodes: BTreeMap::from([(
                1,
                Node {
                    relative: PathBuf::new(),
                    pin: root,
                },
            )]),
            names: BTreeMap::from([(PathBuf::new(), 1)]),
            handles: BTreeMap::new(),
            next_inode: 2,
            next_handle: 1,
        })
    }

    fn authorize(&self, req: &Request<'_>, metadata: bool, kernel_handle: bool) -> Result<(), i32> {
        #[cfg(test)]
        if self.shared.fail_worker.swap(false, Ordering::AcqRel) {
            panic!("private native filesystem worker failure");
        }
        self.shared.live()?;
        if metadata && req.uid() == 0 && req.pid() == self.shared.controller.pid() as u32 {
            self.shared
                .controller
                .assert_current()
                .map_err(|_| libc::EACCES)?;
            return Ok(());
        }
        let binding = self.shared.binding.lock().map_err(|_| libc::EIO)?;
        let binding = binding.as_ref().ok_or(libc::EACCES)?;
        // Kernel readahead may have no task credentials. It can only use an
        // existing selected open handle, never perform lookup or acquire one.
        if kernel_handle && req.pid() == 0 {
            return binding
                .process
                .active()
                .map_err(|_| libc::EACCES)?
                .then_some(())
                .ok_or(libc::EACCES);
        }
        if req.uid() != self.shared.owner || req.pid() <= 1 {
            return Err(libc::EACCES);
        }
        let mut status = String::new();
        File::open(format!("/proc/{}/status", req.pid()))
            .map_err(errno)?
            .take(16385)
            .read_to_string(&mut status)
            .map_err(errno)?;
        if status.len() > 16384 {
            return Err(libc::EACCES);
        }
        let leader = status
            .lines()
            .find_map(|line| line.strip_prefix("Tgid:"))
            .and_then(|value| value.trim().parse::<i32>().ok())
            .ok_or(libc::EACCES)?;
        let peer = crate::worker::gui_transport::kernel::sender(libc::ucred {
            pid: leader,
            uid: req.uid(),
            gid: req.gid(),
        })
        .map_err(errno)?;
        binding
            .process
            .is_peer(&peer)
            .map_err(|_| libc::EACCES)?
            .then_some(())
            .ok_or(libc::EACCES)
    }

    fn node_file(&self, inode: u64, readable: bool) -> Result<File, i32> {
        let node = self.nodes.get(&inode).ok_or(libc::ESTALE)?;
        let file = open_relative(&self.shared, &node.relative, readable)?;
        if identity(&file)? != identity(&node.pin)? {
            return Err(libc::ESTALE);
        }
        Ok(file)
    }

    fn attr(&self, inode: u64) -> Result<FileAttr, i32> {
        let metadata = self.node_file(inode, false)?.metadata().map_err(errno)?;
        Ok(FileAttr {
            ino: inode,
            size: metadata.len(),
            blocks: metadata.blocks(),
            atime: metadata.accessed().map_err(errno)?,
            mtime: metadata.modified().map_err(errno)?,
            ctime: SystemTime::UNIX_EPOCH,
            crtime: SystemTime::UNIX_EPOCH,
            kind: if metadata.is_dir() {
                FileType::Directory
            } else {
                FileType::RegularFile
            },
            perm: (metadata.mode() & if metadata.is_dir() { 0o555 } else { 0o444 }) as u16,
            nlink: if metadata.is_dir() { 2 } else { 1 },
            uid: metadata.uid(),
            gid: metadata.gid(),
            rdev: 0,
            blksize: u32::try_from(metadata.blksize()).map_err(|_| libc::EOVERFLOW)?,
            flags: 0,
        })
    }

    fn lookup_node(&mut self, parent: u64, name: &OsStr) -> Result<(u64, FileAttr), i32> {
        if name.as_bytes().is_empty()
            || name.as_bytes().contains(&b'/')
            || matches!(name.as_bytes(), b"." | b"..")
        {
            return Err(libc::EINVAL);
        }
        let parent_node = self.nodes.get(&parent).ok_or(libc::ESTALE)?;
        let path = parent_node.relative.join(name);
        let _parent = self.node_file(parent, false)?;
        if let Some(inode) = self.names.get(&path) {
            return Ok((*inode, self.attr(*inode)?));
        }
        if self.nodes.len() >= MAX_NODES {
            return Err(libc::ENOSPC);
        }
        let pin = open_relative(&self.shared, &path, false)?;
        let inode = self.next_inode;
        self.next_inode += 1;
        self.names.insert(path.clone(), inode);
        self.nodes.insert(
            inode,
            Node {
                relative: path,
                pin,
            },
        );
        Ok((inode, self.attr(inode)?))
    }

    fn selected_file(&self, inode: u64) -> Result<File, i32> {
        let node = self.nodes.get(&inode).ok_or(libc::ESTALE)?;
        let chosen = self.shared.chosen.lock().map_err(|_| libc::EIO)?;
        for lease in chosen.iter() {
            if fd_path(&lease.pin)? != self.shared.root_path.join(&lease.relative) {
                continue;
            }
            let file = if lease.directory && node.relative.starts_with(&lease.relative) {
                walk(
                    &self.shared,
                    &lease.pin,
                    node.relative
                        .strip_prefix(&lease.relative)
                        .map_err(|_| libc::EACCES)?,
                    true,
                )?
            } else if !lease.directory && node.relative == lease.relative {
                reopen(&self.shared, &lease.pin)?
            } else {
                continue;
            };
            if identity(&file)? != identity(&node.pin)? {
                return Err(libc::ESTALE);
            }
            return Ok(file);
        }
        Err(libc::EACCES)
    }

    fn add_handle(&mut self, handle: Handle) -> Result<u64, i32> {
        if self.handles.len() >= MAX_HANDLES {
            return Err(libc::EMFILE);
        }
        let id = self.next_handle;
        self.next_handle = self.next_handle.checked_add(1).ok_or(libc::EMFILE)?;
        self.handles.insert(id, handle);
        Ok(id)
    }

    fn directory(&mut self, inode: u64) -> Result<Vec<(u64, FileType, OsString)>, i32> {
        let file = self.node_file(inode, true)?;
        if !file.metadata().map_err(errno)?.is_dir() {
            return Err(libc::ENOTDIR);
        }
        let _identity = FsIdentityGuard::enter(self.shared.owner).map_err(|_| libc::EACCES)?;
        let duplicate = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
        if duplicate < 0 {
            return Err(errno(std::io::Error::last_os_error()));
        }
        let stream = unsafe { libc::fdopendir(duplicate) };
        if stream.is_null() {
            unsafe { libc::close(duplicate) };
            return Err(errno(std::io::Error::last_os_error()));
        }
        struct Directory(*mut libc::DIR);
        impl Drop for Directory {
            fn drop(&mut self) {
                unsafe { libc::closedir(self.0) };
            }
        }
        let stream = Directory(stream);
        let mut names = Vec::new();
        loop {
            unsafe { *libc::__errno_location() = 0 };
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                let error = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
                if error != 0 {
                    return Err(error);
                }
                break;
            }
            let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            if names.len() >= MAX_NODES {
                return Err(libc::EOVERFLOW);
            }
            names.push(OsString::from_vec(bytes.to_vec()));
        }
        drop(stream);
        drop(_identity);
        names.sort();
        let mut entries = vec![
            (inode, FileType::Directory, ".".into()),
            (1, FileType::Directory, "..".into()),
        ];
        for name in names {
            match self.lookup_node(inode, &name) {
                Ok((id, attr)) => entries.push((id, attr.kind, name)),
                Err(libc::EACCES | libc::ENOENT | libc::ESTALE | libc::ELOOP) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(entries)
    }
}

impl fuser::Filesystem for Filesystem {
    fn lookup(&mut self, req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let result = self
            .authorize(req, true, false)
            .and_then(|_| self.lookup_node(parent, name));
        match result {
            Ok((_, attr)) => reply.entry(&TTL, &attr, 1),
            Err(error) => reply.error(error),
        }
    }

    fn getattr(&mut self, req: &Request<'_>, inode: u64, fh: Option<u64>, reply: ReplyAttr) {
        let opened = fh.is_some_and(|handle| match self.handles.get(&handle) {
            Some(
                Handle::File {
                    inode: expected, ..
                }
                | Handle::Directory {
                    inode: expected, ..
                },
            ) => *expected == inode,
            _ => false,
        });
        match self
            .authorize(req, true, opened)
            .and_then(|_| self.attr(inode))
        {
            Ok(attr) => reply.attr(&TTL, &attr),
            Err(error) => reply.error(error),
        }
    }

    fn opendir(&mut self, req: &Request<'_>, inode: u64, flags: i32, reply: ReplyOpen) {
        let result = self.authorize(req, true, false).and_then(|_| {
            if flags & libc::O_ACCMODE != libc::O_RDONLY {
                return Err(libc::EROFS);
            }
            let entries = self.directory(inode)?;
            self.add_handle(Handle::Directory { inode, entries })
        });
        match result {
            Ok(handle) => reply.opened(handle, 0),
            Err(error) => reply.error(error),
        }
    }

    fn readdir(
        &mut self,
        req: &Request<'_>,
        inode: u64,
        fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        let result = self
            .authorize(req, true, self.handles.contains_key(&fh))
            .and_then(|_| {
                let offset = usize::try_from(offset).map_err(|_| libc::EINVAL)?;
                match self.handles.get(&fh) {
                    Some(Handle::Directory {
                        inode: expected,
                        entries,
                    }) if *expected == inode => Ok((offset, entries)),
                    _ => Err(libc::EBADF),
                }
            });
        match result {
            Ok((offset, entries)) => {
                for (index, (id, kind, name)) in entries.iter().enumerate().skip(offset) {
                    if reply.add(*id, (index + 1) as i64, *kind, name) {
                        break;
                    }
                }
                reply.ok();
            }
            Err(error) => reply.error(error),
        }
    }

    fn open(&mut self, req: &Request<'_>, inode: u64, flags: i32, reply: ReplyOpen) {
        let result = self.authorize(req, false, false).and_then(|_| {
            if flags & libc::O_ACCMODE != libc::O_RDONLY
                || flags & (libc::O_TRUNC | libc::O_APPEND) != 0
            {
                return Err(libc::EROFS);
            }
            let file = self.selected_file(inode)?;
            if !file.metadata().map_err(errno)?.is_file() {
                return Err(libc::EISDIR);
            }
            self.add_handle(Handle::File { inode, file })
        });
        // No passthrough/keep-cache flag. mmap may still use the per-instance
        // page cache; revoke/expiry therefore retires the actual process tree.
        match result {
            Ok(handle) => reply.opened(handle, 0),
            Err(error) => reply.error(error),
        }
    }

    fn read(
        &mut self,
        req: &Request<'_>,
        inode: u64,
        fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        let result = self
            .authorize(req, false, self.handles.contains_key(&fh))
            .and_then(|_| {
                let selected = self.selected_file(inode)?;
                let offset = u64::try_from(offset).map_err(|_| libc::EINVAL)?;
                if size > MAX_READ {
                    return Err(libc::EOVERFLOW);
                }
                let Some(Handle::File {
                    inode: expected,
                    file,
                }) = self.handles.get(&fh)
                else {
                    return Err(libc::EBADF);
                };
                if *expected != inode || identity(file)? != identity(&selected)? {
                    return Err(libc::ESTALE);
                }
                let _identity =
                    FsIdentityGuard::enter(self.shared.owner).map_err(|_| libc::EACCES)?;
                let mut data = vec![0; size as usize];
                let count = file.read_at(&mut data, offset).map_err(errno)?;
                data.truncate(count);
                Ok(data)
            });
        match result {
            Ok(data) => reply.data(&data),
            Err(error) => reply.error(error),
        }
    }

    fn release(
        &mut self,
        _req: &Request<'_>,
        _inode: u64,
        fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        self.handles.remove(&fh);
        reply.ok();
    }

    fn releasedir(
        &mut self,
        _req: &Request<'_>,
        _inode: u64,
        fh: u64,
        _flags: i32,
        reply: ReplyEmpty,
    ) {
        self.handles.remove(&fh);
        reply.ok();
    }

    fn flush(
        &mut self,
        req: &Request<'_>,
        _inode: u64,
        fh: u64,
        _lock_owner: u64,
        reply: ReplyEmpty,
    ) {
        match self.authorize(req, false, self.handles.contains_key(&fh)) {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error),
        }
    }
}
