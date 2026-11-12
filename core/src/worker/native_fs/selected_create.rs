//! One-shot creation of an absent regular child. No directory write lease.

use std::ffi::{CString, OsStr, OsString};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

use crate::caps::{Cap, Verb};
use crate::clawd::authority::Authorized;
use crate::clawd::client_identity::FsIdentityGuard;

use super::selected_read::ReadProvider;
use super::selected_write::{WriteLeaseCheck, bind_session};
use super::{BackingStage, SaveTarget, Shared, backend};

fn io_message(error: i32) -> String {
    std::io::Error::from_raw_os_error(error).to_string()
}

fn errno(error: std::io::Error) -> i32 {
    error.raw_os_error().unwrap_or(libc::EIO)
}

pub(crate) struct CreateCandidate {
    view: Weak<Shared>,
    parent_node: u64,
    parent_relative: PathBuf,
    relative: PathBuf,
    name: String,
    parent: File,
    identity: (u64, u64),
    stamp: [u64; 8],
    generation: u64,
}

impl CreateCandidate {
    pub(crate) fn node(&self) -> u64 {
        self.parent_node
    }
    pub(crate) fn relative(&self) -> &Path {
        &self.relative
    }
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
    pub(crate) fn identity(&self) -> (u64, u64) {
        self.identity
    }
    pub(crate) fn stamp(&self) -> [u64; 8] {
        self.stamp
    }
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn display(&self) -> Result<String, String> {
        let view = self.view.upgrade().ok_or("native view has retired")?;
        super::selected_read::display_path(&view, &self.relative)
    }

    pub(crate) fn revalidate(&self) -> Result<(), String> {
        let view = self.view.upgrade().ok_or("native view has retired")?;
        view.live().map_err(io_message)?;
        let _serial = view.effects.lock().map_err(|_| "native effects poisoned")?;
        self.revalidate_locked(&view).map_err(io_message)
    }

    pub(crate) fn health_validate(&self) -> Result<(), String> {
        self.revalidate()
    }

    pub(super) fn revalidate_locked(&self, view: &Shared) -> Result<(), i32> {
        view.live()?;
        let current = backend::open_relative(view, &self.parent_relative, true)?;
        if !current.metadata().map_err(errno)?.is_dir()
            || backend::identity(&current)? != self.identity
            || backend::identity(&self.parent)? != self.identity
        {
            return Err(libc::ESTALE);
        }
        let observed = view.observed.lock().map_err(|_| libc::EIO)?;
        if !observed.get(&self.parent_node).is_some_and(|node| {
            node.linked
                && node.relative == self.parent_relative
                && backend::identity(&node.pin).is_ok_and(|identity| identity == self.identity)
        }) {
            return Err(libc::ESTALE);
        }
        drop(observed);
        crate::worker::derive::reject_forbidden_for_owner(
            &view.root_path.join(&self.relative),
            &view.owner_home,
        )
        .map_err(|_| libc::EACCES)?;
        {
            let _owner = FsIdentityGuard::enter(view.owner).map_err(|_| libc::EACCES)?;
            // Linux AT_EACCESS uses the current filesystem credentials, including
            // this thread's owner fsuid/groups; the final open also enforces them.
            if unsafe {
                libc::syscall(
                    libc::SYS_faccessat2,
                    self.parent.as_raw_fd(),
                    c"".as_ptr(),
                    libc::W_OK | libc::X_OK,
                    libc::AT_EMPTY_PATH | libc::AT_EACCESS,
                )
            } != 0
            {
                return Err(errno(std::io::Error::last_os_error()));
            }
        }
        view.live()
    }

    fn absent_locked(&self, view: &Shared) -> Result<(), i32> {
        self.revalidate_locked(view)?;
        let _owner = FsIdentityGuard::enter(view.owner).map_err(|_| libc::EACCES)?;
        match crate::clawd::filesystem::open_at(
            &self.parent,
            OsStr::new(&self.name),
            libc::O_PATH | libc::O_NOFOLLOW,
            0,
        ) {
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => Ok(()),
            Err(error) => Err(errno(error)),
            Ok(_) => Err(libc::EEXIST),
        }
    }
}

pub(crate) struct CreatePermit {
    candidate: Arc<CreateCandidate>,
    capability: Cap,
    deadline: Instant,
    check: Arc<dyn WriteLeaseCheck>,
    session: crate::session::SessionId,
}

