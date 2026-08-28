//! One-time migration of legacy App state into the per-App partition.
//!
//! Before workers were isolated, every bundled App wrote into the
//! *owner's* data root: `calendar/events.db`, `kv.json`, `logs/`, a
//! gateway's `apps/<id>/state.json`, and so on. `COS_DATA_DIR` now
//! names the App's own partition of that root, so those paths would
//! silently resolve somewhere new and the existing state would look
//! like it had vanished.
//!
//! This module moves it, once, before the first sandboxed launch.
//!
//! ## What may be moved
//!
//! Only what [`LEGACY_APP_STATE`] names. It is a fixed table written
//! by this repository, keyed by App id, and holding relative paths
//! beneath the owner data root — a manifest cannot add to it, an
//! argument cannot select from it, and nothing here ever looks at a
//! caller-supplied path. Two of the directories are shared with the
//! kernel (`proc` holds the session registry, `apps/` holds every
//! App's partition), so those entries name individual files rather
//! than the directory around them.
//!
//! ## How it moves
//!
//! Every component is opened `O_NOFOLLOW` from a directory descriptor
//! rooted at the owner data root, so no symlink is followed and no
//! resolution escapes it. A source must be a directory or a single-link
//! regular file owned by the account the launcher runs as; a symlink,
//! socket, FIFO, device or hardlinked file is refused rather than
//! moved. The move itself is `renameat` between two descriptors on the
//! same filesystem, which is atomic and needs no copy; a source on a
//! different filesystem is reported, not copied, and left untouched.
//!
//! A destination that already exists is never merged: an empty
//! directory is replaced, anything else fails with the two paths named
//! so the owner can resolve it. The version marker is written durably
//! only after every entry has been moved, so a crash halfway through
//! leaves the remaining sources in place and the next launch simply
//! moves what is left. Nothing is copied to a temporary location, so
//! there is no partial state to recover.

use std::path::Path;

/// One piece of legacy state.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Legacy {
    /// A whole directory beneath the owner data root, owned by the App.
    Dir(&'static str),
    /// A single regular file beneath the owner data root.
    File(&'static str),
    /// Named files inside a directory the App does *not* own. The
    /// directory itself stays where it is; only these names move.
    FilesIn {
        dir: &'static str,
        names: &'static [&'static str],
        /// Names beginning with one of these also move, which is how
        /// `exec` reclaims its `stdout.<pid>` captures without
        /// touching the session registry beside them.
        prefixes: &'static [&'static str],
    },
}

/// Every bundled App that read or wrote the owner data root directly,
/// with the exact relative paths it used.
///
/// Derived from the `COS_DATA_DIR` uses in `apps/`; adapters take no
/// data directory. Kernel-owned state — `sessions/`, `agent/`,
/// `journal/`, `approvals/`, `clawd/`, `credentials/` and the session
/// registry inside `proc/` — is deliberately absent: it is not App
/// state, it stays outside every partition, and the paths that need it
/// go through the launch broker instead.
pub(crate) const LEGACY_APP_STATE: &[(&str, &[Legacy])] = &[
    ("calendar", &[Legacy::Dir("calendar")]),
    ("db", &[Legacy::Dir("db")]),
    ("fs", &[Legacy::Dir("trash")]),
    ("kv", &[Legacy::File("kv.json")]),
    ("launcher", &[Legacy::Dir("launcher")]),
    ("log", &[Legacy::Dir("logs")]),
    ("notify", &[Legacy::File("notifications.json")]),
    (
        "exec",
        &[Legacy::FilesIn {
            dir: "proc",
            names: &[],
            // Only this App's captured output. `proc/registry.json` is
            // deliberately absent: that name belongs to the *kernel*
            // session and capability registry
            // (`crate::proc::registry_path`), which lives in the same
            // directory and must never move or enter a partition. The
            // App wrote a file of the same name there too, and the
            // partition is what finally separates the two — the App's
            // registry starts empty inside its own `proc/` and the
            // kernel's stays exactly where it is.
            prefixes: &["stdout.", "stderr.", ".stdout.", ".stderr."],
        }],
    ),
    (
        "gateway-discord",
        &[Legacy::FilesIn {
            dir: "apps/gateway-discord",
            names: &["state.json", "config.json", "gateway.pid", "stop.request"],
            prefixes: &[],
        }],
    ),
    (
        "gateway-telegram",
        &[Legacy::FilesIn {
            dir: "apps/gateway-telegram",
            names: &["state.json", "gateway.pid"],
            prefixes: &[],
        }],
    ),
];

