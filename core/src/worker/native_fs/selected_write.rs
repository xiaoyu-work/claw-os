use std::fs::File;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use crate::caps::{Cap, Verb};
use crate::clawd::authority::Authorized;

use super::selected_read::{FileCandidate, ReadProvider};
use super::{backend, ChosenWrite, Shared};

pub(crate) trait WriteLeaseCheck: Send + Sync {
    fn authorize(&self) -> Result<Authorized, String>;
}

pub(crate) struct WritePermit {
    candidate: Arc<FileCandidate>,
    capability: Cap,
    deadline: Instant,
    check: Arc<dyn WriteLeaseCheck>,
    session: crate::session::SessionId,
}

impl WritePermit {
    pub(crate) fn new(
        candidate: Arc<FileCandidate>,
        authorized: &Authorized,
        capability: &Cap,
        deadline: Instant,
        check: Arc<dyn WriteLeaseCheck>,
        session: crate::session::SessionId,
    ) -> Result<Self, String> {
        if capability.verb != Verb::FS_WRITE
            || authorized.spent() != std::slice::from_ref(capability)
            || deadline <= Instant::now()
        {
            return Err("selected write requires a spent ordinary write decision".into());
        }
        candidate.view.upgrade().ok_or("native view has retired")?
            .live().map_err(|_| "native view source or lease ended")?;
        let fresh = check.authorize()?;
        if fresh.spent() != std::slice::from_ref(capability) {
            return Err("write selection has another capability".into());
        }
        candidate.revalidate()?;
        candidate.require_inverse_path()?;
        Ok(Self {
            candidate,
            capability: capability.clone(),
            deadline,
            check,
            session,
        })
    }
}

#[derive(Clone)]
pub(super) struct WriteLease {
    pub(super) selection: String,
    pub(super) candidate: Arc<FileCandidate>,
    capability: Cap,
    pub(super) deadline: Instant,
    check: Arc<dyn WriteLeaseCheck>,
}

impl WriteLease {
    pub(super) fn expired(&self) -> bool {
        Instant::now() >= self.deadline
    }

    pub(super) fn authorize(&self) -> Result<Authorized, i32> {
        self.candidate.view.upgrade().ok_or(libc::EACCES)?.live()?;
        if self.expired() {
            return Err(libc::EACCES);
        }
        let authorized = self.check.authorize().map_err(|_| libc::EACCES)?;
        if authorized.spent() != std::slice::from_ref(&self.capability) {
            return Err(libc::EACCES);
        }
        Ok(authorized)
    }
}

pub(crate) struct SaveReservation {
    pub(crate) id: String,
    pub(crate) parent_node: u64,
    pub(crate) name: String,
    pub(crate) deadline: Instant,
}

impl ReadProvider {
    pub(crate) fn install_write(&self, selection: &str, permit: WritePermit) -> Result<(), String> {
        let shared = self.shared.upgrade().ok_or("native view has retired")?;
        shared.controller()?;
        shared.live().map_err(io_message)?;
        let view = permit
            .candidate
            .view
            .upgrade()
            .ok_or("native view has retired")?;
        if !Arc::ptr_eq(&view, &shared) {
            return Err("write decision belongs to another native view".into());
        }
        let ceiling = shared
            .expiry
            .lock()
            .map_err(|_| "native expiry poisoned")?
            .ceiling;
        if permit.deadline <= Instant::now() || permit.deadline > ceiling {
            return Err("selected write exceeds its original finite authority".into());
        }
        permit.candidate.revalidate()?;
        permit.candidate.require_inverse_path()?;
        let _authorized = permit.check.authorize()?;
        let _serial = shared
            .effects
            .lock()
            .map_err(|_| "native effects poisoned")?;
        let _authorized = permit.check.authorize()?;
        permit.candidate.revalidate_locked(&shared)?;
        let pin = backend::open_relative(&shared, permit.candidate.relative(), false)
            .map_err(io_message)?;
        if pin.metadata().map_err(|error| error.to_string())?.len() > 8 * 1024 * 1024 {
            return Err("selected write exceeds the bounded inverse size".into());
        }
        let _write = backend::reopen_as(&shared, &pin, libc::O_WRONLY).map_err(io_message)?;
        let _inverse = backend::reopen(&shared, &pin).map_err(io_message)?;
        let parent = backend::open_relative(
            &shared,
            permit
                .candidate
                .relative()
                .parent()
                .ok_or("write target has no retained parent")?,
            true,
        )
        .map_err(io_message)?;
        shared.session_context(|| {
            let meta =
                crate::session::get_meta(&permit.session).map_err(|error| error.to_string())?;
            if meta.owner_uid != Some(shared.owner)
                || !crate::session::record_is_root_owned(&permit.session)
            {
                return Err("native write session is not the Root-owned owner record".into());
            }
            Ok(())
        })?;
        match shared.session.get() {
            Some(session) if session != &permit.session => {
                return Err("native write session cannot switch".into())
            }
            Some(_) => {}
            None => {
                shared
                    .session
                    .set(permit.session.clone())
                    .map_err(|_| "native write session already bound")?;
            }
        }
        shared.validate_session()?;
        let mut writes = shared
            .writes
            .lock()
            .map_err(|_| "native write leases poisoned")?;
        if writes.len() >= 32
            || writes.iter().any(|write| {
                write
                    .permission
                    .as_ref()
                    .is_some_and(|permission| permission.selection == selection)
            })
        {
            return Err("native write selection bound reached".into());
        }
        let node = permit.candidate.current_node()?;
        let deadline = permit.deadline;
        writes.push(ChosenWrite {
            relative: permit.candidate.relative().into(),
            pin,
            parent,
            permission: Some(WriteLease {
                selection: selection.into(),
                candidate: permit.candidate,
                capability: permit.capability,
                deadline: permit.deadline,
                check: permit.check,
            }),
        });
        drop(writes);
        shared.bound_selection(deadline)?;
        shared
            .own_invalidations
            .lock()
            .map_err(|_| "native invalidations poisoned")?
            .insert(node, false);
        Ok(())
    }