impl CreatePermit {
    pub(crate) fn new(
        candidate: Arc<CreateCandidate>,
        proof: &Authorized,
        capability: &Cap,
        deadline: Instant,
        check: Arc<dyn WriteLeaseCheck>,
        session: crate::session::SessionId,
    ) -> Result<Self, String> {
        candidate
            .view
            .upgrade()
            .ok_or("native view has retired")?
            .live()
            .map_err(io_message)?;
        if capability.verb != Verb::FS_WRITE
            || proof.spent() != std::slice::from_ref(capability)
            || deadline <= Instant::now()
        {
            return Err("file creation requires a spent ordinary fs.write decision".into());
        }
        let fresh = check.authorize()?;
        if fresh.spent() != std::slice::from_ref(capability) {
            return Err("creation decision has another capability".into());
        }
        candidate.revalidate()?;
        Ok(Self {
            candidate,
            capability: capability.clone(),
            deadline,
            check,
            session,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CreationState {
    Selected,
    Reserved,
    InFlight,
    Created,
    Failed,
    Uncertain,
}

#[derive(Clone)]
pub(crate) struct CreateReservation {
    pub(crate) id: String,
    pub(crate) parent_node: u64,
    pub(crate) name: String,
    pub(crate) target_name: String,
    pub(crate) deadline: Instant,
}

struct Progress {
    state: CreationState,
    slot: Option<CreateReservation>,
    attempted: bool,
}

pub(super) struct CreateLease {
    candidate: Arc<CreateCandidate>,
    capability: Cap,
    deadline: Instant,
    check: Arc<dyn WriteLeaseCheck>,
    progress: Mutex<Progress>,
}

impl CreateLease {
    fn authorize(&self) -> Result<Authorized, i32> {
        self.candidate.view.upgrade().ok_or(libc::EACCES)?.live()?;
        if Instant::now() >= self.deadline {
            return Err(libc::EACCES);
        }
        let proof = self.check.authorize().map_err(|_| libc::EACCES)?;
        if proof.spent() != std::slice::from_ref(&self.capability) {
            return Err(libc::EACCES);
        }
        self.candidate.view.upgrade().ok_or(libc::EACCES)?.live()?;
        Ok(proof)
    }
}

impl ReadProvider {
    pub(crate) fn creation_candidate(
        &self,
        parent_node: u64,
        name: &str,
    ) -> Result<Arc<CreateCandidate>, String> {
        claw_os_sdk::generated::validate_gui_file_create_request(
            &serde_json::json!({"parent_node":parent_node,"name":name}),
        )
        .map_err(|error| error.to_string())?;
        if name.starts_with(".cos-save-") || name.starts_with(".cos-native-") {
            return Err("creation target uses the private native staging namespace".into());
        }
        let shared = self.shared.upgrade().ok_or("native view has retired")?;
        shared.controller()?;
        shared.live().map_err(io_message)?;
        let _serial = shared
            .effects
            .lock()
            .map_err(|_| "native effects poisoned")?;
        let (parent_relative, parent, generation, stamp) = {
            let observed = shared
                .observed
                .lock()
                .map_err(|_| "native observations poisoned")?;
            let node = observed
                .get(&parent_node)
                .filter(|node| node.linked)
                .ok_or("unknown native parent directory")?;
            (
                node.relative.clone(),
                node.pin.try_clone().map_err(|error| error.to_string())?,
                node.version,
                node.stamp,
            )
        };
        let relative = parent_relative.join(name);
        backend::relative_parts(&relative).map_err(io_message)?;
        if shared.root_path.join(&relative).to_str().is_none() {
            return Err(
                "creation inverse requires a UTF-8 parent identity; no lossy conversion".into(),
            );
        }
        let identity = backend::identity(&parent).map_err(io_message)?;
        let candidate = Arc::new(CreateCandidate {
            view: Arc::downgrade(&shared),
            parent_node,
            parent_relative,
            relative,
            name: name.into(),
            parent,
            identity,
            generation,
            stamp,
        });
        candidate.absent_locked(&shared).map_err(io_message)?;
        if shared
            .saves
            .lock()
            .map_err(|_| "native reservations poisoned")?
            .iter()
            .any(|slot| slot.relative == candidate.relative)
        {
            return Err("creation target aliases a reserved private scratch".into());
        }
        shared.live().map_err(io_message)?;
        Ok(candidate)
    }

    pub(crate) fn install_create(
        &self,
        selection: &str,
        permit: CreatePermit,
    ) -> Result<(), String> {
        let shared = self.shared.upgrade().ok_or("native view has retired")?;
        shared.controller()?;
        shared.live().map_err(io_message)?;
        let source = permit
            .candidate
            .view
            .upgrade()
            .ok_or("native view has retired")?;
        if !Arc::ptr_eq(&source, &shared) {
            return Err("creation belongs to another native view".into());
        }
        let ceiling = shared
            .expiry
            .lock()
            .map_err(|_| "native expiry poisoned")?
            .ceiling;
        if permit.deadline <= Instant::now() || permit.deadline > ceiling {
            return Err("creation exceeds its original finite authority".into());
        }
        let _proof = permit.check.authorize()?;
        let _serial = shared
            .effects
            .lock()
            .map_err(|_| "native effects poisoned")?;
        let _proof = permit.check.authorize()?;
        permit
            .candidate
            .absent_locked(&shared)
            .map_err(io_message)?;
        bind_session(&shared, &permit.session)?;
        let mut creates = shared
            .creations
            .lock()
            .map_err(|_| "native creations poisoned")?;
        if creates.len() >= 32 || creates.contains_key(selection) {
            return Err("native creation selection bound reached".into());
        }
        let deadline = permit.deadline;
        creates.insert(
            selection.into(),
            Arc::new(CreateLease {
                candidate: permit.candidate,
                capability: permit.capability,
                deadline,
                check: permit.check,
                progress: Mutex::new(Progress {
                    state: CreationState::Selected,
                    slot: None,
                    attempted: false,
                }),
            }),
        );
        drop(creates);
        shared.bound_selection(deadline)?;
        shared.live().map_err(io_message)
    }

    pub(crate) fn creation_state(&self, selection: &str) -> Result<CreationState, String> {
        let shared = self.shared.upgrade().ok_or("native view has retired")?;
        shared.controller()?;
        shared.live().map_err(io_message)?;
        let lease = lease(&shared, selection).map_err(io_message)?;
        let state = lease
            .progress
            .lock()
            .map_err(|_| "native creation progress poisoned")?
            .state;
        shared.live().map_err(io_message)?;
        Ok(state)
    }

    pub(crate) fn reserve_create(&self, selection: &str) -> Result<CreateReservation, String> {
        let shared = self.shared.upgrade().ok_or("native view has retired")?;
        shared.controller()?;
        shared.live().map_err(io_message)?;
        let lease = lease(&shared, selection).map_err(io_message)?;
        let _proof = lease.authorize().map_err(io_message)?;
        let _serial = shared
            .effects
            .lock()
            .map_err(|_| "native effects poisoned")?;
        let _proof = lease.authorize().map_err(io_message)?;
        lease.candidate.absent_locked(&shared).map_err(io_message)?;
        let mut progress = lease
            .progress
            .lock()
            .map_err(|_| "native creation progress poisoned")?;
        if progress.attempted {
            return Err("creation finalization cannot be replayed".into());
        }
        if let Some(slot) = &progress.slot {
            return Ok(slot.clone());
        }
        let mut saves = shared
            .saves
            .lock()
            .map_err(|_| "native reservations poisoned")?;
        if saves.len() >= backend::MAX_SAVES {
            return Err("native reservation bound reached".into());
        }
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let name = format!(".cos-save-{suffix}");
        let relative = lease.candidate.parent_relative.join(&name);
        if relative == lease.candidate.relative
            || saves.iter().any(|slot| slot.relative == relative)
        {
            return Err("reserved scratch identity collision".into());
        }
        match backend::open_relative(&shared, &relative, false) {
            Err(libc::ENOENT) => {}
            Err(error) => return Err(io_message(error)),
            Ok(_) => return Err("reserved scratch name already exists".into()),
        }
        saves.push(SaveTarget {
            relative,
            target: lease.candidate.relative.clone(),
            creation: Some(selection.into()),
        });
        let slot = CreateReservation {
            id: format!("ss-{suffix}"),
            parent_node: lease.candidate.parent_node,
            name,
            target_name: lease.candidate.name.clone(),
            deadline: lease.deadline,
        };
        progress.slot = Some(slot.clone());
        progress.state = CreationState::Reserved;
        shared
            .own_invalidations
            .lock()
            .map_err(|_| "native invalidations poisoned")?
            .insert(slot.parent_node, true);
        Ok(slot)
    }
}

fn lease(shared: &Shared, selection: &str) -> Result<Arc<CreateLease>, i32> {
    shared
        .creations
        .lock()
        .map_err(|_| libc::EIO)?
        .get(selection)
        .cloned()
        .ok_or(libc::EACCES)
}

pub(super) fn authorize_scratch(shared: &Shared, selection: &str) -> Result<(), i32> {
    let lease = lease(shared, selection)?;
    let _proof = lease.authorize()?;
    lease.candidate.revalidate().map_err(|_| libc::ESTALE)?;
    let progress = lease.progress.lock().map_err(|_| libc::EIO)?;
    if progress.attempted || progress.state != CreationState::Reserved {
        return Err(libc::EACCES);
    }
    Ok(())
}

pub(super) fn check_slot(
    shared: &Shared,
    selection: &str,
    parent: &File,
    target: &Path,
) -> Result<(), i32> {
    let lease = lease(shared, selection)?;
    if target != lease.candidate.relative()
        || backend::identity(parent)? != lease.candidate.identity
    {
        return Err(libc::ESTALE);
    }
    authorize_scratch(shared, selection)
}

pub(super) struct CreationProof {
    file: File,
    stamp: [u64; 8],
}

impl CreationProof {
    pub(super) fn file(&self) -> &File {
        &self.file
    }
}

fn set_state(lease: &CreateLease, state: CreationState) -> Result<(), i32> {
    lease.progress.lock().map_err(|_| libc::EIO)?.state = state;
    Ok(())
}

/// Caller holds the existing effect mutex and active-effect guard.
pub(super) fn commit(
    shared: &Shared,
    selection: &str,
    target: &Path,
    parent: &File,
    bytes: &[u8],
) -> Result<CreationProof, i32> {
    let lease = lease(shared, selection)?;
    if bytes.len() as u64 > backend::MAX_FILE
        || target != lease.candidate.relative()
        || backend::identity(parent)? != lease.candidate.identity
    {
        return Err(libc::EACCES);
    }
    {
        let mut progress = lease.progress.lock().map_err(|_| libc::EIO)?;
        if progress.attempted || progress.state != CreationState::Reserved {
            return Err(libc::EACCES);
        }
        progress.attempted = true;
        progress.state = CreationState::InFlight;
    }
    let prepared = (|| {
        shared.live()?;
        shared.validate_session().map_err(|_| libc::EIO)?;
        let proof = lease.authorize()?;
        // Missing is a real preimage, not an empty-file snapshot.
        lease.candidate.absent_locked(shared)?;
        shared
            .mutation_attempts
            .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < backend::MAX_MUTATIONS).then_some(current + 1)
            })
            .map_err(|_| libc::ENOSPC)?;
        let sid = shared.session.get().ok_or(libc::EROFS)?;
        let logical = shared.root_path.join(target);
        if logical.to_str().is_none() {
            return Err(libc::EINVAL);
        }
        shared
            .session_context(|| {
                let seq = crate::session::record_fs_write_bytes(sid, &logical, None)
                    .map_err(|error| error.to_string())?;
                let key = format!("{}:{seq}", sid.as_str());
                crate::session::journal::begin_mutation(crate::session::journal::MutationStart {
                    partition: crate::session::journal::Partition::Session(sid.clone()),
                    owner_uid: shared.owner,
                    route: crate::clawd::routes::Command::SystemFilesystemWrite
                        .route()
                        .name,
                    request_key: &key,
                    grant: Some(proof.grant_ref().as_str()),
                    session_mutation: Some(seq),
                    context_ingest: false,
                })
                .map_err(|error| error.to_string())
            })
            .map_err(|_| libc::EIO)
    })();
    let bracket = match prepared {
        Ok(bracket) => bracket,
        Err(error) => {
            if set_state(&lease, CreationState::Failed).is_err() {
                shared.provider_failed(
                    "native creation progress could not record a refused attempt".into(),
                );
                return Err(libc::EIO);
            }
            return Err(error);
        }
    };
    let stage_name = OsString::from(format!(".cos-native-{}", uuid::Uuid::new_v4().simple()));
    let stage = prepare_stage(shared, parent, &stage_name, bytes);
    let mut published = false;
    let dispatched = stage.and_then(|file| {
        shared.live()?;
        let _proof = lease.authorize()?;
        lease.candidate.revalidate_locked(shared)?;
        let target_name = OsString::from(&lease.candidate.name);
        let identity = backend::identity(&file)?;
        let mut stages = shared.stages.lock().map_err(|_| libc::EIO)?;
        let index = stages
            .iter()
            .position(|stage| {
                backend::identity(&stage.file).is_ok_and(|current| current == identity)
            })
            .ok_or(libc::ESTALE)?;
        let _owner = FsIdentityGuard::enter(shared.owner).map_err(|_| libc::EACCES)?;
        // Flags are Root-owned even when the App used ordinary rename().
        rename_noreplace(parent, &stage_name, OsStr::new(&lease.candidate.name))?;
        published = true;
        stages[index].published = true;
        stages[index].name = target_name;
        Ok(file)
    });
    let file = match dispatched {
        Ok(file) => file,
        Err(error) => {
            if !published && discard_unpublished_stage(shared, parent, &stage_name).is_ok() {
                let closed = shared.session_context(|| {
                    bracket
                        .fail(
                            "native_create_not_published",
                            "conditional creation did not publish its target",
                        )
                        .map_err(|error| error.to_string())
                });
                if let Err(detail) = closed {
                    shared.indeterminate(detail);
                    let _ = set_state(&lease, CreationState::Uncertain);
                    return Err(libc::EIO);
                }
                if set_state(&lease, CreationState::Failed).is_err() {
                    shared.provider_failed(
                        "native creation progress failed after confirmed no publication".into(),
                    );
                    return Err(libc::EIO);
                }
                return Err(error);
            }
            let detail = shared
                .session_context(|| Err::<(), _>(bracket.indeterminate().to_string()))
                .unwrap_err();
            shared.indeterminate(detail);
            let _ = set_state(&lease, CreationState::Uncertain);
            return Err(libc::EIO);
        }
    };
    let verified = (|| {
        let stamp = backend::verify_exact_image(shared.owner, &file, bytes, 1)?;
        {
            let _owner = FsIdentityGuard::enter(shared.owner).map_err(|_| libc::EACCES)?;
            file.sync_all().map_err(errno)?;
            parent.sync_all().map_err(errno)?;
        }
        lease.candidate.revalidate_locked(shared)?;
        let current = backend::open_relative(shared, target, false)?;
        if backend::identity(&current)? != backend::identity(&file)?
            || backend::fingerprint(&current.metadata().map_err(errno)?) != stamp
        {
            return Err(libc::ESTALE);
        }
        Ok(stamp)
    })();
    let stamp = match verified {
        Ok(stamp) => stamp,
        Err(_) => {
            let detail = shared
                .session_context(|| Err::<(), _>(bracket.indeterminate().to_string()))
                .unwrap_err();
            shared.indeterminate(detail);
            let _ = set_state(&lease, CreationState::Uncertain);
            return Err(libc::EIO);
        }
    };
    if let Err(detail) = shared.session_context(|| {
        bracket
            .commit()
            .map(|_| ())
            .map_err(|error| error.to_string())
    }) {
        shared.indeterminate(detail);
        let _ = set_state(&lease, CreationState::Uncertain);
        return Err(libc::EIO);
    }
    let completed = (|| {
        let mut stages = shared.stages.lock().map_err(|_| libc::EIO)?;
        let identity = backend::identity(&file)?;
        let index = stages
            .iter()
            .position(|stage| {
                stage.published
                    && backend::identity(&stage.file).is_ok_and(|current| current == identity)
            })
            .ok_or(libc::ESTALE)?;
        stages.remove(index);
        Ok::<_, i32>(())
    })();
    if completed.is_err() {
        shared.provider_failed(
            "committed creation could not retire its tracked backing stage".into(),
        );
        return Err(libc::EIO);
    }
    Ok(CreationProof { file, stamp })
}

fn prepare_stage(shared: &Shared, parent: &File, name: &OsStr, bytes: &[u8]) -> Result<File, i32> {
    let mut stages = shared.stages.lock().map_err(|_| libc::EIO)?;
    if stages.len() >= backend::MAX_SAVES {
        return Err(libc::ENOSPC);
    }
    let retained_parent = parent.try_clone().map_err(errno)?;
    let _owner = FsIdentityGuard::enter(shared.owner).map_err(|_| libc::EACCES)?;
    let file = crate::clawd::filesystem::open_at(
        parent,
        name,
        libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
        0o600,
    )
    .map_err(errno)?;
    stages.push(BackingStage {
        parent: retained_parent,
        name: name.into(),
        file,
        published: false,
    });
    let stage = stages.last().ok_or(libc::EIO)?;
    stage.file.write_all_at(bytes, 0).map_err(errno)?;
    stage.file.sync_all().map_err(errno)?;
    stage.file.try_clone().map_err(errno)
}

fn rename_noreplace(parent: &File, from: &OsStr, to: &OsStr) -> Result<(), i32> {
    let from = CString::new(from.as_bytes()).map_err(|_| libc::EINVAL)?;
    let to = CString::new(to.as_bytes()).map_err(|_| libc::EINVAL)?;
    if unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            parent.as_raw_fd(),
            from.as_ptr(),
            parent.as_raw_fd(),
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } != 0
    {
        return Err(errno(std::io::Error::last_os_error()));
    }
    Ok(())
}