/// Marker recording that this partition has been brought forward.
const MARKER: &str = ".cos-state-version";

/// Roots beneath the owner data root that hold kernel state. No table
/// entry may name one, or anything inside one, as a whole directory:
/// they are not App state, and an App that could read them would have
/// the session registry, the journal or the credential store.
///
/// `proc` and `apps` are on the list *and* appear in the table, which
/// is exactly why [`protected`] works on the effective source path of
/// every entry rather than on its shape: those two are only ever
/// entered by file name.
const SHARED_KERNEL_ROOTS: &[&str] = &[
    "agent",
    "apps",
    "approvals",
    "clawd",
    "credentials",
    "journal",
    "models",
    "proc",
    "sessions",
    "users",
];

/// Individual paths that must never move, whatever names them.
///
/// `proc/registry.json` is the kernel session and capability registry
/// (`crate::proc::registry_path`), with its `filelock` sentinel and
/// rename staging file beside it. The marker names are this module's
/// own bookkeeping: moving one over another would make a finished
/// migration look unfinished, or the reverse.
const PROTECTED_PATHS: &[&str] = &[
    "proc/registry.json",
    "proc/registry.json.lock",
    "proc/registry.tmp",
];

/// File names that must never be selected inside *any* directory,
/// including by a prefix.
const PROTECTED_NAMES: &[&str] = &[
    "registry.json",
    "registry.json.lock",
    "registry.tmp",
    "meta.json",
    MARKER,
    ".cos-state-version.new",
];

/// Bump when [`LEGACY_APP_STATE`] gains entries an existing partition
/// still has to collect.
const CURRENT_VERSION: u32 = 1;

/// Largest number of entries a shared directory is scanned for.
const MAX_SCANNED_ENTRIES: usize = 4096;

/// Would moving `relative` take kernel state with it?
///
/// Answered on the *effective source path* — the concrete thing that
/// would be renamed — so a whole directory and one file inside a shared
/// directory are judged by the same rule. `whole` says whether the
/// entry claims the path and everything under it, which is the only
/// case where owning a shared root matters.
fn protected(relative: &str, whole: bool) -> Option<String> {
    let trimmed = relative.trim_matches('/');
    if PROTECTED_PATHS.contains(&trimmed) {
        return Some(format!("`{trimmed}` is kernel state"));
    }
    if let Some(name) = trimmed.rsplit('/').next() {
        if PROTECTED_NAMES.contains(&name) {
            return Some(format!("`{name}` is kernel or migration bookkeeping"));
        }
    }
    if whole {
        if let Some(first) = trimmed.split('/').next() {
            if SHARED_KERNEL_ROOTS.contains(&first) {
                return Some(format!("`{first}` is a shared kernel root"));
            }
        }
    }
    None
}

/// Every concrete relative path an entry would rename, paired with
/// whether it claims a whole subtree.
fn effective_sources(entry: &Legacy) -> Vec<(String, bool)> {
    match entry {
        Legacy::Dir(path) | Legacy::File(path) => vec![((*path).to_string(), true)],
        Legacy::FilesIn { dir, names, .. } => names
            .iter()
            .map(|name| (format!("{dir}/{name}"), false))
            .collect(),
    }
}

/// Prefixes are checked separately: they select by name at launch, so
/// the question is whether any protected name could ever match one.
fn protected_prefix(entry: &Legacy) -> Option<String> {
    let Legacy::FilesIn { prefixes, .. } = entry else {
        return None;
    };
    for prefix in *prefixes {
        if prefix.is_empty() {
            return Some("an empty prefix selects everything".to_string());
        }
        if let Some(name) = PROTECTED_NAMES
            .iter()
            .find(|name| name.starts_with(*prefix))
        {
            return Some(format!("prefix `{prefix}` would select `{name}`"));
        }
    }
    None
}

/// Refuse a table entry that would move kernel state.
///
/// The unit tests assert this over the whole table, and it runs again
/// here so a future edit fails the launch rather than the review.
fn check_entry(app_id: &str, entry: &Legacy) -> Result<(), String> {
    for (relative, whole) in effective_sources(entry) {
        if let Some(reason) = protected(&relative, whole) {
            return Err(format!(
                "the `{app_id}` legacy state table names `{relative}`: {reason}"
            ));
        }
    }
    if let Some(reason) = protected_prefix(entry) {
        return Err(format!("the `{app_id}` legacy state table {reason}"));
    }
    Ok(())
}

