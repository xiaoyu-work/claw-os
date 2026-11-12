use std::collections::BTreeMap;
use std::ffi::{CStr, OsStr, OsString};
use std::fs::File;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;
use std::time::{Duration, SystemTime};

use fuser::{
    FileAttr, FileType, PollHandle, ReplyAttr, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyStatfs,
    Request,
};
use fuser::{ReplyCreate, ReplyWrite, TimeOrNow};
#[cfg(test)]
use fuser::ReplyPoll;

use super::Shared;
use super::{BackingStage, SaveTarget};
#[cfg(test)]
use super::ChosenWrite;
use crate::clawd::client_identity::FsIdentityGuard;

const MAX_NODES: usize = 4096;
const MAX_HANDLES: usize = 128;
const MAX_READ: u32 = 131072;
const MAX_FILE: u64 = 8 * 1024 * 1024;
const MAX_SAVES: usize = 8;
const MAX_BYTES_PER_VIEW: u64 = 64 * 1024 * 1024;
const MAX_MUTATIONS: u64 = 256;
const TTL: Duration = Duration::ZERO;

struct Node {
    relative: PathBuf,
    pin: File,
    save: Option<PathBuf>,
    detached: bool,
}

enum Handle {
    SelectedFile { inode: u64, file: File, selection: String },
    File {
        inode: u64,
        file: File,
        read: bool,
        write: bool,
        own_save: bool,
        append: bool,
        watch_version: u64,
    },
    Directory {
        inode: u64,
        entries: Vec<(u64, FileType, OsString)>,
    },
}

impl Handle {
    fn inode(&self) -> u64 {
        match self {
            Self::SelectedFile { inode, .. } => *inode,
            Self::File { inode, .. } => *inode,
            Self::Directory { inode, .. } => *inode,
        }
    }
}

pub(super) struct Observed {
    pub(super) relative: PathBuf,
    pub(super) pin: File,
    parent: u64,
    pub(super) stamp: [u64; 8],
    pub(super) version: u64,
    pub(super) linked: bool,
}

pub(super) struct Subscription {
    inode: u64,
    handle: PollHandle,
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

pub(super) fn identity(file: &File) -> Result<(u64, u64), i32> {
    file.metadata()
        .map(|metadata| (metadata.dev(), metadata.ino()))
        .map_err(errno)
}

fn same_file(left: &File, right: &File) -> bool {
    identity(left)
        .ok()
        .zip(identity(right).ok())
        .is_some_and(|(left, right)| left == right)
}

fn fd_path(file: &File) -> Result<PathBuf, i32> {
    std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).map_err(errno)
}

fn request_leader(pid: u32) -> Result<i32, i32> {
    let mut status = String::new();
    File::open(format!("/proc/{pid}/status"))
        .map_err(errno)?
        .take(16385)
        .read_to_string(&mut status)
        .map_err(errno)?;
    if status.len() > 16384 {
        return Err(libc::EACCES);
    }
    status
        .lines()
        .find_map(|line| line.strip_prefix("Tgid:"))
        .and_then(|value| value.trim().parse::<i32>().ok())
        .ok_or(libc::EACCES)
}

