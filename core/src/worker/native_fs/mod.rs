//! Internal native resource kernel provider. Not an App mount API.

use std::collections::BTreeMap;
use std::ffi::CString;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use claw_display_control::{workload, ProcessIdentity};

use crate::clawd::client_identity::FsIdentityGuard;

mod backend;
mod selected_read;
mod selected_write;
pub(crate) use selected_read::{FileCandidate, ReadLeaseCheck, ReadPermit, ReadProvider};
pub(crate) use selected_write::{WriteLeaseCheck, WritePermit};

struct Binding {
    process: workload::InstanceProcess,
    group: OwnedFd,
}

#[cfg(test)]
struct ChosenRead {
    relative: PathBuf,
    pin: File,
    directory: bool,
}

struct ChosenWrite {
    relative: PathBuf,
    pin: File,
    parent: File,
    permission: Option<selected_write::WriteLease>,
}

struct SaveTarget {
    relative: PathBuf,
    target: PathBuf,
}

struct BackingStage {
    parent: File,
    name: OsString,
    file: File,
    published: bool,
}

struct Expiry {
    deadline: Instant,
    ceiling: Instant,
}

struct Shared {
    owner: u32,
    controller: ProcessIdentity,
    root: File,
    root_path: PathBuf,
    owner_home: PathBuf,
    expiry: Mutex<Expiry>,
    metadata_only: bool,
    binding: Mutex<Option<Binding>>,
    #[cfg(test)]
    chosen: Mutex<Vec<ChosenRead>>,
    writes: Mutex<Vec<ChosenWrite>>,
    saves: Mutex<Vec<SaveTarget>>,
    #[cfg(test)]
    watches: Mutex<Vec<ChosenRead>>,
    session: OnceLock<crate::session::SessionId>,
    effects: Mutex<()>,
    stages: Mutex<Vec<BackingStage>>,
    active_effect: AtomicBool,
    own_invalidations: Mutex<BTreeMap<u64, bool>>,
    observed: Mutex<BTreeMap<u64, backend::Observed>>,
    polls: Mutex<BTreeMap<u64, backend::Subscription>>,
    notifier: Mutex<Option<fuser::Notifier>>,
    selected_reads: Mutex<BTreeMap<String, selected_read::GrantedRead>>,
    selected_expiry: Mutex<Option<Instant>>,
    revoked: AtomicBool,
    ended: AtomicBool,
    monitor_stop: AtomicBool,
    cleanup_error: Mutex<Option<String>>,
    #[cfg(test)]
    fail_worker: AtomicBool,
    #[cfg(test)]
    fail_completion: AtomicBool,
    scratch_bytes: AtomicU64,
    inverse_bytes: AtomicU64,
    mutation_attempts: AtomicU64,
}

impl Shared {
    fn controller(&self) -> Result<(), String> {
        if unsafe { libc::geteuid() } != 0
            || ProcessIdentity::current().map_err(|error| error.to_string())? != self.controller
        {
            return Err("native filesystem control requires its retained Root controller".into());
        }
        Ok(())
    }

    fn live(&self) -> Result<(), i32> {
        if self.selected_expiry.lock().map_err(|_| libc::EIO)?
            .is_some_and(|deadline| Instant::now() >= deadline) {
            self.revoked.store(true, Ordering::Release);
        }
        if self.revoked.load(Ordering::Acquire)
            || Instant::now() >= self.expiry.lock().map_err(|_| libc::EIO)?.deadline
        {
            return Err(libc::EACCES);
        }
        Ok(())
    }

    fn bound_selection(&self, deadline: Instant) -> Result<(), String> {
        let mut selected = self.selected_expiry.lock().map_err(|_| "native selection expiry poisoned")?;
        *selected = Some(selected.map_or(deadline, |current| current.min(deadline)));
        Ok(())
    }