fn discard_unpublished_stage(shared: &Shared, parent: &File, name: &OsStr) -> Result<(), i32> {
    let mut stages = shared.stages.lock().map_err(|_| libc::EIO)?;
    let parent_identity = backend::identity(parent)?;
    let index = stages.iter().position(|stage| {
        stage.name == name
            && backend::identity(&stage.parent).is_ok_and(|identity| identity == parent_identity)
    });
    let _owner = FsIdentityGuard::enter(shared.owner).map_err(|_| libc::EACCES)?;
    let current =
        crate::clawd::filesystem::open_at(parent, name, libc::O_PATH | libc::O_NOFOLLOW, 0);
    match index {
        None => {
            if !matches!(current, Err(ref error) if error.raw_os_error() == Some(libc::ENOENT)) {
                return Err(libc::ESTALE);
            }
            parent.sync_all().map_err(errno)
        }
        Some(index) => {
            let stage = &stages[index];
            if stage.published {
                return Err(libc::ESTALE);
            }
            let current = current.map_err(errno)?;
            if backend::identity(&current)? != backend::identity(&stage.file)? {
                return Err(libc::ESTALE);
            }
            let name = CString::new(name.as_bytes()).map_err(|_| libc::EINVAL)?;
            if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } != 0 {
                return Err(errno(std::io::Error::last_os_error()));
            }
            if stage.file.metadata().map_err(errno)?.nlink() != 0 {
                return Err(libc::ESTALE);
            }
            parent.sync_all().map_err(errno)?;
            stages.remove(index);
            Ok(())
        }
    }
}