fn checked_object(shared: &Shared, file: &File) -> Result<(), i32> {
    let actual = fd_path(file)?;
    let boundary = shared.metadata_root.as_ref()
        .map_or(shared.root_path.as_path(), |source| source.path());
    if !actual.starts_with(boundary) {
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
    if let Some(source) = &shared.metadata_root {
        // Keep authorization Home-relative; strip the prefix only for descriptor IO.
        let local = path.strip_prefix(&source.relative).map_err(|_| libc::EACCES)?;
        walk(shared, &source.root, local, readable)
    } else {
        walk(shared, &shared.root, path, readable)
    }
}

fn walk(shared: &Shared, base: &File, path: &Path, readable: bool) -> Result<File, i32> {
    shared.live()?;
    let _identity = FsIdentityGuard::enter(shared.owner).map_err(|_| libc::EACCES)?;
    if fd_path(&shared.root)? != shared.root_path {
        return Err(libc::ESTALE);
    }
    if let Some(source) = &shared.metadata_root {
        if fd_path(&source.root)? != source.root_path {
            return Err(libc::ESTALE);
        }
    }
    checked_object(shared, base)?;
    let parts = relative_parts(path)?;
    let mut file = base.try_clone().map_err(errno)?;
    if parts.is_empty() && readable {
        drop(_identity);
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
    drop(_identity);
    shared.live()?;
    Ok(file)
}

pub(super) fn reopen(shared: &Shared, pin: &File) -> Result<File, i32> {
    reopen_as(shared, pin, libc::O_RDONLY)
}

pub(super) fn reopen_as(shared: &Shared, pin: &File, flags: i32) -> Result<File, i32> {
    shared.live()?;
    let _identity = FsIdentityGuard::enter(shared.owner).map_err(|_| libc::EACCES)?;
    checked_object(shared, pin)?;
    // This is a Root-retained descriptor, never an App-provided proc path.
    let file = std::fs::OpenOptions::new()
        .read(flags & libc::O_ACCMODE != libc::O_WRONLY)
        .write(flags & libc::O_ACCMODE != libc::O_RDONLY)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC | (flags & libc::O_APPEND))
        .open(format!("/proc/self/fd/{}", pin.as_raw_fd()))
        .map_err(errno)?;
    if identity(pin)? != identity(&file)? {
        return Err(libc::ESTALE);
    }
    checked_object(shared, &file)?;
    Ok(file)
}

#[cfg(test)]
pub(super) fn choose_write(shared: &Shared, relative: &Path) -> Result<(), i32> {
    if relative.as_os_str().is_empty() || shared.root_path.join(relative).to_str().is_none() {
        return Err(libc::EINVAL);
    }
    let pin = open_relative(shared, relative, false)?;
    let metadata = pin.metadata().map_err(errno)?;
    if !metadata.is_file() {
        return Err(libc::EISDIR);
    }
    if metadata.len() > MAX_FILE {
        return Err(libc::EFBIG);
    }
    let _write = reopen_as(shared, &pin, libc::O_WRONLY)?;
    // The existing inverse recorder needs owner-readable bytes, not an App
    // content-read grant. Never bypass owner permissions to obtain an inverse.
    let _inverse = reopen(shared, &pin)?;
    let parent = open_relative(shared, relative.parent().ok_or(libc::EINVAL)?, true)?;
    let mut writes = shared.writes.lock().map_err(|_| libc::EIO)?;
    if writes.len() >= 32 {
        return Err(libc::ENOSPC);
    }
    writes.push(ChosenWrite {
        relative: relative.into(),
        pin,
        parent,
        permission: None,
    });
    Ok(())
}

pub(super) fn reserve_save(shared: &Shared, target: &Path) -> Result<PathBuf, i32> {
    shared.live()?;
    let writes = shared.writes.lock().map_err(|_| libc::EIO)?;
    let selected = writes
        .iter()
        .find(|write| write.relative == target)
        .ok_or(libc::EACCES)?;
    let current = open_relative(shared, target, false)?;
    if identity(&current)? != identity(&selected.pin)? {
        return Err(libc::ESTALE);
    }
    let directory = target.parent().ok_or(libc::EINVAL)?;
    if identity(&open_relative(shared, directory, true)?)? != identity(&selected.parent)? {
        return Err(libc::ESTALE);
    }
    let parent_metadata = selected.parent.metadata().map_err(errno)?;
    if parent_metadata.mode() & libc::S_ISVTX != 0
        && parent_metadata.uid() != shared.owner
        && current.metadata().map_err(errno)?.uid() != shared.owner
    {
        return Err(libc::EACCES);
    }
    {
        let _owner = FsIdentityGuard::enter(shared.owner).map_err(|_| libc::EACCES)?;
        let proof = crate::clawd::filesystem::open_at(
            &selected.parent,
            OsStr::new("."),
            libc::O_RDWR | libc::O_TMPFILE,
            0o600,
        )
        .map_err(errno)?;
        if proof.metadata().map_err(errno)?.nlink() != 0 {
            return Err(libc::EIO);
        }
    }
    let mut saves = shared.saves.lock().map_err(|_| libc::EIO)?;
    if saves.len() >= MAX_SAVES {
        return Err(libc::ENOSPC);
    }
    let relative = directory.join(format!(".cos-save-{}", uuid::Uuid::new_v4().simple()));
    match open_relative(shared, &relative, false) {
        Err(libc::ENOENT) => {}
        Err(error) => return Err(error),
        Ok(_) => return Err(libc::EEXIST),
    }
    saves.push(SaveTarget {
        relative: relative.clone(),
        target: target.into(),
    });
    Ok(relative)
}

fn snapshot(shared: &Shared, pin: &File) -> Result<(File, Vec<u8>), i32> {
    let mut file = reopen(shared, pin)?;
    let before = fingerprint(&file.metadata().map_err(errno)?);
    let mut bytes = Vec::new();
    let _owner = FsIdentityGuard::enter(shared.owner).map_err(|_| libc::EACCES)?;
    (&mut file).take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(errno)?;
    if bytes.len() as u64 > MAX_FILE {
        return Err(libc::EFBIG);
    }
    if before != fingerprint(&pin.metadata().map_err(errno)?) {
        return Err(libc::EAGAIN);
    }
    Ok((file, bytes))
}

enum MutationImage<'a> {
    Write { offset: u64, bytes: &'a [u8] },
    Truncate(u64),
    Replace(&'a [u8]),
}

impl MutationImage<'_> {
    fn replacement(&self) -> bool {
        matches!(self, Self::Replace(_))
    }

    fn expected(&self, previous: &[u8]) -> Result<Vec<u8>, i32> {
        let mut image = match self {
            Self::Replace(bytes) if bytes.len() as u64 <= MAX_FILE => return Ok(bytes.to_vec()),
            Self::Replace(_) => return Err(libc::EFBIG),
            _ => previous.to_vec(),
        };
        match self {
            Self::Write { offset, bytes } => {
                if bytes.is_empty() {
                    return Ok(image);
                }
                let end = offset
                    .checked_add(bytes.len() as u64)
                    .filter(|end| *end <= MAX_FILE)
                    .ok_or(libc::EFBIG)?;
                if end > image.len() as u64 {
                    image.resize(end as usize, 0);
                }
                image[*offset as usize..end as usize].copy_from_slice(bytes);
            }
            Self::Truncate(size) if *size <= MAX_FILE => image.resize(*size as usize, 0),
            Self::Truncate(_) => return Err(libc::EFBIG),
            Self::Replace(_) => unreachable!("replacement returned above"),
        }
        Ok(image)
    }
}

/// A checked image at one point in time, not external-writer serialization or CAS.
/// Later changes still invalidate selections; same-byte/ABA host writes cannot
/// be distinguished from the intended image.
pub(super) struct VerifiedMutation {
    published: File,
    postimage: Vec<u8>,
    stamp: [u64; 8],
    previous: File,
    preimage: Vec<u8>,
    old_stamp: Option<[u64; 8]>,
}

fn verify_exact_image(
    owner: u32,
    file: &File,
    expected: &[u8],
    links: u64,
) -> Result<[u64; 8], i32> {
    if expected.len() as u64 > MAX_FILE {
        return Err(libc::EFBIG);
    }
    let _owner = FsIdentityGuard::enter(owner).map_err(|_| libc::EACCES)?;
    let metadata = file.metadata().map_err(errno)?;
    if !metadata.is_file() || metadata.nlink() != links || metadata.len() != expected.len() as u64 {
        return Err(libc::ESTALE);
    }
    let before = fingerprint(&metadata);
    let mut buffer = vec![0; (MAX_READ as usize).min(expected.len().max(1))];
    let mut offset = 0usize;
    while offset < expected.len() {
        let take = buffer.len().min(expected.len() - offset);
        let count = file
            .read_at(&mut buffer[..take], offset as u64)
            .map_err(errno)?;
        if count == 0 || buffer[..count] != expected[offset..offset + count] {
            return Err(libc::EAGAIN);
        }
        offset += count;
    }
    let mut extra = [0];
    if file
        .read_at(&mut extra, expected.len() as u64)
        .map_err(errno)?
        != 0
        || fingerprint(&file.metadata().map_err(errno)?) != before
    {
        return Err(libc::EAGAIN);
    }
    Ok(before)
}

fn verify_mutation(
    owner: u32,
    previous: File,
    preimage: Vec<u8>,
    published: File,
    postimage: Vec<u8>,
    replace: bool,
) -> Result<VerifiedMutation, i32> {
    if (identity(&previous)? == identity(&published)?) == replace {
        return Err(libc::ESTALE);
    }
    let stamp = verify_exact_image(owner, &published, &postimage, 1)?;
    let old_stamp = if replace {
        Some(verify_exact_image(owner, &previous, &preimage, 0)?)
    } else {
        None
    };
    Ok(VerifiedMutation {
        published,
        postimage,
        stamp,
        previous,
        preimage,
        old_stamp,
    })
}

impl VerifiedMutation {
    pub(super) fn file(&self) -> &File {
        &self.published
    }

    pub(super) fn current_metadata(
        &self,
        shared: &Shared,
        path: &Path,
        previous: &File,
    ) -> Result<std::fs::Metadata, i32> {
        if identity(previous)? != identity(&self.previous)?
            || verify_exact_image(shared.owner, &self.published, &self.postimage, 1)? != self.stamp
        {
            return Err(libc::ESTALE);
        }
        if self.old_stamp.is_some() {
            self.old_version(shared.owner, &self.previous)?;
        }
        let current = open_relative(shared, path, false)?;
        let metadata = current.metadata().map_err(errno)?;
        if identity(&current)? != identity(&self.published)? || fingerprint(&metadata) != self.stamp {
            return Err(libc::ESTALE);
        }
        Ok(metadata)
    }

    pub(super) fn old_version(&self, owner: u32, file: &File) -> Result<[u64; 8], i32> {
        let expected = self.old_stamp.ok_or(libc::ESTALE)?;
        if identity(file)? != identity(&self.previous)?
            || verify_exact_image(owner, file, &self.preimage, 0)? != expected
        {
            return Err(libc::ESTALE);
        }
        Ok(expected)
    }
}

fn charge(counter: &std::sync::atomic::AtomicU64, bytes: u64) -> Result<(), i32> {
    counter
        .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current
                .checked_add(bytes)
                .filter(|total| *total <= MAX_BYTES_PER_VIEW)
        })
        .map(|_| ())
        .map_err(|_| libc::ENOSPC)
}

fn scratch_growth(shared: &Shared, file: &File, size: u64) -> Result<(), i32> {
    charge(
        &shared.scratch_bytes,
        size.saturating_sub(file.metadata().map_err(errno)?.len()),
    )
}

struct ActiveEffect<'a>(&'a std::sync::atomic::AtomicBool);

impl Drop for ActiveEffect<'_> {
    fn drop(&mut self) { self.0.store(false, Ordering::Release); }
}

fn mutate(
    shared: &Shared,
    path: &Path,
    pin: &File,
    image: MutationImage<'_>,
    effect: impl FnOnce() -> Result<File, i32>,
) -> Result<VerifiedMutation, i32> {
    let _serial = shared.effects.lock().map_err(|_| libc::EIO)?;
    shared.active_effect.store(true, Ordering::Release);
    let _active = ActiveEffect(&shared.active_effect);
    mutate_locked(shared, path, pin, image, effect)
}

