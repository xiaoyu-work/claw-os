use std::collections::BTreeMap;
use std::fs::File;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

use super::{backend, Shared};

/// Implemented only by a Root-held, ordinary authority-store read grant.
pub(crate) trait ReadLeaseCheck: Send + Sync {
    fn require(&self) -> Result<(), String>;
}

pub(crate) struct FileCandidate {
    pub(super) view: Weak<Shared>,
    node: u64,
    relative: PathBuf,
    identity: (u64, u64),
    stamp: [u64; 8],
    generation: u64,
    current: Mutex<CurrentRevision>,
}

struct CurrentRevision {
    node: u64,
    pin: File,
    identity: (u64, u64),
    stamp: [u64; 8],
    generation: u64,
}

impl FileCandidate {
    pub(crate) fn node(&self) -> u64 {
        self.node
    }
    pub(crate) fn relative(&self) -> &Path {
        &self.relative
    }
    pub(crate) fn identity(&self) -> (u64, u64) {
        self.identity
    }
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(super) fn current_node(&self) -> Result<u64, String> {
        Ok(self
            .current
            .lock()
            .map_err(|_| "native revision poisoned")?
            .node)
    }

    pub(super) fn current_identity(&self) -> Result<(u64, u64), String> {
        Ok(self
            .current
            .lock()
            .map_err(|_| "native revision poisoned")?
            .identity)
    }

    pub(crate) fn require_inverse_path(&self) -> Result<(), String> {
        let view = self.view.upgrade().ok_or("native view has retired")?;
        view.live().map_err(|_| "native view source or lease ended")?;
        if view.root_path.join(&self.relative).to_str().is_none() {
            return Err("native write inverse requires a UTF-8 target; byte identity is never converted lossily".into());
        }
        Ok(())
    }

    pub(super) fn publish(
        &self,
        node: u64,
        pin: &File,
        metadata: &std::fs::Metadata,
        generation: u64,
    ) -> Result<(), String> {
        let current = CurrentRevision {
            node,
            pin: pin.try_clone().map_err(|error| error.to_string())?,
            identity: (metadata.dev(), metadata.ino()),
            stamp: backend::fingerprint(metadata),
            generation,
        };
        *self
            .current
            .lock()
            .map_err(|_| "native revision poisoned")? = current;
        Ok(())
    }
    pub(crate) fn stamp(&self) -> [u64; 8] {
        self.stamp
    }

    pub(crate) fn display(&self) -> Result<String, String> {
        let view = self.view.upgrade().ok_or("native view has retired")?;
        display_path(&view, &self.relative)
    }

    pub(crate) fn revalidate(&self) -> Result<(), String> {
        let view = self.view.upgrade().ok_or("native view has retired")?;
        let _serial = view.effects.lock().map_err(|_| "native effects poisoned")?;
        self.revalidate_locked(&view)
    }

    pub(crate) fn health_validate(&self) -> Result<(), String> {
        let view = self.view.upgrade().ok_or("native view has retired")?;
        view.live().map_err(|_| "native view source or lease ended")?;
        let _serial = match view.effects.try_lock() {
            Ok(serial) => serial,
            Err(std::sync::TryLockError::WouldBlock) => return Ok(()),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err("native effects poisoned".into())
            }
        };
        self.revalidate_locked(&view)
    }

    pub(super) fn revalidate_locked(&self, view: &Shared) -> Result<(), String> {
        view.live().map_err(|_| "native view lease ended")?;
        let revision = self
            .current
            .lock()
            .map_err(|_| "native revision poisoned")?;
        let current = backend::open_relative(view, &self.relative, false)
            .map_err(|error| std::io::Error::from_raw_os_error(error).to_string())?;
        let metadata = current.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || (metadata.dev(), metadata.ino()) != revision.identity
            || backend::fingerprint(&metadata) != revision.stamp
            || backend::identity(&revision.pin).map_err(|error| error.to_string())?
                != revision.identity
        {
            return Err("selected source identity or revision changed".into());
        }
        let observed = view
            .observed
            .lock()
            .map_err(|_| "native observations poisoned")?;
        if !observed.get(&revision.node).is_some_and(|node| {
            node.linked && node.version == revision.generation && node.relative == self.relative
        }) {
            return Err("selected native node generation changed".into());
        }
        Ok(())
    }
}