/// Bring `app_id`'s legacy state into `partition`, once.
///
/// `owner_root` is the launcher's own data root and `partition` is
/// `<owner_root>/apps/<app_id>`, already created and verified by the
/// caller. Returns `Ok(())` when there is nothing to do, which is the
/// normal case after the first launch.
pub(crate) fn migrate_legacy_state(
    owner_root: &Path,
    partition: &Path,
    app_id: &str,
) -> Result<(), String> {
    #[cfg(unix)]
    {
        let Some(entries) = LEGACY_APP_STATE
            .iter()
            .find(|(id, _)| *id == app_id)
            .map(|(_, entries)| *entries)
        else {
            return Ok(());
        };
        if marker_version(partition) >= CURRENT_VERSION {
            return Ok(());
        }
        for entry in entries {
            check_entry(app_id, entry)?;
        }
        unix::migrate(owner_root, partition, app_id, entries)?;
        write_marker(partition)
    }
    #[cfg(not(unix))]
    {
        let _ = (owner_root, partition, app_id);
        Ok(())
    }
}

fn marker_version(partition: &Path) -> u32 {
    std::fs::read_to_string(partition.join(MARKER))
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok())
        .unwrap_or(0)
}

/// Record the version durably: a marker that survived only in the page
/// cache would re-run the migration after a crash, and the second run
/// must not be the one that decides a destination is a collision.
fn write_marker(partition: &Path) -> Result<(), String> {
    let temp = partition.join(format!("{MARKER}.new"));
    let final_path = partition.join(MARKER);
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&temp)
            .map_err(|error| format!("record App state version: {error}"))?;
        file.write_all(CURRENT_VERSION.to_string().as_bytes())
            .map_err(|error| format!("record App state version: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("record App state version: {error}"))?;
    }
    std::fs::rename(&temp, &final_path)
        .map_err(|error| format!("record App state version: {error}"))?;
    if let Ok(dir) = std::fs::File::open(partition) {
        let _ = dir.sync_all();
    }
    Ok(())
}

#[cfg(unix)]
mod unix {
    use super::{Legacy, MAX_SCANNED_ENTRIES};
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::{AsRawFd, OwnedFd};
    use std::path::Path;