fn mutate_locked(
    shared: &Shared,
    path: &Path,
    pin: &File,
    image: MutationImage<'_>,
    effect: impl FnOnce() -> Result<File, i32>,
) -> Result<VerifiedMutation, i32> {
    shared.live()?;
    shared.validate_session().map_err(|_| libc::EIO)?;
    let authorized = super::selected_write::authorize_effect(shared, path, pin)?;
    let before = fingerprint(&pin.metadata().map_err(errno)?);
    shared
        .mutation_attempts
        .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            (current < MAX_MUTATIONS).then_some(current + 1)
        })
        .map_err(|_| libc::ENOSPC)?;
    let (previous_file, previous) = snapshot(shared, pin)?;
    if fingerprint(&previous_file.metadata().map_err(errno)?) != before {
        return Err(libc::ESTALE);
    }
    let replace = image.replacement();
    let expected = image.expected(&previous)?;
    charge(&shared.inverse_bytes, previous.len() as u64)?;
    let sid = shared.session.get().ok_or(libc::EROFS)?;
    let logical = shared.root_path.join(path);
    if logical.to_str().is_none() {
        return Err(libc::EINVAL);
    }
    let command = if replace {
        crate::clawd::routes::Command::SystemFileReplace
    } else {
        crate::clawd::routes::Command::SystemFilesystemWrite
    };
    let bracket = shared
        .session_context(|| {
            let seq = crate::session::record_fs_write_bytes(sid, &logical, Some(&previous))
                .map_err(|error| error.to_string())?;
            let key = format!("{}:{seq}", sid.as_str());
            crate::session::journal::begin_mutation(crate::session::journal::MutationStart {
                partition: crate::session::journal::Partition::Session(sid.clone()),
                owner_uid: shared.owner,
                route: command.route().name,
                request_key: &key,
                grant: authorized.as_ref().map(|proof| proof.grant_ref().as_str()),
                session_mutation: Some(seq),
                context_ingest: false,
            })
            .map_err(|error| error.to_string())
        })
        .map_err(|_| libc::EIO)?;
    let current = open_relative(shared, path, false);
    let unchanged = current.as_ref().is_ok_and(|current| same_file(current, pin)
        && current.metadata().is_ok_and(|metadata| fingerprint(&metadata) == before));
    if shared.live().is_err() || !unchanged {
        let closed = shared.session_context(|| {
            bracket
                .fail(
                    "native_precondition_changed",
                    "native resource authority or source changed before dispatch",
                )
                .map_err(|error| error.to_string())
        });
        if let Err(error) = closed {
            shared.indeterminate(error);
        }
        return Err(libc::EACCES);
    }
    if super::selected_write::authorize_effect(shared, path, pin).is_err() {
        if let Err(error) = shared.session_context(|| bracket
            .fail("native_write_authority_ended", "selected write ended before dispatch")
            .map_err(|error| error.to_string()))
        { shared.indeterminate(error); }
        return Err(libc::EACCES);
    }
    let result = effect().and_then(|published| {
        let published = reopen(shared, &published)?;
        verify_mutation(shared.owner, previous_file, previous, published, expected, replace)
    });
    let outcome = shared.session_context(|| {
        if result.is_err() {
            return Err(bracket.indeterminate().to_string());
        }
        #[cfg(test)]
        let injected = shared.fail_completion.swap(false, Ordering::AcqRel);
        #[cfg(test)]
        if injected {
            crate::session::journal::faults::arm(
                crate::session::journal::faults::Fault::AppendWrite,
            );
        }
        let committed = bracket.commit().map_err(|error| error.to_string());
        #[cfg(test)]
        if injected {
            crate::session::journal::faults::disarm();
        }
        committed.map(|_| ())
    });
    if let Err(error) = outcome {
        shared.indeterminate(error);
        return Err(libc::EIO);
    }
    if shared.metadata_only && !replace {
        let published = (|| {
            let node = shared.observed.lock().map_err(|_| libc::EIO)?.iter()
                .find(|(_, node)| node.linked && node.relative == path)
                .map(|(inode, _)| *inode).ok_or(libc::ESTALE)?;
            super::selected_write::publish_owned_revision(
                shared, path, pin, node, result.as_ref().map_err(|_| libc::EIO)?,
            )
        })();
        if published.is_err() {
            shared.provider_failed("committed native write could not publish its verified resource revision".into());
            return Err(libc::EIO);
        }
    }
    result
}