    fn retire_processes(&self, deadline: Instant) -> Result<(), String> {
        let group = self
            .binding
            .lock()
            .map_err(|_| "native filesystem binding poisoned")?
            .as_ref()
            .map(|binding| binding.group.try_clone())
            .transpose()
            .map_err(|error| error.to_string())?;
        if let Some(group) = group {
            workload::retire_cgroup(group.as_fd(), deadline).map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn instance_live(&self) -> Result<bool, String> {
        self.binding
            .lock()
            .map_err(|_| "native filesystem binding poisoned")?
            .as_ref()
            .map(|binding| binding.process.active().map_err(|error| error.to_string()))
            .unwrap_or(Ok(true))
    }

    fn session_context<T>(&self, run: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        crate::paths::RoutedPathContext::for_owner(self.owner, self.owner_home.clone())
            .scope_sync(run)
    }

    fn validate_session(&self) -> Result<(), String> {
        let sid = self
            .session
            .get()
            .ok_or("native filesystem view is read-only")?;
        self.session_context(|| {
            let meta = crate::session::get_meta(sid).map_err(|error| error.to_string())?;
            if !crate::session::record_is_root_owned(sid)
                || meta.owner_uid != Some(self.owner)
                || matches!(
                    meta.status,
                    crate::session::Status::Done | crate::session::Status::Failed
                )
            {
                return Err(
                    "native write evidence requires its retained Root-owned owner session".into(),
                );
            }
            if !crate::session::journal::unresolved_mutations(
                &crate::session::journal::Partition::Session(sid.clone()),
                self.owner,
            )
            .map_err(|error| error.to_string())?
            .is_empty()
            {
                return Err("native write session has unresolved canonical mutations".into());
            }
            Ok(())
        })
    }

    fn indeterminate(&self, detail: String) {
        if let Ok(mut error) = self.cleanup_error.lock() {
            *error = Some(format!(
                "native mutation indeterminate; canonical journal recovery required: {detail}"
            ));
        }
        self.revoked.store(true, Ordering::Release);
    }

    fn provider_failed(&self, detail: String) {
        if let Ok(mut error) = self.cleanup_error.lock() {
            *error = Some(detail);
        }
        self.revoked.store(true, Ordering::Release);
    }
}

/// A Root controller's per-window metadata consent and bounded read view.
///
/// The internal caller, not a FUSE request or App-supplied JSON, attests consent
/// and chooses resources. Production exposes only the metadata-only constructor;
/// selected content and mutation entry points remain test-build-only.
pub(crate) struct NativeFsView {
    shared: Arc<Shared>,
    mountpoint: PathBuf,
    mount_id: u64,
    mounted: bool,
    worker: Option<JoinHandle<std::io::Result<()>>>,
    monitor: Option<JoinHandle<()>>,
    watcher: Option<JoinHandle<()>>,
}

static PENDING: OnceLock<Mutex<Vec<NativeFsView>>> = OnceLock::new();

impl NativeFsView {
    #[cfg(test)]
    pub(crate) fn create(
        owner: u32,
        browse_root: &Path,
        mountpoint: &Path,
        lifetime: Duration,
    ) -> Result<Self, String> {
        Self::create_inner(owner, browse_root, mountpoint, lifetime, false, None)
    }

    #[cfg(test)]
    pub(crate) fn create_writable(
        owner: u32,
        browse_root: &Path,
        mountpoint: &Path,
        lifetime: Duration,
        session: &crate::session::SessionId,
    ) -> Result<Self, String> {
        Self::create_inner(
            owner,
            browse_root,
            mountpoint,
            lifetime,
            false,
            Some(session.clone()),
        )
    }

    pub(crate) fn create_home_metadata(
        owner: u32,
        home: &Path,
        mountpoint: &Path,
        identity: (u64, u64),
        ceiling: Instant,
    ) -> Result<Self, String> {
        let authenticated = crate::paths::verified_home_for_uid(owner)?
            .canonicalize()
            .map_err(|error| error.to_string())?;
        if authenticated != home {
            return Err("metadata source differs from authenticated owner Home".into());
        }
        let lifetime = ceiling
            .saturating_duration_since(Instant::now())
            .min(Duration::from_secs(10));
        if lifetime < Duration::from_millis(250) {
            return Err("Home metadata consent is expiring".into());
        }
        let view = Self::create_inner(owner, home, mountpoint, lifetime, true, None)?;
        let actual = view
            .shared
            .root
            .metadata()
            .map_err(|error| error.to_string())?;
        if (actual.dev(), actual.ino()) != identity {
            return Err("authenticated Home identity changed after window consent".into());
        }
        view.shared
            .expiry
            .lock()
            .map_err(|_| "native metadata expiry poisoned")?
            .ceiling = ceiling;
        Ok(view)
    }

    pub(crate) fn renew_home_metadata(&self, lifetime: Duration) -> Result<(), String> {
        self.shared.controller()?;
        self.shared
            .live()
            .map_err(|_| "Home metadata lease ended; fresh registration is required")?;
        let bound = self
            .shared
            .binding
            .lock()
            .map_err(|_| "native binding poisoned")?
            .is_some();
        if !self.shared.metadata_only || !bound || !self.shared.instance_live()? {
            return Err("metadata renewal requires its exact bound GUI instance".into());
        }
        let mut expiry = self
            .shared
            .expiry
            .lock()
            .map_err(|_| "native metadata expiry poisoned")?;
        let now = Instant::now();
        let deadline = now + lifetime.min(Duration::from_secs(2));
        if deadline.saturating_duration_since(now) < Duration::from_millis(250)
            || expiry.ceiling.saturating_duration_since(now) < Duration::from_millis(250)
            || now >= expiry.deadline
        {
            return Err("Home metadata authority is expiring".into());
        }
        expiry.deadline = deadline.min(expiry.ceiling);
        Ok(())
    }

    pub(crate) fn retire_pending_metadata(deadline: Instant) -> Result<(), String> {
        if unsafe { libc::geteuid() } != 0 {
            return Err("native metadata retirement requires Root".into());
        }
        Self::retry_pending(deadline)
    }

    fn create_inner(
        owner: u32,
        browse_root: &Path,
        mountpoint: &Path,
        lifetime: Duration,
        metadata_only: bool,
        session: Option<crate::session::SessionId>,
    ) -> Result<Self, String> {
        if unsafe { libc::geteuid() } != 0
            || owner == 0
            || lifetime.is_zero()
            || lifetime > Duration::from_secs(300)
        {
            return Err(
                "native filesystem prototype requires Root, a normal owner and a bounded lease"
                    .into(),
            );
        }
        #[cfg(not(test))]
        if !metadata_only || session.is_some() {
            return Err("production native resources require the consented Home view, without write authority".into());
        }
        let writable = session.is_some() || metadata_only;
        if metadata_only && session.is_some() {
            return Err("metadata views cannot enable writes".into());
        }
        Self::retry_pending(Instant::now() + Duration::from_secs(3))?;
        let controller = ProcessIdentity::current().map_err(|error| error.to_string())?;
        let owner_home = crate::paths::verified_home_for_uid(owner)?;
        let root_path = browse_root
            .canonicalize()
            .map_err(|error| error.to_string())?;
        crate::worker::derive::reject_forbidden_for_owner(&root_path, &owner_home)?;
        let root = {
            let _identity = FsIdentityGuard::enter(owner)?;
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&root_path)
                .map_err(|error| error.to_string())?
        };
        if root.metadata().map_err(|error| error.to_string())?.uid() != owner {
            return Err("prototype browse root must belong to its authenticated owner".into());
        }
        let mountpoint = mountpoint
            .canonicalize()
            .map_err(|error| error.to_string())?;
        crate::provenance::fsec::require_secure_location(&mountpoint, &[0])
            .map_err(|error| error.to_string())?;
        let metadata = std::fs::symlink_metadata(&mountpoint).map_err(|error| error.to_string())?;
        if !metadata.is_dir()
            || metadata.uid() != 0
            || metadata.mode() & 0o077 != 0
            || std::fs::read_dir(&mountpoint)
                .map_err(|error| error.to_string())?
                .next()
                .is_some()
            || mount_at(&mountpoint)?.is_some()
        {
            return Err(
                "native filesystem mountpoint must be a private empty Root directory".into(),
            );
        }
        let write_session = OnceLock::new();
        if let Some(session) = session {
            write_session.set(session).map_err(|_| "native mutation session already initialized")?;
        }
        let shared = Arc::new(Shared {
            owner,
            controller,
            root,
            root_path,
            owner_home,
            expiry: Mutex::new(Expiry {
                deadline: Instant::now() + lifetime,
                ceiling: Instant::now() + lifetime,
            }),
            metadata_only,
            binding: Mutex::new(None),
            #[cfg(test)]
            chosen: Mutex::new(Vec::new()),
            writes: Mutex::new(Vec::new()),
            saves: Mutex::new(Vec::new()),
            #[cfg(test)]
            watches: Mutex::new(Vec::new()),
            session: write_session,
            effects: Mutex::new(()),
            stages: Mutex::new(Vec::new()),
            active_effect: AtomicBool::new(false),
            own_invalidations: Mutex::new(BTreeMap::new()),
            observed: Mutex::new(BTreeMap::new()),
            polls: Mutex::new(BTreeMap::new()),
            notifier: Mutex::new(None),
            selected_reads: Mutex::new(BTreeMap::new()),
            selected_expiry: Mutex::new(None),
            revoked: AtomicBool::new(false),
            ended: AtomicBool::new(false),
            monitor_stop: AtomicBool::new(false),
            cleanup_error: Mutex::new(None),
            #[cfg(test)]
            fail_worker: AtomicBool::new(false),
            #[cfg(test)]
            fail_completion: AtomicBool::new(false),
            scratch_bytes: AtomicU64::new(0),
            inverse_bytes: AtomicU64::new(0),
            mutation_attempts: AtomicU64::new(0),
        });
        if shared.session.get().is_some() {
            shared.validate_session()?;
        }
        let filesystem = backend::Filesystem::new(shared.clone())?;
        let device = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open("/dev/fuse")
            .map_err(|error| format!("required /dev/fuse: {error}"))?;
        let (gid, _) = crate::clawd::client_identity::owner_groups(owner)?;
        let target =
            CString::new(mountpoint.as_os_str().as_bytes()).map_err(|error| error.to_string())?;
        let source =
            CString::new(format!("claw-native-fs-{}", uuid::Uuid::new_v4().simple())).unwrap();
        let options = CString::new(format!(
            "fd={},rootmode=40000,user_id={owner},group_id={gid},allow_other,default_permissions,max_read=131072",
            device.as_raw_fd(),
        )).unwrap();
        let result = unsafe {
            libc::mount(
                source.as_ptr(),
                target.as_ptr(),
                c"fuse".as_ptr(),
                libc::MS_NOSUID
                    | libc::MS_NODEV
                    | libc::MS_NOEXEC
                    | if !writable { libc::MS_RDONLY } else { 0 },
                options.as_ptr().cast(),
            )
        };
        if result != 0 {
            return Err(format!(
                "required private FUSE mount: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mount_id = match mount_at(&mountpoint) {
            Ok(Some((id, kind))) if kind == "fuse" => id,
            result => {
                if unsafe { libc::umount2(target.as_ptr(), 0) } != 0 {
                    return Err(format!("native filesystem startup cleanup remains pending: {} (identity: {result:?})",
                        std::io::Error::last_os_error()));
                }
                if mount_at(&mountpoint)?.is_some() {
                    return Err(
                        "native filesystem startup mount remains after checked unmount".into(),
                    );
                }
                return Err(format!(
                    "native filesystem mount identity unavailable: {result:?}"
                ));
            }
        };
        let mut view = Self {
            shared: shared.clone(),
            mountpoint,
            mount_id,
            mounted: true,
            worker: None,
            monitor: None,
            watcher: None,
        };
        let state = shared.clone();
        view.worker = Some(
            std::thread::Builder::new()
                .name("claw-native-fs".into())
                .spawn(move || {
                    struct Ended(Arc<Shared>);
                    impl Drop for Ended {
                        fn drop(&mut self) {
                            if let Ok(mut polls) = self.0.polls.lock() {
                                polls.clear();
                            }
                            if let Ok(mut notifier) = self.0.notifier.lock() {
                                notifier.take();
                            }
                            self.0.ended.store(true, Ordering::Release);
                            self.0.revoked.store(true, Ordering::Release);
                        }
                    }
                    let _ended = Ended(state.clone());
                    let mut session =
                        fuser::Session::from_fd(filesystem, device.into(), fuser::SessionACL::All);
                    *state
                        .notifier
                        .lock()
                        .map_err(|_| std::io::Error::other("native notifier poisoned"))? =
                        Some(session.notifier());
                    session.run()
                })
                .map_err(|error| error.to_string())?,
        );
        let observer = shared.clone();
        view.watcher = Some(
            std::thread::Builder::new()
                .name("claw-native-fs-watch".into())
                .spawn(move || {
                    struct WatchEnded(Arc<Shared>);
                    impl Drop for WatchEnded {
                        fn drop(&mut self) {
                            self.0.revoked.store(true, Ordering::Release);
                        }
                    }
                    let _ended = WatchEnded(observer.clone());
                    if let Err(error) = backend::observe(&observer) {
                        observer
                            .provider_failed(format!("native backing observer failed: {error}"));
                    }
                })
                .map_err(|error| error.to_string())?,
        );
        view.monitor = Some(
            std::thread::Builder::new()
                .name("claw-native-fs-custody".into())
                .spawn(move || {
                    while !shared.monitor_stop.load(Ordering::Acquire) {
                        if shared.live().is_err()
                            || shared.ended.load(Ordering::Acquire)
                            || !shared.instance_live().unwrap_or(false)
                        {
                            shared.revoked.store(true, Ordering::Release);
                            if let Err(error) =
                                shared.retire_processes(Instant::now() + Duration::from_secs(3))
                            {
                                if let Ok(mut pending) = shared.cleanup_error.lock() {
                                    *pending = Some(error);
                                }
                                std::thread::sleep(Duration::from_millis(100));
                                continue;
                            }
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                })
                .map_err(|error| error.to_string())?,
        );
        Ok(view)
    }

    pub(crate) fn bind(&self, process: &ProcessIdentity, group: OwnedFd) -> Result<(), String> {
        self.shared.controller()?;
        self.shared
            .live()
            .map_err(|_| "native filesystem lease is not live")?;
        if process.uid() != self.shared.owner {
            return Err("native filesystem process has another owner".into());
        }

        let process = workload::InstanceProcess::receive(
            process.pidfd().map_err(|error| error.to_string())?,
            group.try_clone().map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let mut binding = self
            .shared
            .binding
            .lock()
            .map_err(|_| "native filesystem binding poisoned")?;
        if binding.is_some() {
            return Err("native filesystem process binding is one-use".into());
        }
        *binding = Some(Binding { process, group });
        Ok(())
    }

    pub(crate) fn read_provider(&self) -> Result<ReadProvider, String> {
        ReadProvider::new(self.shared.clone())
    }

    #[cfg(test)]
    pub(crate) fn choose_read(&self, relative: &Path) -> Result<(), String> {
        self.shared.controller()?;
        if self.shared.metadata_only {
            return Err("Home metadata views cannot select content".into());
        }
        let pin = backend::open_relative(&self.shared, relative, false)
            .map_err(|error| format!("chosen native resource refused: {error}"))?;
        let metadata = pin.metadata().map_err(|error| error.to_string())?;
        let directory = metadata.is_dir();
        // Prove ordinary owner read access now; an O_PATH metadata pin alone
        // cannot authorize binary content.
        let _read = backend::open_relative(&self.shared, relative, true)
            .map_err(|error| format!("chosen resource owner access refused: {error}"))?;
        let read_metadata = _read.metadata().map_err(|error| error.to_string())?;
        if (metadata.dev(), metadata.ino()) != (read_metadata.dev(), read_metadata.ino()) {
            return Err("chosen resource changed during owner authorization".into());
        }
        let mut chosen = self
            .shared
            .chosen
            .lock()
            .map_err(|_| "chosen resources poisoned")?;
        if chosen.len() >= 32 {
            return Err("native filesystem chosen-resource limit reached".into());
        }
        chosen.push(ChosenRead {
            relative: relative.into(),
            pin,
            directory,
        });
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn choose_write(&self, relative: &Path) -> Result<(), String> {
        self.shared.controller()?;
        if self.shared.metadata_only {
            return Err("Home metadata views cannot select writes".into());
        }
        self.shared.validate_session()?;
        backend::choose_write(&self.shared, relative).map_err(|error| {
            format!(
                "chosen native write resource refused: {}",
                std::io::Error::from_raw_os_error(error)
            )
        })
    }

    #[cfg(test)]
    pub(crate) fn reserve_save(&self, target: &Path) -> Result<PathBuf, String> {
        self.shared.controller()?;
        if self.shared.metadata_only {
            return Err("Home metadata views cannot stage saves".into());
        }
        self.shared.validate_session()?;
        backend::reserve_save(&self.shared, target).map_err(|error| {
            format!(
                "native save reservation refused: {}",
                std::io::Error::from_raw_os_error(error)
            )
        })
    }

    #[cfg(test)]
    pub(crate) fn choose_watch(&self, relative: &Path) -> Result<(), String> {
        self.shared.controller()?;
        if self.shared.metadata_only {
            return Err("Home metadata views have no content watch API".into());
        }
        let pin = backend::open_relative(&self.shared, relative, false)
            .map_err(|error| std::io::Error::from_raw_os_error(error).to_string())?;
        if !pin.metadata().map_err(|error| error.to_string())?.is_file() {
            return Err("native poll watch currently requires one regular file".into());
        }
        let mut watches = self
            .shared
            .watches
            .lock()
            .map_err(|_| "native watches poisoned")?;
        if watches.len() >= 32 {
            return Err("native watch limit reached".into());
        }
        watches.push(ChosenRead {
            relative: relative.into(),
            pin,
            directory: false,
        });
        Ok(())
    }

    pub(crate) fn revoke(&self) -> Result<(), String> {
        self.shared.controller()?;
        self.shared.revoked.store(true, Ordering::Release);
        Ok(())
    }

    pub(crate) fn health(&self) -> Result<(), String> {
        self.shared.controller()?;
        if let Some(error) = self
            .shared
            .cleanup_error
            .lock()
            .map_err(|_| "native cleanup state poisoned")?
            .as_ref()
        {
            return Err(format!(
                "native filesystem checked retirement pending: {error}"
            ));
        }
        self.shared
            .live()
            .map_err(|_| "native filesystem lease is revoked or expired")?;
        if self.shared.ended.load(Ordering::Acquire) {
            return Err("native filesystem worker ended".into());
        }
        if !self.shared.instance_live()? {
            return Err("native filesystem instance process ended or changed".into());
        }
        Ok(())
    }

    fn retry_pending(deadline: Instant) -> Result<(), String> {
        let mut pending = PENDING
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .map_err(|_| "native filesystem pending custody poisoned")?;
        let mut index = 0;
        while index < pending.len() {
            if pending[index].retire_checked(deadline).is_ok() {
                pending.swap_remove(index);
            } else {
                index += 1;
            }
        }
        if !pending.is_empty() {
            return Err("native filesystem prior retirement is pending; custody retained".into());
        }
        Ok(())
    }

    pub(crate) fn retire_checked(&mut self, deadline: Instant) -> Result<(), String> {
        self.shared.controller()?;
        self.shared.revoked.store(true, Ordering::Release);
        self.shared.retire_processes(deadline)?;
        self.shared.selected_reads.lock().map_err(|_| "native read leases poisoned")?.clear();
        backend::cleanup_stages(&self.shared, deadline)?;
        while self
            .watcher
            .as_ref()
            .is_some_and(|watcher| !watcher.is_finished())
        {
            if Instant::now() >= deadline {
                return Err("native backing observer has not retired; custody retained".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.shared
            .notifier
            .lock()
            .map_err(|_| "native notifier poisoned")?
            .take();
        self.shared
            .polls
            .lock()
            .map_err(|_| "native polls poisoned")?
            .clear();
        if self.mounted {
            if mount_at(&self.mountpoint)?.map(|(id, _)| id) != Some(self.mount_id) {
                return Err("native filesystem mount identity changed; custody retained".into());
            }
            let path = CString::new(self.mountpoint.as_os_str().as_bytes())
                .map_err(|error| error.to_string())?;
            if unsafe { libc::umount2(path.as_ptr(), 0) } != 0 {
                return Err(format!(
                    "native filesystem checked unmount pending: {}",
                    std::io::Error::last_os_error()
                ));
            }
            self.mounted = false;
        }
        if mount_at(&self.mountpoint)?.is_some() {
            return Err("native filesystem mount remains after unmount".into());
        }
        self.shared.monitor_stop.store(true, Ordering::Release);
        for done in [
            self.worker.as_ref().map(JoinHandle::is_finished),
            self.monitor.as_ref().map(JoinHandle::is_finished),
            self.watcher.as_ref().map(JoinHandle::is_finished),
        ] {
            if done == Some(false) && Instant::now() >= deadline {
                return Err("native filesystem provider retirement remains pending".into());
            }
        }
        while self
            .worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
            || self
                .monitor
                .as_ref()
                .is_some_and(|monitor| !monitor.is_finished())
            || self
                .watcher
                .as_ref()
                .is_some_and(|watcher| !watcher.is_finished())
        {
            if Instant::now() >= deadline {
                return Err("native filesystem provider threads have not retired".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // A prior worker failure is reported by health; this result acknowledges
        // only checked custody completion, not successful filesystem execution.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        if let Some(monitor) = self.monitor.take() {
            let _ = monitor.join();
        }
        if let Some(watcher) = self.watcher.take() {
            let _ = watcher.join();
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn fail_worker_for_fixture(&self) -> Result<(), String> {
        self.shared.controller()?;
        self.shared.fail_worker.store(true, Ordering::Release);
        let _ = std::fs::metadata(&self.mountpoint);
        let deadline = Instant::now() + Duration::from_secs(3);
        while !self.shared.ended.load(Ordering::Acquire) {
            if Instant::now() >= deadline {
                return Err("private failure fixture did not stop the actual FUSE worker".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn fail_completion_for_fixture(&self) -> Result<(), String> {
        self.shared.controller()?;
        self.shared.fail_completion.store(true, Ordering::Release);
        Ok(())
    }
}

impl Drop for NativeFsView {
    fn drop(&mut self) {
        if (self.mounted
            || self.worker.is_some()
            || self.monitor.is_some()
            || self.watcher.is_some())
            && self
                .retire_checked(Instant::now() + Duration::from_secs(3))
                .is_err()
        {
            let retained = Self {
                shared: self.shared.clone(),
                mountpoint: self.mountpoint.clone(),
                mount_id: self.mount_id,
                mounted: self.mounted,
                worker: self.worker.take(),
                monitor: self.monitor.take(),
                watcher: self.watcher.take(),
            };
            self.mounted = false;
            PENDING
                .get_or_init(|| Mutex::new(Vec::new()))
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(retained);
            tracing::error!("native filesystem retains failed retirement and refuses replacement");
        }
    }
}

fn mount_at(path: &Path) -> Result<Option<(u64, String)>, String> {
    let path = path
        .to_str()
        .ok_or("native filesystem mountpoint is not UTF-8")?;
    if path.contains([' ', '\n', '\\']) {
        return Err("invalid private mountpoint".into());
    }
    let info =
        std::fs::read_to_string("/proc/self/mountinfo").map_err(|error| error.to_string())?;
    for line in info.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.get(4) == Some(&path) {
            let separator = fields
                .iter()
                .position(|field| *field == "-")
                .ok_or("invalid mount record")?;
            let id = fields[0].parse().map_err(|_| "invalid mount identity")?;
            return Ok(Some((
                id,
                fields
                    .get(separator + 1)
                    .ok_or("missing mount type")?
                    .to_string(),
            )));
        }
    }
    Ok(None)
}