    pub(super) fn migrate(
        owner_root: &Path,
        partition: &Path,
        app_id: &str,
        entries: &[Legacy],
    ) -> Result<(), String> {
        let root = open_dir(owner_root)?;
        let owner = owner_uid(&root, owner_root)?;
        let target = open_dir(partition)?;
        for entry in entries {
            match *entry {
                Legacy::Dir(relative) => {
                    move_named(&root, &target, relative, owner, Kind::Dir, app_id)?;
                }
                Legacy::File(relative) => {
                    move_named(&root, &target, relative, owner, Kind::File, app_id)?;
                }
                Legacy::FilesIn {
                    dir,
                    names,
                    prefixes,
                } => {
                    let Some(source_dir) = open_relative(&root, dir)? else {
                        continue;
                    };
                    // The shared directory is enumerated once and only
                    // the App's own names are taken from it, so the
                    // session registry beside them never moves. Each
                    // selected name is checked again against the
                    // protected set: what a prefix matches depends on
                    // what is on disk, not on the table alone.
                    for name in scan(&source_dir, dir, names, prefixes)? {
                        let relative = format!("{dir}/{name}");
                        if let Some(reason) = super::protected(&relative, false) {
                            return Err(format!(
                                "refusing to move `{relative}` for `{app_id}`: {reason}"
                            ));
                        }
                        move_named(&root, &target, &relative, owner, Kind::File, app_id)?;
                    }
                }
            }
        }
        Ok(())
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Kind {
        Dir,
        File,
    }

    /// Move one relative path from the owner root into the partition,
    /// creating the same relative shape below it.
    fn move_named(
        root: &OwnedFd,
        target: &OwnedFd,
        relative: &str,
        owner: u32,
        kind: Kind,
        app_id: &str,
    ) -> Result<(), String> {
        let (parent_rel, name) = split(relative)?;
        let Some(source_parent) = open_relative(root, parent_rel)? else {
            return Ok(());
        };
        let Some(stat) = stat_at(&source_parent, name)? else {
            return Ok(());
        };
        let mode = stat.st_mode & libc::S_IFMT;
        if mode == libc::S_IFLNK {
            return Err(format!(
                "legacy App state `{relative}` is a symlink; \
                 move it into the `{app_id}` App data directory by hand"
            ));
        }
        match kind {
            Kind::Dir if mode != libc::S_IFDIR => {
                return Err(format!("legacy App state `{relative}` is not a directory"))
            }
            Kind::File if mode != libc::S_IFREG => {
                return Err(format!(
                    "legacy App state `{relative}` is not a regular file"
                ))
            }
            _ => {}
        }
        if stat.st_uid != owner {
            return Err(format!(
                "legacy App state `{relative}` belongs to uid {} rather than {owner}",
                stat.st_uid
            ));
        }
        if kind == Kind::File && stat.st_nlink != 1 {
            return Err(format!(
                "legacy App state `{relative}` has {} hard links; \
                 move it into the `{app_id}` App data directory by hand",
                stat.st_nlink
            ));
        }

        let destination_parent = make_relative(target, parent_rel)?;
        if let Some(existing) = stat_at(&destination_parent, name)? {
            if existing.st_mode & libc::S_IFMT == libc::S_IFDIR
                && is_empty(&destination_parent, name)?
            {
                remove_empty_dir(&destination_parent, name, relative)?;
            } else {
                return Err(format!(
                    "the `{app_id}` App already has `{relative}` in its data directory \
                     and legacy state of the same name is still in the owner data root; \
                     keep one and remove the other"
                ));
            }
        }

        rename_at(&source_parent, name, &destination_parent, name, relative)?;
        sync_dir(&destination_parent);
        sync_dir(&source_parent);
        Ok(())
    }

    fn split(relative: &str) -> Result<(&str, &str), String> {
        let trimmed = relative.trim_matches('/');
        if trimmed.is_empty()
            || trimmed
                .split('/')
                .any(|part| part.is_empty() || part == "..")
        {
            return Err(format!(
                "legacy App state path `{relative}` is not relative"
            ));
        }
        Ok(match trimmed.rsplit_once('/') {
            Some((parent, name)) => (parent, name),
            None => ("", trimmed),
        })
    }

    #[cfg(test)]
    pub(super) fn test_split(relative: &str) -> Result<(&str, &str), String> {
        if relative.starts_with('/') {
            return Err("legacy App state path is absolute".to_string());
        }
        split(relative)
    }

    fn open_dir(path: &Path) -> Result<OwnedFd, String> {
        let c_path = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| "App data root path contains a NUL byte".to_string())?;
        let fd = unsafe {
            libc::open(
                c_path.as_ptr(),
                libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_RDONLY,
            )
        };
        if fd < 0 {
            return Err(format!(
                "open App data root `{}`: {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
        Ok(unsafe { <OwnedFd as std::os::unix::io::FromRawFd>::from_raw_fd(fd) })
    }

    fn owner_uid(root: &OwnedFd, path: &Path) -> Result<u32, String> {
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(root.as_raw_fd(), &mut stat) } != 0 {
            return Err(format!(
                "inspect App data root `{}`: {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
        let effective = unsafe { libc::geteuid() };
        if stat.st_uid != effective {
            return Err(format!(
                "App data root `{}` belongs to uid {} rather than {effective}",
                path.display(),
                stat.st_uid
            ));
        }
        Ok(effective)
    }

    /// Walk `relative` from `base`, never following a symlink. `None`
    /// means a component does not exist, which is the ordinary "nothing
    /// to migrate" answer.
    fn open_relative(base: &OwnedFd, relative: &str) -> Result<Option<OwnedFd>, String> {
        let mut current = dup(base)?;
        for part in relative.split('/').filter(|part| !part.is_empty()) {
            let name = CString::new(part)
                .map_err(|_| "legacy App state path contains a NUL byte".to_string())?;
            let fd = unsafe {
                libc::openat(
                    current.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_RDONLY,
                )
            };
            if fd < 0 {
                let error = std::io::Error::last_os_error();
                return match error.raw_os_error() {
                    Some(libc::ENOENT) | Some(libc::ENOTDIR) | Some(libc::ELOOP) => Ok(None),
                    _ => Err(format!("open legacy App state `{relative}`: {error}")),
                };
            }
            current = unsafe { <OwnedFd as std::os::unix::io::FromRawFd>::from_raw_fd(fd) };
        }
        Ok(Some(current))
    }

    /// Same walk, but creating each `0700` directory that is missing.
    fn make_relative(base: &OwnedFd, relative: &str) -> Result<OwnedFd, String> {
        let mut current = dup(base)?;
        for part in relative.split('/').filter(|part| !part.is_empty()) {
            let name =
                CString::new(part).map_err(|_| "App data path contains a NUL byte".to_string())?;
            if unsafe { libc::mkdirat(current.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::EEXIST) {
                    return Err(format!("create App data directory `{relative}`: {error}"));
                }
            }
            let fd = unsafe {
                libc::openat(
                    current.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_RDONLY,
                )
            };
            if fd < 0 {
                return Err(format!(
                    "open App data directory `{relative}`: {}",
                    std::io::Error::last_os_error()
                ));
            }
            current = unsafe { <OwnedFd as std::os::unix::io::FromRawFd>::from_raw_fd(fd) };
        }
        Ok(current)
    }

    fn dup(fd: &OwnedFd) -> Result<OwnedFd, String> {
        fd.try_clone()
            .map_err(|error| format!("duplicate directory descriptor: {error}"))
    }

    fn stat_at(parent: &OwnedFd, name: &str) -> Result<Option<libc::stat>, String> {
        let c_name = CString::new(name).map_err(|_| "path contains a NUL byte".to_string())?;
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        let seen = unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                c_name.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if seen != 0 {
            let error = std::io::Error::last_os_error();
            return match error.raw_os_error() {
                Some(libc::ENOENT) | Some(libc::ENOTDIR) => Ok(None),
                _ => Err(format!("inspect `{name}`: {error}")),
            };
        }
        Ok(Some(stat))
    }

    fn is_empty(parent: &OwnedFd, name: &str) -> Result<bool, String> {
        let Some(dir) = open_relative(parent, name)? else {
            return Ok(false);
        };
        let mut listing = read_dir(&dir, name)?;
        Ok(listing.next().is_none())
    }

    fn remove_empty_dir(parent: &OwnedFd, name: &str, relative: &str) -> Result<(), String> {
        let c_name = CString::new(name).map_err(|_| "path contains a NUL byte".to_string())?;
        if unsafe { libc::unlinkat(parent.as_raw_fd(), c_name.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
            return Err(format!(
                "replace empty App data directory `{relative}`: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    fn rename_at(
        source_parent: &OwnedFd,
        source_name: &str,
        destination_parent: &OwnedFd,
        destination_name: &str,
        relative: &str,
    ) -> Result<(), String> {
        let from = CString::new(source_name).map_err(|_| "path contains a NUL byte".to_string())?;
        let to =
            CString::new(destination_name).map_err(|_| "path contains a NUL byte".to_string())?;
        if unsafe {
            libc::renameat(
                source_parent.as_raw_fd(),
                from.as_ptr(),
                destination_parent.as_raw_fd(),
                to.as_ptr(),
            )
        } != 0
        {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EXDEV) {
                return Err(format!(
                    "legacy App state `{relative}` is on a different filesystem \
                     from the App data directory; move it across by hand, \
                     nothing has been changed"
                ));
            }
            return Err(format!("move legacy App state `{relative}`: {error}"));
        }
        Ok(())
    }

    fn sync_dir(dir: &OwnedFd) {
        unsafe { libc::fsync(dir.as_raw_fd()) };
    }

    /// Names in a shared directory that belong to this App.
    fn scan(
        dir: &OwnedFd,
        label: &str,
        names: &[&str],
        prefixes: &[&str],
    ) -> Result<Vec<String>, String> {
        let mut taken = Vec::new();
        let mut seen = 0_usize;
        for entry in read_dir(dir, label)? {
            seen += 1;
            if seen > MAX_SCANNED_ENTRIES {
                return Err(format!(
                    "legacy App state directory `{label}` holds more than \
                     {MAX_SCANNED_ENTRIES} entries; move the App's files by hand"
                ));
            }
            let name = entry;
            let matches = names.contains(&name.as_str())
                || prefixes.iter().any(|prefix| name.starts_with(prefix));
            if matches {
                taken.push(name);
            }
        }
        taken.sort();
        Ok(taken)
    }

    /// `readdir` over a directory descriptor, without reopening it by
    /// path: the descriptor is the identity that was validated.
    fn read_dir(dir: &OwnedFd, label: &str) -> Result<std::vec::IntoIter<String>, String> {
        let copy = dup(dir)?;
        let raw = <OwnedFd as std::os::unix::io::IntoRawFd>::into_raw_fd(copy);
        let handle = unsafe { libc::fdopendir(raw) };
        if handle.is_null() {
            let error = std::io::Error::last_os_error();
            unsafe { libc::close(raw) };
            return Err(format!("read legacy App state `{label}`: {error}"));
        }
        let mut names = Vec::new();
        loop {
            let entry = unsafe { libc::readdir(handle) };
            if entry.is_null() {
                break;
            }
            let raw_name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
            let name = raw_name.to_string_lossy().into_owned();
            if name == "." || name == ".." {
                continue;
            }
            names.push(name);
        }
        unsafe { libc::closedir(handle) };
        Ok(names.into_iter())
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/worker/migrate.rs"
    ));
}