pub(super) fn cleanup_stages(shared: &Shared, deadline: Instant) -> Result<(), String> {
    let _serial = loop {
        match shared.effects.try_lock() {
            Ok(guard) => break guard,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => break poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                if Instant::now() >= deadline {
                    return Err("native mutation still in flight; custody retained".into());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    };
    let mut stages = shared
        .stages
        .lock()
        .map_err(|_| "native backing stages poisoned")?;
    while let Some(stage) = stages.last() {
        if Instant::now() >= deadline {
            return Err("native stage retirement deadline; custody retained".into());
        }
        let _owner = FsIdentityGuard::enter(shared.owner)?;
        if stage
            .file
            .metadata()
            .map_err(|error| error.to_string())?
            .nlink()
            == 0
        {
            stage.parent.sync_all().map_err(|error| error.to_string())?;
            stages.pop();
            continue;
        }
        let current = crate::clawd::filesystem::open_at(
            &stage.parent,
            &stage.name,
            libc::O_PATH | libc::O_NOFOLLOW,
            0,
        )
        .map_err(|error| error.to_string())?;
        if identity(&current).map_err(|error| error.to_string())?
            != identity(&stage.file).map_err(|error| error.to_string())?
        {
            return Err("native backing stage identity changed; custody retained".into());
        }
        if !stage.published {
            let name =
                std::ffi::CString::new(stage.name.as_bytes()).map_err(|error| error.to_string())?;
            if unsafe { libc::unlinkat(stage.parent.as_raw_fd(), name.as_ptr(), 0) } != 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            if stage
                .file
                .metadata()
                .map_err(|error| error.to_string())?
                .nlink()
                != 0
            {
                return Err("native backing stage remains linked; custody retained".into());
            }
        }
        stage.parent.sync_all().map_err(|error| error.to_string())?;
        stages.pop();
    }
    Ok(())
}

fn materialize(
    shared: &Shared,
    target: &Path,
    expected: &File,
    parent: File,
    bytes: &[u8],
) -> Result<File, i32> {
    let _owner = FsIdentityGuard::enter(shared.owner).map_err(|_| libc::EACCES)?;
    let name = OsString::from(format!(".cos-native-{}", uuid::Uuid::new_v4().simple()));
    let mut stages = shared.stages.lock().map_err(|_| libc::EIO)?;
    if stages.len() >= MAX_SAVES {
        return Err(libc::ENOSPC);
    }
    let file = crate::clawd::filesystem::open_at(
        &parent,
        &name,
        libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
        0o600,
    )
    .map_err(errno)?;
    stages.push(BackingStage {
        parent,
        name,
        file,
        published: false,
    });
    let stage = stages.last_mut().ok_or(libc::EIO)?;
    stage.file.write_all_at(bytes, 0).map_err(errno)?;
    use std::os::unix::fs::PermissionsExt;
    stage
        .file
        .set_permissions(std::fs::Permissions::from_mode(
            expected.metadata().map_err(errno)?.mode() & 0o777,
        ))
        .map_err(errno)?;
    stage.file.sync_all().map_err(errno)?;
    let current = open_relative(shared, target, false)?;
    if !same_file(&current, expected) {
        return Err(libc::ESTALE);
    }
    if !same_file(
        &open_relative(shared, target.parent().ok_or(libc::EINVAL)?, true)?,
        &stage.parent,
    ) {
        return Err(libc::ESTALE);
    }
    let from = std::ffi::CString::new(stage.name.as_bytes()).map_err(|_| libc::EINVAL)?;
    let to = std::ffi::CString::new(target.file_name().ok_or(libc::EINVAL)?.as_bytes())
        .map_err(|_| libc::EINVAL)?;
    if unsafe {
        libc::renameat(
            stage.parent.as_raw_fd(),
            from.as_ptr(),
            stage.parent.as_raw_fd(),
            to.as_ptr(),
        )
    } != 0
    {
        return Err(errno(std::io::Error::last_os_error()));
    }
    stage.name = target.file_name().ok_or(libc::EINVAL)?.into();
    stage.published = true;
    let published = open_relative(shared, target, false)?;
    if !same_file(&published, &stage.file) {
        return Err(libc::ESTALE);
    }
    stage.parent.sync_all().map_err(errno)?;
    let file = stages.pop().ok_or(libc::EIO)?.file;
    Ok(file)
}

pub(super) fn fingerprint(metadata: &std::fs::Metadata) -> [u64; 8] {
    [
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime() as u64,
        metadata.mtime_nsec() as u64,
        metadata.ctime() as u64,
        metadata.ctime_nsec() as u64,
        metadata.nlink(),
    ]
}

fn change_time(metadata: &std::fs::Metadata) -> Result<SystemTime, i32> {
    let seconds = metadata.ctime();
    let base = if seconds >= 0 {
        SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(seconds as u64))
    } else {
        SystemTime::UNIX_EPOCH.checked_sub(Duration::from_secs(seconds.unsigned_abs()))
    }
    .ok_or(libc::EOVERFLOW)?;
    base.checked_add(Duration::from_nanos(
        u64::try_from(metadata.ctime_nsec()).map_err(|_| libc::EOVERFLOW)?,
    ))
    .ok_or(libc::EOVERFLOW)
}

fn requested_time(time: TimeOrNow) -> SystemTime {
    match time {
        TimeOrNow::SpecificTime(time) => time,
        TimeOrNow::Now => SystemTime::now(),
    }
}

pub(super) fn observe(shared: &Shared) -> Result<(), String> {
    while shared.live().is_ok()
        && !shared.ended.load(Ordering::Acquire)
        && !shared.monitor_stop.load(Ordering::Acquire)
    {
        let notifier = shared
            .notifier
            .lock()
            .map_err(|_| "native notifier poisoned")?
            .clone();
        let Some(notifier) = notifier else {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        };
        let owned = std::mem::take(&mut *shared.own_invalidations.lock()
            .map_err(|_| "native owned invalidations poisoned")?);
        for (inode, directory) in owned {
            notifier.inval_inode(inode, if directory { -1 } else { 0 }, 0)
                .map_err(|error| error.to_string())?;
        }
        let nodes = shared
            .observed
            .lock()
            .map_err(|_| "native observations poisoned")?
            .iter()
            .filter(|(_, node)| node.linked)
            .map(|(inode, node)| {
                Ok((
                    *inode,
                    node.relative.clone(),
                    node.pin.try_clone().map_err(|error| error.to_string())?,
                    node.parent,
                    node.stamp,
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        for (inode, path, pin, parent, previous) in nodes {
            if shared.live().is_err() {
                return Ok(());
            }
            let serial = match shared.effects.try_lock() {
                Ok(serial) => serial,
                Err(std::sync::TryLockError::WouldBlock) => continue,
                Err(std::sync::TryLockError::Poisoned(_)) => return Err("native effects poisoned".into()),
            };
            if !shared.observed.lock().map_err(|_| "native observations poisoned")?
                .get(&inode).is_some_and(|node| node.linked && node.stamp == previous)
            { continue; }
            let current = match open_relative(shared, &path, false) {
                Ok(current) if same_file(&current, &pin) => {
                    Some(current.metadata().map_err(|error| error.to_string())?)
                }
                Ok(_)
                | Err(libc::ENOENT | libc::ENOTDIR | libc::ELOOP | libc::EACCES | libc::ESTALE) => {
                    None
                }
                Err(error) => return Err(std::io::Error::from_raw_os_error(error).to_string()),
            };
            if current
                .as_ref()
                .is_some_and(|metadata| fingerprint(metadata) == previous)
            {
                continue;
            }
            let directory = pin.metadata().map_err(|error| error.to_string())?.is_dir();
            drop(serial);
            notifier
                .inval_inode(inode, if directory { -1 } else { 0 }, 0)
                .map_err(|error| error.to_string())?;
            if current.is_none() && inode != 1 {
                notifier
                    .delete(
                        parent,
                        inode,
                        path.file_name().ok_or("observed file has no name")?,
                    )
                    .map_err(|error| error.to_string())?;
            }
            // Entries have zero TTL. Pre-evicting child dentries here would
            // erase the cached name needed by the exact delete notification.
            {
                let mut observed = shared
                    .observed
                    .lock()
                    .map_err(|_| "native observations poisoned")?;
                let Some(node) = observed.get_mut(&inode) else {
                    continue;
                };
                if !same_file(&node.pin, &pin) || !node.linked || node.stamp != previous {
                    continue;
                }
                node.version = node
                    .version
                    .checked_add(1)
                    .ok_or("native watch generation overflow")?;
                if let Some(metadata) = current {
                    node.stamp = fingerprint(&metadata);
                } else {
                    node.linked = false;
                }
            }
            let mut polls = shared.polls.lock().map_err(|_| "native polls poisoned")?;
            let ready = polls
                .iter()
                .filter(|(_, poll)| poll.inode == inode)
                .map(|(fh, _)| *fh)
                .collect::<Vec<_>>();
            let handles = ready
                .into_iter()
                .filter_map(|fh| polls.remove(&fh))
                .collect::<Vec<_>>();
            drop(polls);
            for subscription in handles {
                match subscription.handle.notify() {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.to_string()),
                }
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Ok(())
}

impl Filesystem {
    pub(super) fn new(shared: Arc<Shared>) -> Result<Self, String> {
        let (root, relative) = if let Some(source) = &shared.metadata_root {
            (source.root.try_clone().map_err(|error| error.to_string())?, source.relative.clone())
        } else {
            (shared.root.try_clone().map_err(|error| error.to_string())?, PathBuf::new())
        };
        let filesystem = Self {
            shared,
            nodes: BTreeMap::from([(
                1,
                Node {
                    relative: relative.clone(),
                    pin: root,
                    save: None,
                    detached: false,
                },
            )]),
            names: BTreeMap::from([(relative, 1)]),
            handles: BTreeMap::new(),
            next_inode: 2,
            next_handle: 1,
        };
        filesystem.track(1, 1).map_err(|error| error.to_string())?;
        Ok(filesystem)
    }

    fn authorize(&self, req: &Request<'_>, metadata: bool, kernel_handle: bool) -> Result<(), i32> {
        let result = self.authorize_request(req, metadata, kernel_handle);
        if let Err(error) = result {
            tracing::warn!(
                pid = req.pid(),
                uid = req.uid(),
                gid = req.gid(),
                owner = self.shared.owner,
                controller = self.shared.controller.pid(),
                metadata,
                kernel_handle,
                error,
                "private native filesystem peer refused"
            );
        }
        result
    }

    fn authorize_request(
        &self,
        req: &Request<'_>,
        metadata: bool,
        kernel_handle: bool,
    ) -> Result<(), i32> {
        #[cfg(test)]
        if self.shared.fail_worker.swap(false, Ordering::AcqRel) {
            panic!("private native filesystem worker failure");
        }
        self.shared.live()?;
        if metadata
            && req.uid() == 0
            && req.pid() != 0
            && request_leader(req.pid())? == self.shared.controller.pid()
        {
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
        let leader = request_leader(req.pid())?;
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
        if node.save.is_some() || node.detached {
            return node.pin.try_clone().map_err(errno);
        }
        let file = open_relative(&self.shared, &node.relative, readable)?;
        if identity(&file)? != identity(&node.pin)? {
            return Err(libc::ESTALE);
        }
        Ok(file)
    }

    fn attr(&self, inode: u64) -> Result<FileAttr, i32> {
        let metadata = self.node_file(inode, false)?.metadata().map_err(errno)?;
        let attr = FileAttr {
            ino: inode,
            size: metadata.len(),
            blocks: metadata.blocks(),
            atime: metadata.accessed().map_err(errno)?,
            mtime: metadata.modified().map_err(errno)?,
            ctime: change_time(&metadata)?,
            crtime: SystemTime::UNIX_EPOCH,
            kind: if metadata.is_dir() {
                FileType::Directory
            } else {
                FileType::RegularFile
            },
            perm: (metadata.mode() & if metadata.is_dir() { 0o555 } else { 0o444 }) as u16,
            nlink: if metadata.is_dir() {
                2
            } else {
                u32::from(metadata.nlink() != 0)
            },
            uid: metadata.uid(),
            gid: metadata.gid(),
            rdev: 0,
            blksize: u32::try_from(metadata.blksize()).map_err(|_| libc::EOVERFLOW)?,
            flags: 0,
        };
        let attr = self.writable_attr(inode, &metadata, attr)?;
        Ok(attr)
    }

    fn writable_attr(
        &self,
        inode: u64,
        metadata: &std::fs::Metadata,
        mut attr: FileAttr,
    ) -> Result<FileAttr, i32> {
        let node = self.nodes.get(&inode).ok_or(libc::ESTALE)?;
        let private = node.save.is_some();
        let writable = private
            || self
                .shared
                .writes
                .lock()
                .map_err(|_| libc::EIO)?
                .iter()
                .any(|write| write.relative == node.relative && same_file(&write.pin, &node.pin));
        let save_parent = metadata.is_dir()
            && self
                .shared
                .saves
                .lock()
                .map_err(|_| libc::EIO)?
                .iter()
                .any(|save| save.relative.parent() == Some(node.relative.as_path()));
        attr.perm = if private {
            0o600
        } else {
            (metadata.mode()
                & if metadata.is_dir() {
                    if save_parent {
                        0o777
                    } else {
                        0o555
                    }
                } else if writable {
                    0o666
                } else {
                    0o444
                }) as u16
        };
        if private {
            attr.nlink = u32::from(!node.detached);
            attr.uid = self.shared.owner;
            attr.gid = crate::clawd::client_identity::owner_groups(self.shared.owner)
                .map_err(|_| libc::EACCES)?
                .0;
        }
        Ok(attr)
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
            if self
                .nodes
                .get(inode)
                .is_some_and(|node| node.save.is_some())
            {
                return Ok((*inode, self.attr(*inode)?));
            }
        }
        let pin = open_relative(&self.shared, &path, false)?;
        if let Some(inode) = self.names.get(&path) {
            if identity(&self.nodes.get(inode).ok_or(libc::ESTALE)?.pin)? == identity(&pin)? {
                return Ok((*inode, self.attr(*inode)?));
            }
        }
        if self.nodes.len() >= MAX_NODES {
            return Err(libc::ENOSPC);
        }
        let inode = self.next_inode;
        self.next_inode += 1;
        self.names.insert(path.clone(), inode);
        self.nodes.insert(
            inode,
            Node {
                relative: path,
                pin,
                save: None,
                detached: false,
            },
        );
        self.track(inode, parent)?;
        Ok((inode, self.attr(inode)?))
    }

    fn track(&self, inode: u64, parent: u64) -> Result<(), i32> {
        let node = self.nodes.get(&inode).ok_or(libc::ESTALE)?;
        let metadata = node.pin.metadata().map_err(errno)?;
        self.shared.observed.lock().map_err(|_| libc::EIO)?.insert(
            inode,
            Observed {
                relative: node.relative.clone(),
                pin: node.pin.try_clone().map_err(errno)?,
                parent,
                stamp: fingerprint(&metadata),
                version: 0,
                linked: true,
            },
        );
        Ok(())
    }

    #[cfg(test)]
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

    fn readable_file(&self, inode: u64) -> Result<File, i32> {
        if self.shared.metadata_only {
            return super::ReadProvider::new(self.shared.clone()).map_err(|_| libc::EACCES)?
                .open(inode, None).map(|(_, file)| file);
        }
        #[cfg(test)]
        { self.selected_file(inode) }
        #[cfg(not(test))]
        { Err(libc::EACCES) }
    }

    fn selected_write(&self, inode: u64, flags: i32) -> Result<File, i32> {
        self.shared.live()?;
        if self.shared.session.get().is_none() {
            return Err(libc::EROFS);
        }
        let node = self.nodes.get(&inode).ok_or(libc::ESTALE)?;
        if node.detached {
            return Err(libc::ESTALE);
        }
        if node.save.is_some() {
            if self.shared.metadata_only {
                let target = node.save.as_ref().ok_or(libc::EACCES)?;
                let pin = open_relative(&self.shared, target, false)?;
                let permission = super::selected_write::permission(&self.shared, target, &pin)?
                    .ok_or(libc::EACCES)?;
                let _authorized = permission.authorize()?;
                permission.candidate.revalidate().map_err(|_| libc::ESTALE)?;
            }
            return node.pin.try_clone().map_err(errno);
        }
        let writes = self.shared.writes.lock().map_err(|_| libc::EIO)?;
        let selected = writes
            .iter()
            .find(|write| write.relative == node.relative && same_file(&write.pin, &node.pin))
            .ok_or(libc::EACCES)?;
        let relative = selected.relative.clone();
        let pin = selected.pin.try_clone().map_err(errno)?;
        let permission = selected.permission.clone();
        drop(writes);
        if self.shared.metadata_only {
            let permission = permission.ok_or(libc::EACCES)?;
            let _authorized = permission.authorize()?;
            permission.candidate.revalidate().map_err(|_| libc::ESTALE)?;
        }
        let current = open_relative(&self.shared, &relative, false)?;
        if identity(&current)? != identity(&node.pin)? {
            return Err(libc::ESTALE);
        }
        reopen_as(&self.shared, &pin, flags)
    }

    fn handle_file(&mut self, inode: u64, flags: i32) -> Result<u64, i32> {
        if self.handles.len() >= MAX_HANDLES || self.next_handle == u64::MAX {
            return Err(libc::EMFILE);
        }
        let own_save = self.nodes.get(&inode).ok_or(libc::ESTALE)?.save.is_some();
        let read = flags & libc::O_ACCMODE != libc::O_WRONLY;
        let write = flags & libc::O_ACCMODE != libc::O_RDONLY;
        if flags & libc::O_ACCMODE > libc::O_RDWR {
            return Err(libc::EINVAL);
        }
        let file = if own_save {
            self.node_file(inode, false)?
        } else if write {
            if read {
                self.readable_file(inode)?;
            }
            self.selected_write(inode, flags)?
        } else {
            self.readable_file(inode)?
        };
        if !file.metadata().map_err(errno)?.is_file() {
            return Err(libc::EISDIR);
        }
        if flags & libc::O_TRUNC != 0 {
            if !write {
                return Err(libc::EACCES);
            }
            self.truncate(inode, 0, None, None)?;
        }
        let watch_version = self
            .shared
            .observed
            .lock()
            .map_err(|_| libc::EIO)?
            .get(&inode)
            .map(|node| node.version)
            .unwrap_or(0);
        self.add_handle(Handle::File {
            inode,
            file,
            read,
            write,
            own_save,
            append: flags & libc::O_APPEND != 0,
            watch_version,
        })
    }

    fn truncate(
        &self,
        inode: u64,
        size: u64,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
    ) -> Result<(), i32> {
        if size > MAX_FILE {
            return Err(libc::EFBIG);
        }
        let file = self.selected_write(inode, libc::O_WRONLY)?;
        let node = self.nodes.get(&inode).ok_or(libc::ESTALE)?;
        let mut times = std::fs::FileTimes::new();
        let set_times = atime.is_some() || mtime.is_some();
        if let Some(time) = atime {
            times = times.set_accessed(requested_time(time));
        }
        if let Some(time) = mtime {
            times = times.set_modified(requested_time(time));
        }
        if node.save.is_some() {
            scratch_growth(&self.shared, &file, size)?;
            file.set_len(size).map_err(errno)?;
            if set_times {
                file.set_times(times).map_err(errno)?;
            }
            return Ok(());
        }
        mutate(&self.shared, &node.relative, &node.pin, MutationImage::Truncate(size), || {
            let _owner = FsIdentityGuard::enter(self.shared.owner).map_err(|_| libc::EACCES)?;
            file.set_len(size).map_err(errno)?;
            if set_times {
                file.set_times(times).map_err(errno)?;
            }
            file.sync_all().map_err(errno)?;
            node.pin.try_clone().map_err(errno)
        }).map(|_| ())
    }

    fn create_save(&mut self, parent: u64, name: &OsStr, flags: i32) -> Result<(u64, u64), i32> {
        if self.shared.session.get().is_none() {
            return Err(libc::EROFS);
        }
        let directory = self
            .nodes
            .get(&parent)
            .ok_or(libc::ESTALE)?
            .relative
            .clone();
        let relative = directory.join(name);
        relative_parts(&relative)?;
        let target = self
            .shared
            .saves
            .lock()
            .map_err(|_| libc::EIO)?
            .iter()
            .find(|save| save.relative == relative)
            .map(|save| save.target.clone())
            .ok_or(libc::EACCES)?;
        if self.names.contains_key(&relative) {
            return Err(libc::EEXIST);
        }
        if self.nodes.len() >= MAX_NODES {
            return Err(libc::ENOSPC);
        }
        let writes = self.shared.writes.lock().map_err(|_| libc::EIO)?;
        let selected = writes
            .iter()
            .find(|write| write.relative == target)
            .ok_or(libc::EACCES)?;
        if !same_file(&self.node_file(parent, false)?, &selected.parent) {
            return Err(libc::ESTALE);
        }
        if !same_file(&open_relative(&self.shared, &target, false)?, &selected.pin) {
            return Err(libc::ESTALE);
        }
        match open_relative(&self.shared, &relative, false) {
            Err(libc::ENOENT) => {}
            Err(error) => return Err(error),
            Ok(_) => return Err(libc::EEXIST),
        }
        drop(writes);
        let descriptor =
            unsafe { libc::memfd_create(c"claw-native-save".as_ptr(), libc::MFD_CLOEXEC) };
        if descriptor < 0 {
            return Err(errno(std::io::Error::last_os_error()));
        }
        let pin = unsafe { File::from_raw_fd(descriptor) };
        let inode = self.next_inode;
        self.next_inode += 1;
        self.names.insert(relative.clone(), inode);
        self.nodes.insert(
            inode,
            Node {
                relative,
                pin,
                save: Some(target),
                detached: false,
            },
        );
        let handle = match self.handle_file(inode, flags) {
            Ok(handle) => handle,
            Err(error) => {
                self.names.retain(|_, selected| *selected != inode);
                self.nodes.remove(&inode);
                return Err(error);
            }
        };
        Ok((inode, handle))
    }

    fn save(
        &mut self,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        flags: u32,
    ) -> Result<(), i32> {
        if parent != newparent {
            return Err(libc::EXDEV);
        }
        if flags == libc::RENAME_NOREPLACE {
            return Err(libc::EEXIST);
        }
        if flags != 0 {
            return Err(libc::EOPNOTSUPP);
        }
        let directory = self
            .nodes
            .get(&parent)
            .ok_or(libc::ESTALE)?
            .relative
            .clone();
        let from = directory.join(name);
        let to = directory.join(newname);
        relative_parts(&from)?;
        relative_parts(&to)?;
        let inode = *self.names.get(&from).ok_or(libc::ENOENT)?;
        let node = self.nodes.get(&inode).ok_or(libc::ESTALE)?;
        if node.save.as_ref() != Some(&to) {
            return Err(libc::EACCES);
        }
        let data = node.pin.try_clone().map_err(errno)?;
        let size = data.metadata().map_err(errno)?.len();
        if size > MAX_FILE {
            return Err(libc::EFBIG);
        }
        let mut bytes = vec![0; size as usize];
        data.read_exact_at(&mut bytes, 0).map_err(errno)?;
        let (old, _) = self.lookup_node(newparent, newname)?;
        let previous = self
            .nodes
            .get(&old)
            .ok_or(libc::ESTALE)?
            .pin
            .try_clone()
            .map_err(errno)?;
        let _authorized = self.selected_write(old, libc::O_WRONLY)?;
        let pinned_parent = {
            let writes = self.shared.writes.lock().map_err(|_| libc::EIO)?;
            let selected = writes
                .iter()
                .find(|write| write.relative == to && same_file(&write.pin, &previous))
                .ok_or(libc::EACCES)?;
            if !same_file(&self.node_file(parent, false)?, &selected.parent) {
                return Err(libc::ESTALE);
            }
            selected.parent.try_clone().map_err(errno)?
        };
        let shared = self.shared.clone();
        let _serial = shared.effects.lock().map_err(|_| libc::EIO)?;
        shared.active_effect.store(true, Ordering::Release);
        let _active = ActiveEffect(&shared.active_effect);
        let revision = mutate_locked(&self.shared, &to, &previous, MutationImage::Replace(&bytes), || {
            materialize(&self.shared, &to, &previous, pinned_parent, &bytes)
        })?;
        let updated = (|| -> Result<(), i32> {
            let published = revision.file().try_clone().map_err(errno)?;
            if !self.shared.metadata_only {
                let mut writes = self.shared.writes.lock().map_err(|_| libc::EIO)?;
                for selected in writes
                    .iter_mut()
                    .filter(|write| write.relative == to && same_file(&write.pin, &previous))
                {
                    selected.pin = published.try_clone().map_err(errno)?;
                }
                #[cfg(test)]
                let mut reads = self.shared.chosen.lock().map_err(|_| libc::EIO)?;
                #[cfg(test)]
                for selected in reads.iter_mut().filter(|read| {
                    !read.directory && read.relative == to && same_file(&read.pin, &previous)
                }) {
                    selected.pin = published.try_clone().map_err(errno)?;
                }
            }
            self.nodes.get_mut(&old).ok_or(libc::ESTALE)?.detached = true;
            if let Some(observed) = self
                .shared
                .observed
                .lock()
                .map_err(|_| libc::EIO)?
                .get_mut(&old)
            {
                observed.linked = false;
            }
            let node = self.nodes.get_mut(&inode).ok_or(libc::ESTALE)?;
            node.relative = to.clone();
            node.pin = published;
            node.save = None;
            self.names.remove(&from);
            self.names.insert(to, inode);
            self.shared
                .saves
                .lock()
                .map_err(|_| libc::EIO)?
                .retain(|save| save.relative != from);
            self.track(inode, parent)?;
            if self.shared.metadata_only {
                super::selected_write::publish_owned_revision(
                    &self.shared, &self.nodes.get(&inode).ok_or(libc::ESTALE)?.relative,
                    &previous, inode, &revision,
                )?;
            }
            Ok(())
        })();
        if updated.is_err() {
            self.shared.provider_failed(
                "native save bookkeeping failed after a durable canonical replacement".into(),
            );
        }
        updated
    }

    fn discard_save(&mut self, parent: u64, name: &OsStr) -> Result<(), i32> {
        let path = self
            .nodes
            .get(&parent)
            .ok_or(libc::ESTALE)?
            .relative
            .join(name);
        let inode = *self.names.get(&path).ok_or(libc::ENOENT)?;
        let node = self.nodes.get_mut(&inode).ok_or(libc::ESTALE)?;
        if node.save.is_none() {
            return Err(libc::EPERM);
        }
        node.detached = true;
        self.names.remove(&path);
        self.shared
            .saves
            .lock()
            .map_err(|_| libc::EIO)?
            .retain(|save| save.relative != path);
        Ok(())
    }

    fn write_file(&self, inode: u64, fh: u64, offset: i64, data: &[u8]) -> Result<(), i32> {
        let Some(Handle::File {
            inode: expected,
            file,
            write: true,
            own_save,
            append,
            ..
        }) = self.handles.get(&fh)
        else {
            return Err(libc::EBADF);
        };
        if *expected != inode {
            return Err(libc::EBADF);
        }
        if data.len() > MAX_READ as usize {
            return Err(libc::EFBIG);
        }
        let node = self.nodes.get(&inode).ok_or(libc::ESTALE)?;
        let target = if node.save.is_some() {
            file.try_clone().map_err(errno)?
        } else {
            self.selected_write(
                inode,
                libc::O_WRONLY | if *append { libc::O_APPEND } else { 0 },
            )?
        };
        let position = if *append {
            target.metadata().map_err(errno)?.len()
        } else {
            u64::try_from(offset).map_err(|_| libc::EINVAL)?
        };
        if position.checked_add(data.len() as u64).ok_or(libc::EFBIG)? > MAX_FILE {
            return Err(libc::EFBIG);
        }
        if node.save.is_some() {
            scratch_growth(&self.shared, &target, position + data.len() as u64)?;
            target.write_all_at(data, position).map_err(errno)?;
            return Ok(());
        }
        if !*own_save && !same_file(file, &target) {
            return Err(libc::ESTALE);
        }
        if *own_save {
            scratch_growth(&self.shared, file, position + data.len() as u64)?;
        }
        mutate(&self.shared, &node.relative, &node.pin, MutationImage::Write { offset: position, bytes: data }, || {
            let _owner = FsIdentityGuard::enter(self.shared.owner).map_err(|_| libc::EACCES)?;
            target.write_all_at(data, position).map_err(errno)?;
            target.sync_all().map_err(errno)?;
            node.pin.try_clone().map_err(errno)
        })?;
        if *own_save && file.write_all_at(data, position).is_err() {
            self.shared
                .provider_failed("native private save mirror failed after a durable write".into());
            return Err(libc::EIO);
        }
        Ok(())
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

    fn open_selected(&mut self, req: &Request<'_>, inode: u64, flags: i32, reply: ReplyOpen) {
        #[cfg(test)]
        let started = Instant::now();
        #[cfg(test)]
        eprintln!("private selected-file timing sourceStep=native_open_begin elapsedMs=0");
        let result = self.authorize(req, false, false).and_then(|_| {
            if flags & libc::O_ACCMODE != libc::O_RDONLY || flags & (libc::O_TRUNC | libc::O_APPEND) != 0 {
                return Err(libc::EACCES);
            }
            if self.handles.len() >= MAX_HANDLES || self.next_handle == u64::MAX { return Err(libc::EMFILE); }
            let provider = super::ReadProvider::new(self.shared.clone()).map_err(|_| libc::EACCES)?;
            let (selection, file) = provider.open(inode, None)?;
            self.add_handle(Handle::SelectedFile { inode, file, selection })
        });
        #[cfg(test)]
        eprintln!(
            "private selected-file timing sourceStep=native_open_end elapsedMs={} ok={}",
            started.elapsed().as_millis(), result.is_ok(),
        );
        match result { Ok(handle) => reply.opened(handle, 0), Err(error) => reply.error(error) }
    }

    fn read_selected(&self, req: &Request<'_>, inode: u64, fh: u64, offset: i64, size: u32,
        reply: ReplyData)
    {
        #[cfg(test)]
        let started = Instant::now();
        #[cfg(test)]
        eprintln!(
            "private selected-file timing sourceStep=native_read_begin elapsedMs=0 offset={offset} requestedBytes={size}",
        );
        let result = self.authorize(req, false, self.handles.contains_key(&fh)).and_then(|_| {
            let Some(Handle::SelectedFile { inode: expected, file, selection }) = self.handles.get(&fh)
                else { return Err(libc::EBADF); };
            if *expected != inode || size > MAX_READ { return Err(libc::EINVAL); }
            let offset = u64::try_from(offset).map_err(|_| libc::EINVAL)?;
            let provider = super::ReadProvider::new(self.shared.clone()).map_err(|_| libc::EACCES)?;
            let (_, current) = {
                #[cfg(test)]
                let permission_started = Instant::now();
                let permission = provider.open(inode, Some(selection));
                #[cfg(test)]
                eprintln!(
                    "private selected-file timing sourceStep=native_read_fresh_permission elapsedMs={} ok={}",
                    permission_started.elapsed().as_millis(), permission.is_ok(),
                );
                permission?
            };
            if !same_file(file, &current) { provider.stop(); return Err(libc::ESTALE); }
            let _owner = FsIdentityGuard::enter(self.shared.owner).map_err(|_| libc::EACCES)?;
            let mut data = vec![0; size as usize];
            let count = file.read_at(&mut data, offset).map_err(errno)?;
            data.truncate(count);
            Ok(data)
        });
        #[cfg(test)]
        eprintln!(
            "private selected-file timing sourceStep=native_read_end elapsedMs={} ok={} readBytes={}",
            started.elapsed().as_millis(), result.is_ok(),
            result.as_ref().map_or(0, |bytes| bytes.len()),
        );
        match result { Ok(data) => reply.data(&data), Err(error) => reply.error(error) }
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
        for (path, id) in &self.names {
            if path.parent()
                == Some(
                    self.nodes
                        .get(&inode)
                        .ok_or(libc::ESTALE)?
                        .relative
                        .as_path(),
                )
                && self
                    .nodes
                    .get(id)
                    .is_some_and(|node| node.save.is_some() && !node.detached)
            {
                entries.push((
                    *id,
                    FileType::RegularFile,
                    path.file_name().ok_or(libc::EINVAL)?.into(),
                ));
            }
        }
        Ok(entries)
    }
}

impl fuser::Filesystem for Filesystem {
    fn init(&mut self, _req: &Request<'_>, config: &mut fuser::KernelConfig) -> Result<(), i32> {
        match config.set_max_write(MAX_READ) {
            Ok(_) => Ok(()),
            Err(nearest) if nearest > 0 && nearest <= MAX_READ => config
                .set_max_write(nearest)
                .map(|_| ())
                .map_err(|_| libc::EINVAL),
            Err(_) => Err(libc::EINVAL),
        }
    }

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
        let opened = fh.is_some_and(|handle| {
            self.handles
                .get(&handle)
                .is_some_and(|handle| handle.inode() == inode)
        });
        match self
            .authorize(req, true, opened)
            .and_then(|_| self.attr(inode))
        {
            Ok(attr) => reply.attr(&TTL, &attr),
            Err(error) => reply.error(error),
        }
    }

    fn statfs(&mut self, req: &Request<'_>, inode: u64, reply: ReplyStatfs) {
        let result = self.authorize(req, true, false).and_then(|_| {
            let _node = self.node_file(inode, false)?;
            let root = self.node_file(1, false)?;
            let _owner = FsIdentityGuard::enter(self.shared.owner).map_err(|_| libc::EACCES)?;
            let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
            if unsafe { libc::fstatvfs(root.as_raw_fd(), &mut stats) } != 0 {
                return Err(errno(std::io::Error::last_os_error()));
            }
            let sizes = (
                u32::try_from(stats.f_bsize).map_err(|_| libc::EOVERFLOW)?,
                u32::try_from(stats.f_namemax).map_err(|_| libc::EOVERFLOW)?,
                u32::try_from(stats.f_frsize).map_err(|_| libc::EOVERFLOW)?,
            );
            drop(_owner);
            self.shared.live()?;
            Ok((stats, sizes))
        });
        match result {
            Ok((stats, (bsize, namelen, frsize))) => reply.statfs(
                stats.f_blocks, stats.f_bfree, stats.f_bavail, stats.f_files, stats.f_ffree,
                bsize, namelen, frsize,
            ),
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
        if self.shared.metadata_only && flags & libc::O_ACCMODE == libc::O_RDONLY
            && self.nodes.get(&inode).is_none_or(|node| node.save.is_none())
        {
            self.open_selected(req, inode, flags, reply);
            return;
        }
        let result = self
            .authorize(req, false, false)
            .and_then(|_| self.handle_file(inode, flags));
        // No passthrough/keep-cache flag. mmap may still use the per-instance
        // page cache; revoke/expiry therefore retires the actual process tree.
        match result {
            Ok(handle) => reply.opened(
                handle,
                if flags & libc::O_ACCMODE != libc::O_RDONLY {
                    fuser::consts::FOPEN_DIRECT_IO
                } else {
                    0
                },
            ),
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
        if matches!(self.handles.get(&fh), Some(Handle::SelectedFile { .. })) {
            self.read_selected(req, inode, fh, offset, size, reply);
            return;
        }
        let result = self
            .authorize(req, false, self.handles.contains_key(&fh))
            .and_then(|_| {
                let offset = u64::try_from(offset).map_err(|_| libc::EINVAL)?;
                if size > MAX_READ {
                    return Err(libc::EOVERFLOW);
                }
                let Some(Handle::File {
                    inode: expected,
                    file,
                    read: true,
                    own_save,
                    ..
                }) = self.handles.get(&fh)
                else {
                    return Err(libc::EBADF);
                };
                if *expected != inode {
                    return Err(libc::ESTALE);
                }
                if !*own_save {
                    let node = self.nodes.get(&inode).ok_or(libc::ESTALE)?;
                    let selected = if self.shared.metadata_only {
                        super::ReadProvider::new(self.shared.clone()).map_err(|_| libc::EACCES)?
                            .read_existing(inode)?.1
                    } else if node.detached {
                        node.pin.try_clone().map_err(errno)?
                    } else {
                        self.readable_file(inode)?
                    };
                    if !same_file(file, &selected) {
                        return Err(libc::ESTALE);
                    }
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
        let released = self.handles.remove(&fh).map(|handle| handle.inode());
        if let Ok(mut polls) = self.shared.polls.lock() {
            polls.remove(&fh);
        }

        if let Some(inode) = released {
            if !self.handles.values().any(|handle| handle.inode() == inode) {
                if let Ok(provider) = super::ReadProvider::new(self.shared.clone()) {
                    provider.release_history(inode);
                }
            }
        }
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

    fn write(
        &mut self,
        req: &Request<'_>,
        inode: u64,
        fh: u64,
        offset: i64,
        data: &[u8],
        write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        let result = self.authorize(req, false, false).and_then(|_| {
            // CACHE means unbound background writeback, which is never enabled.
            // LOCKOWNER (2) and KILL_SUIDGID (4) are ordinary kernel write hints.
            if write_flags & !6 != 0 {
                return Err(libc::EOPNOTSUPP);
            }
            self.write_file(inode, fh, offset, data)
        });
        match result {
            Ok(()) => reply.written(data.len() as u32),
            Err(error) => reply.error(error),
        }
    }

    fn setattr(
        &mut self,
        req: &Request<'_>,
        inode: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        fh: Option<u64>,
        crtime: Option<SystemTime>,
        chgtime: Option<SystemTime>,
        bkuptime: Option<SystemTime>,
        flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        let result = self.authorize(req, false, false).and_then(|_| {
            if mode.is_some()
                || uid.is_some()
                || gid.is_some()
                || crtime.is_some()
                || chgtime.is_some()
                || bkuptime.is_some()
                || flags.is_some()
            {
                return Err(libc::EOPNOTSUPP);
            }
            if let Some(fh) = fh {
                if !matches!(self.handles.get(&fh), Some(Handle::File {
                    inode: expected, write: true, ..
                }) if *expected == inode)
                {
                    return Err(libc::EBADF);
                }
            }
            self.truncate(inode, size.ok_or(libc::EOPNOTSUPP)?, atime, mtime)?;
            self.attr(inode)
        });
        match result {
            Ok(attr) => reply.attr(&TTL, &attr),
            Err(error) => reply.error(error),
        }
    }

    fn create(
        &mut self,
        req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        let result = self.authorize(req, false, false).and_then(|_| {
            if mode & 0o7111 != 0
                || mode & libc::S_IFMT != 0 && mode & libc::S_IFMT != libc::S_IFREG
            {
                return Err(libc::EPERM);
            }
            let (inode, handle) = self.create_save(parent, name, flags)?;
            Ok((self.attr(inode)?, handle))
        });
        match result {
            Ok((attr, handle)) => {
                reply.created(&TTL, &attr, 1, handle, fuser::consts::FOPEN_DIRECT_IO)
            }
            Err(error) => reply.error(error),
        }
    }

    fn rename(
        &mut self,
        req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        flags: u32,
        reply: ReplyEmpty,
    ) {
        match self
            .authorize(req, false, false)
            .and_then(|_| self.save(parent, name, newparent, newname, flags))
        {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error),
        }
    }

    fn unlink(&mut self, req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        match self
            .authorize(req, false, false)
            .and_then(|_| self.discard_save(parent, name))
        {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error),
        }
    }

    fn rmdir(&mut self, _req: &Request<'_>, _parent: u64, _name: &OsStr, reply: ReplyEmpty) {
        reply.error(libc::EPERM);
    }

    fn fsync(
        &mut self,
        req: &Request<'_>,
        inode: u64,
        fh: u64,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        let result = self
            .authorize(req, false, self.handles.contains_key(&fh))
            .and_then(|_| {
                if let Some(Handle::SelectedFile { inode: expected, file, selection }) = self.handles.get(&fh) {
                    if *expected != inode { return Err(libc::EBADF); }
                    let _read = super::ReadProvider::new(self.shared.clone()).map_err(|_| libc::EACCES)?
                        .open(inode, Some(selection))?;
                    return file.sync_all().map_err(errno);
                }
                let Some(Handle::File {
                    inode: expected,
                    file,
                    own_save,
                    ..
                }) = self.handles.get(&fh)
                else {
                    return Err(libc::EBADF);
                };
                if *expected != inode {
                    return Err(libc::EBADF);
                }
                if *own_save {
                    return file.sync_all().map_err(errno);
                }
                if self.shared.metadata_only { let _write = self.selected_write(inode, libc::O_WRONLY)?; }
                let _owner = FsIdentityGuard::enter(self.shared.owner).map_err(|_| libc::EACCES)?;
                file.sync_all().map_err(errno)
            });
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error),
        }
    }

    fn fsyncdir(
        &mut self,
        req: &Request<'_>,
        inode: u64,
        fh: u64,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        let result = self
            .authorize(req, true, self.handles.contains_key(&fh))
            .and_then(|_| {
                if !matches!(self.handles.get(&fh), Some(Handle::Directory { inode: expected, .. })
                if *expected == inode)
                {
                    return Err(libc::EBADF);
                }
                let directory = self.node_file(inode, true)?;
                let _owner = FsIdentityGuard::enter(self.shared.owner).map_err(|_| libc::EACCES)?;
                directory.sync_all().map_err(errno)
            });
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error),
        }
    }

    #[cfg(test)]
    fn poll(
        &mut self,
        req: &Request<'_>,
        inode: u64,
        fh: u64,
        ph: PollHandle,
        events: u32,
        flags: u32,
        reply: ReplyPoll,
    ) {
        let result = self.authorize(req, false, false).and_then(|_| {
            let node = self.nodes.get(&inode).ok_or(libc::ESTALE)?;
            if !self
                .shared
                .watches
                .lock()
                .map_err(|_| libc::EIO)?
                .iter()
                .any(|watch| watch.relative == node.relative && same_file(&watch.pin, &node.pin))
            {
                return Err(libc::EACCES);
            }
            let Some(Handle::File {
                inode: expected,
                read: true,
                watch_version,
                ..
            }) = self.handles.get_mut(&fh)
            else {
                return Err(libc::EBADF);
            };
            if *expected != inode {
                return Err(libc::EBADF);
            }
            let mut polls = self.shared.polls.lock().map_err(|_| libc::EIO)?;
            let version = self
                .shared
                .observed
                .lock()
                .map_err(|_| libc::EIO)?
                .get(&inode)
                .map(|node| node.version)
                .ok_or(libc::ESTALE)?;
            if version != *watch_version {
                *watch_version = version;
                polls.remove(&fh);
                return Ok(libc::POLLPRI as u32);
            }
            if events & libc::POLLPRI as u32 != 0 && flags & 1 != 0 {
                polls.insert(fh, Subscription { inode, handle: ph });
            }
            Ok(0)
        });
        match result {
            Ok(events) => reply.poll(events),
            Err(error) => reply.error(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_revision_evidence_rejects_interleaved_postimage_and_old_version_bytes() {
        std::fs::create_dir_all("build").unwrap();
        let directory = tempfile::Builder::new()
            .prefix("native-postimage-")
            .tempdir_in("build")
            .unwrap();
        let owner = unsafe { libc::geteuid() };
        let file = tempfile::NamedTempFile::new_in(directory.path()).unwrap();
        let previous = b"before\0binary-base".to_vec();
        file.as_file().write_all_at(&previous, 0).unwrap();
        let expected = MutationImage::Write {
            offset: 2,
            bytes: b"OWN",
        }
        .expected(&previous)
        .unwrap();
        file.as_file().write_all_at(b"OWN", 2).unwrap();
        file.as_file().sync_all().unwrap();
        assert!(verify_mutation(
            owner,
            file.as_file().try_clone().unwrap(),
            previous.clone(),
            file.as_file().try_clone().unwrap(),
            expected.clone(),
            false,
        )
        .is_ok());

        let external = file.as_file().try_clone().unwrap();
        external.write_all_at(b"unexpected", 6).unwrap();
        external.sync_all().unwrap();
        assert!(
            verify_mutation(
                owner,
                file.as_file().try_clone().unwrap(),
                previous,
                file.as_file().try_clone().unwrap(),
                expected,
                false,
            )
            .is_err(),
            "a late fingerprint must not turn external bytes into an own revision"
        );

        let old = tempfile::NamedTempFile::new_in(directory.path()).unwrap();
        let new = tempfile::NamedTempFile::new_in(directory.path()).unwrap();
        let before = b"old selected bytes\0\xff".to_vec();
        let after = b"new reserved bytes\0\xfe".to_vec();
        old.as_file().write_all_at(&before, 0).unwrap();
        new.as_file().write_all_at(&after, 0).unwrap();
        new.as_file().sync_all().unwrap();
        let held = old.as_file().try_clone().unwrap();
        std::fs::rename(new.path(), old.path()).unwrap();
        let clean = verify_mutation(
            owner,
            held.try_clone().unwrap(),
            before.clone(),
            new.as_file().try_clone().unwrap(),
            after.clone(),
            true,
        )
        .unwrap();
        assert!(clean.old_version(owner, &held).is_ok());
        held.write_all_at(b"external", 0).unwrap();
        held.sync_all().unwrap();
        assert!(
            clean.old_version(owner, &held).is_err(),
            "a held old FD must retain only its evidenced preimage, not a late arbitrary stamp"
        );
        assert!(verify_mutation(
            owner,
            held,
            before,
            new.as_file().try_clone().unwrap(),
            after,
            true,
        )
        .is_err());
    }
}