pub(super) fn publish_created(
    shared: &Shared,
    selection: &str,
    proof: &CreationProof,
) -> Result<(), i32> {
    let lease = lease(shared, selection)?;
    lease.candidate.revalidate_locked(shared)?;
    let current = backend::open_relative(shared, lease.candidate.relative(), false)?;
    if backend::identity(&current)? != backend::identity(&proof.file)?
        || backend::fingerprint(&current.metadata().map_err(errno)?) != proof.stamp
    {
        return Err(libc::ESTALE);
    }

    let mut progress = lease.progress.lock().map_err(|_| libc::EIO)?;
    if !progress.attempted || progress.state != CreationState::InFlight {
        return Err(libc::EACCES);
    }
    progress.state = CreationState::Created;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_replace_creation_never_replaces_a_racing_regular_file_or_symlink() {
        std::fs::create_dir_all("build").unwrap();
        let directory = tempfile::Builder::new()
            .prefix("native-create-")
            .tempdir_in("build")
            .unwrap();
        let parent = File::open(directory.path()).unwrap();
        std::fs::write(directory.path().join("scratch"), b"authored\0\xff").unwrap();
        std::fs::write(directory.path().join("target"), b"foreign\0\xfe").unwrap();
        assert_eq!(
            rename_noreplace(&parent, OsStr::new("scratch"), OsStr::new("target")),
            Err(libc::EEXIST)
        );
        assert_eq!(
            std::fs::read(directory.path().join("target")).unwrap(),
            b"foreign\0\xfe"
        );
        std::fs::remove_file(directory.path().join("target")).unwrap();
        std::os::unix::fs::symlink("missing", directory.path().join("target")).unwrap();
        assert_eq!(
            rename_noreplace(&parent, OsStr::new("scratch"), OsStr::new("target")),
            Err(libc::EEXIST)
        );
        assert!(
            std::fs::symlink_metadata(directory.path().join("target"))
                .unwrap()
                .is_symlink()
        );
        std::fs::remove_file(directory.path().join("target")).unwrap();
        rename_noreplace(&parent, OsStr::new("scratch"), OsStr::new("target")).unwrap();
        assert_eq!(
            std::fs::read(directory.path().join("target")).unwrap(),
            b"authored\0\xff"
        );
        assert!(!directory.path().join("scratch").exists());
    }
}