pub(super) fn display_path(view: &Shared, relative: &Path) -> Result<String, String> {
    view.live().map_err(|_| "native view source or lease ended")?;
    let mut display = String::new();
    for byte in view.root_path.join(relative).as_os_str().as_bytes() {
        if (0x20..=0x7e).contains(byte) && !matches!(*byte, b'\\' | b'"' | b'\'') {
            display.push(char::from(*byte));
        } else {
            use std::fmt::Write;
            write!(&mut display, "\\x{byte:02x}").map_err(|error| error.to_string())?;
        }
    }
    Ok(display)
}

pub(crate) struct ReadPermit {
    candidate: Arc<FileCandidate>,
    deadline: Instant,
    check: Arc<dyn ReadLeaseCheck>,
}

impl ReadPermit {
    pub(crate) fn new(
        candidate: Arc<FileCandidate>,
        authorized: &crate::clawd::authority::Authorized,
        capability: &crate::caps::Cap,
        deadline: Instant,
        check: Arc<dyn ReadLeaseCheck>,
    ) -> Result<Self, String> {
        if capability.verb != crate::caps::Verb::FS_READ
            || authorized.spent() != std::slice::from_ref(capability)
            || deadline <= Instant::now()
        {
            return Err("selected read requires a spent ordinary read authority proof".into());
        }
        candidate.view.upgrade().ok_or("native view has retired")?
            .live().map_err(|_| "native view source or lease ended")?;
        check.require()?;
        candidate.revalidate()?;
        Ok(Self {
            candidate,
            deadline,
            check,
        })
    }
}

pub(super) struct GrantedRead {
    pub(super) candidate: Arc<FileCandidate>,
    deadline: Instant,
    check: Arc<dyn ReadLeaseCheck>,
    pub(super) file: File,
    pub(super) history: BTreeMap<u64, (File, [u64; 8])>,
}

#[derive(Clone)]
pub(crate) struct ReadProvider {
    pub(super) shared: Weak<Shared>,
}

impl ReadProvider {
    pub(super) fn new(shared: Arc<Shared>) -> Result<Self, String> {
        shared.controller()?;
        if !shared.metadata_only {
            return Err("selected read requires the controlled metadata view".into());
        }
        Ok(Self {
            shared: Arc::downgrade(&shared),
        })
    }

    pub(crate) fn candidate(&self, node: u64) -> Result<Arc<FileCandidate>, String> {
        let shared = self.shared.upgrade().ok_or("native view has retired")?;
        shared.controller()?;
        shared.live().map_err(|_| "native view lease ended")?;
        let _serial = shared
            .effects
            .lock()
            .map_err(|_| "native effects poisoned")?;
        if !(2..=4096).contains(&node) {
            return Err("unknown native file node".into());
        }
        let (relative, pin, generation) = {
            let observed = shared
                .observed
                .lock()
                .map_err(|_| "native observations poisoned")?;
            let node = observed
                .get(&node)
                .filter(|node| node.linked)
                .ok_or("unknown native file node")?;
            (
                node.relative.clone(),
                node.pin.try_clone().map_err(|error| error.to_string())?,
                node.version,
            )
        };
        let current = backend::open_relative(&shared, &relative, false)
            .map_err(|error| std::io::Error::from_raw_os_error(error).to_string())?;
        let metadata = current.metadata().map_err(|error| error.to_string())?;
        let identity = (metadata.dev(), metadata.ino());
        if !metadata.is_file()
            || metadata.nlink() != 1
            || backend::identity(&pin).map_err(|error| error.to_string())? != identity
        {
            return Err("native node is not the retained regular file".into());
        }
        let current_pin = pin.try_clone().map_err(|error| error.to_string())?;
        let stamp = backend::fingerprint(&metadata);
        Ok(Arc::new(FileCandidate {
            view: Arc::downgrade(&shared),
            node,
            relative,
            identity,
            stamp,
            generation,
            current: Mutex::new(CurrentRevision {
                node,
                pin: current_pin,
                identity,
                stamp,
                generation,
            }),
        }))
    }