    pub(crate) fn reserve_save(&self, selection: &str) -> Result<SaveReservation, String> {
        let shared = self.shared.upgrade().ok_or("native view has retired")?;
        shared.controller()?;
        shared.live().map_err(io_message)?;
        let permission = {
            let writes = shared
                .writes
                .lock()
                .map_err(|_| "native write leases poisoned")?;
            writes
                .iter()
                .filter_map(|write| write.permission.as_ref())
                .find(|permission| permission.selection == selection)
                .cloned()
                .ok_or("atomic save requires this window's selected write")?
        };
        let _authorized = permission.authorize().map_err(io_message)?;
        let _serial = shared
            .effects
            .lock()
            .map_err(|_| "native effects poisoned")?;
        let _authorized = permission.authorize().map_err(io_message)?;
        permission.candidate.revalidate_locked(&shared)?;
        let relative =
            backend::reserve_save(&shared, permission.candidate.relative()).map_err(io_message)?;
        let name = relative
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("Root scratch name is not canonical")?
            .to_string();
        let suffix = name
            .strip_prefix(".cos-save-")
            .ok_or("Root scratch name has another origin")?;
        let id = format!("ss-{suffix}");
        let parent_path = relative.parent().ok_or("Root scratch has no parent")?;
        let parent_node = shared
            .observed
            .lock()
            .map_err(|_| "native observations poisoned")?
            .iter()
            .find(|(_, node)| node.linked && node.relative == parent_path)
            .map(|(inode, _)| *inode)
            .ok_or("native scratch parent is not materialized")?;
        shared
            .own_invalidations
            .lock()
            .map_err(|_| "native invalidations poisoned")?
            .insert(parent_node, true);
        Ok(SaveReservation {
            id,
            parent_node,
            name,
            deadline: permission.deadline,
        })
    }
}

fn io_message(error: i32) -> String {
    std::io::Error::from_raw_os_error(error).to_string()
}

pub(super) fn permission(
    shared: &Shared,
    path: &Path,
    pin: &File,
) -> Result<Option<WriteLease>, i32> {
    shared.live()?;
    let expected = backend::identity(pin)?;
    let writes = shared.writes.lock().map_err(|_| libc::EIO)?;
    for write in writes.iter().filter(|write| write.relative == path) {
        if backend::identity(&write.pin)? != expected {
            continue;
        }
        if shared.metadata_only && write.permission.is_none() {
            return Err(libc::EACCES);
        }
        return Ok(write.permission.clone());
    }
    Err(libc::EACCES)
}

pub(super) fn authorize_effect(
    shared: &Shared,
    path: &Path,
    pin: &File,
) -> Result<Option<Authorized>, i32> {
    let permission = permission(shared, path, pin)?;
    let Some(permission) = permission else {
        return Ok(None);
    };
    permission
        .candidate
        .revalidate_locked(shared)
        .map_err(|_| libc::ESTALE)?;
    permission.authorize().map(Some)
}

/// Called only after the existing canonical mutation bracket committed.
pub(super) fn publish_owned_revision(
    shared: &Shared,
    path: &Path,
    previous: &File,
    node: u64,
    evidence: &backend::VerifiedMutation,
) -> Result<(), i32> {
    let metadata = evidence.current_metadata(shared, path, previous)?;
    let published = evidence.file();
    let generation = {
        let mut observed = shared.observed.lock().map_err(|_| libc::EIO)?;
        let observed = observed.get_mut(&node).ok_or(libc::ESTALE)?;
        observed.stamp = backend::fingerprint(&metadata);
        observed.version = observed.version.checked_add(1).ok_or(libc::EOVERFLOW)?;
        observed.version
    };
    let before = backend::identity(previous)?;
    {
        let mut reads = shared.selected_reads.lock().map_err(|_| libc::EIO)?;
        for read in reads
            .values_mut()
            .filter(|read| read.candidate.relative() == path)
        {
            if read.candidate.current_identity().map_err(|_| libc::EIO)? != before {
                continue;
            }
            let old_node = read.candidate.current_node().map_err(|_| libc::EIO)?;
            if old_node != node {
                let old = read
                    .file
                    .try_clone()
                    .map_err(|error| error.raw_os_error().unwrap_or(libc::EIO))?;
                let stamp = evidence.old_version(shared.owner, &old)?;
                if read.history.len() >= 256 {
                    return Err(libc::ENOSPC);
                }
                read.history.insert(old_node, (old, stamp));
                read.file = backend::reopen(shared, published)?;
            }
            read.candidate
                .publish(node, published, &metadata, generation)
                .map_err(|_| libc::EIO)?;
        }
    }
    {
        let mut writes = shared.writes.lock().map_err(|_| libc::EIO)?;
        for write in writes.iter_mut().filter(|write| write.relative == path) {
            if backend::identity(&write.pin)? != before {
                continue;
            }
            if let Some(permission) = &write.permission {
                permission
                    .candidate
                    .publish(node, published, &metadata, generation)
                    .map_err(|_| libc::EIO)?;
            }
            write.pin = published
                .try_clone()
                .map_err(|error| error.raw_os_error().unwrap_or(libc::EIO))?;
        }
    }
    shared
        .own_invalidations
        .lock()
        .map_err(|_| libc::EIO)?
        .insert(node, false);
    Ok(())
}