    pub(crate) fn install(&self, selection: &str, permit: ReadPermit) -> Result<(), String> {
        let shared = self.shared.upgrade().ok_or("native view has retired")?;
        shared.controller()?;
        shared.live().map_err(|_| "native view source or lease ended")?;
        let candidate_view = permit
            .candidate
            .view
            .upgrade()
            .ok_or("native view has retired")?;
        if !Arc::ptr_eq(&shared, &candidate_view) {
            return Err("selected read belongs to another view".into());
        }
        let now = Instant::now();
        let ceiling = shared
            .expiry
            .lock()
            .map_err(|_| "native expiry poisoned")?
            .ceiling;
        if permit.deadline <= now || permit.deadline > ceiling {
            return Err("selected read exceeds its finite view authority".into());
        }
        permit.check.require()?;
        let _serial = shared
            .effects
            .lock()
            .map_err(|_| "native effects poisoned")?;
        permit.check.require()?;
        permit.candidate.revalidate_locked(&shared)?;
        let pin = permit
            .candidate
            .current
            .lock()
            .map_err(|_| "native revision poisoned")?
            .pin
            .try_clone()
            .map_err(|error| error.to_string())?;
        let file = backend::reopen(&shared, &pin)
            .map_err(|error| std::io::Error::from_raw_os_error(error).to_string())?;
        let mut selected = shared
            .selected_reads
            .lock()
            .map_err(|_| "native read leases poisoned")?;
        if selected.len() >= 32 || selected.contains_key(selection) {
            return Err("native selected-read lease bound reached".into());
        }
        let deadline = permit.deadline;
        selected.insert(
            selection.into(),
            GrantedRead {
                candidate: permit.candidate,
                deadline: permit.deadline,
                check: permit.check,
                file,
                history: BTreeMap::new(),
            },
        );
        drop(selected);
        shared.bound_selection(deadline)?;
        Ok(())
    }

    pub(crate) fn stop(&self) {
        if let Some(shared) = self.shared.upgrade() {
            shared.revoked.store(true, Ordering::Release);
        }
    }

    pub(super) fn open(&self, node: u64, selection: Option<&str>) -> Result<(String, File), i32> {
        let shared = self.shared.upgrade().ok_or(libc::EACCES)?;
        shared.live()?;
        let lease = {
            let reads = shared.selected_reads.lock().map_err(|_| libc::EIO)?;
            let mut selected = None;
            for (id, read) in reads.iter() {
                if selection.is_some_and(|selection| selection != id) {
                    continue;
                }
                let current = read.candidate.current_node().map_err(|_| libc::EIO)? == node;
                let file = if current {
                    Some((read.file.try_clone(), None))
                } else if selection.is_some() {
                    read.history
                        .get(&node)
                        .map(|(file, stamp)| (file.try_clone(), Some(*stamp)))
                } else {
                    None
                };
                if let Some((file, history)) = file {
                    selected = Some((
                        id.clone(),
                        read.candidate.clone(),
                        read.deadline,
                        read.check.clone(),
                        file,
                        history,
                    ));
                    break;
                }
            }
            selected.ok_or(libc::EACCES)?
        };
        let (id, candidate, deadline, check, file, history) = lease;
        if Instant::now() >= deadline || check.require().is_err() || candidate.revalidate().is_err()
        {
            self.stop();
            return Err(libc::EACCES);
        }
        let file = file.map_err(|error| error.raw_os_error().unwrap_or(libc::EIO))?;
        if history.is_some_and(|stamp| {
            file.metadata()
                .map_or(true, |metadata| backend::fingerprint(&metadata) != stamp)
        }) {
            self.stop();
            return Err(libc::ESTALE);
        }
        Ok((id, file))
    }

    pub(crate) fn check_histories(&self) -> Result<(), String> {
        let shared = self.shared.upgrade().ok_or("native view has retired")?;
        shared.live().map_err(|_| "native view source or lease ended")?;
        let _serial = match shared.effects.try_lock() {
            Ok(serial) => serial,
            Err(std::sync::TryLockError::WouldBlock) => return Ok(()),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err("native effects poisoned".into())
            }
        };
        let reads = shared
            .selected_reads
            .lock()
            .map_err(|_| "native read leases poisoned")?;
        for read in reads.values() {
            for (file, stamp) in read.history.values() {
                if backend::fingerprint(&file.metadata().map_err(|error| error.to_string())?)
                    != *stamp
                {
                    return Err("retained old selected-file revision changed externally".into());
                }
            }
        }
        Ok(())
    }

    pub(super) fn release_history(&self, node: u64) {
        if let Some(shared) = self.shared.upgrade() {
            if let Ok(mut reads) = shared.selected_reads.lock() {
                for read in reads.values_mut() {
                    read.history.remove(&node);
                }
            }
        }
    }

    pub(super) fn read_existing(&self, node: u64) -> Result<(String, File), i32> {
        let shared = self.shared.upgrade().ok_or(libc::EACCES)?;
        let selection = {
            let reads = shared.selected_reads.lock().map_err(|_| libc::EIO)?;
            reads
                .iter()
                .find(|(_, read)| {
                    read.history.contains_key(&node)
                        || read
                            .candidate
                            .current_node()
                            .is_ok_and(|current| current == node)
                })
                .map(|(id, _)| id.clone())
                .ok_or(libc::EACCES)?
        };
        self.open(node, Some(&selection))
    }
}
