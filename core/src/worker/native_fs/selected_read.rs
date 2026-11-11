use std::fs::File;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};
use std::time::Instant;

use super::{backend, Shared};

/// Implemented only by a Root-held, ordinary authority-store read grant.
pub(crate) trait ReadLeaseCheck: Send + Sync {
    fn require(&self) -> Result<(), String>;
}

pub(crate) struct FileCandidate {
    view: Weak<Shared>,
    node: u64,
    relative: PathBuf,
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
    pub(crate) fn stamp(&self) -> [u64; 8] {
        self.stamp
    }

    pub(crate) fn display(&self) -> Result<String, String> {
        let view = self.view.upgrade().ok_or("native view has retired")?;
        let mut display = String::new();
        for byte in view.root_path.join(&self.relative).as_os_str().as_bytes() {
            if (0x20..=0x7e).contains(byte) && !matches!(*byte, b'\\' | b'"' | b'\'') {
                display.push(char::from(*byte));
            } else {
                use std::fmt::Write;
                write!(&mut display, "\\x{byte:02x}").map_err(|error| error.to_string())?;
            }
        }
        Ok(display)
    }

    pub(crate) fn revalidate(&self) -> Result<(), String> {
        let view = self.view.upgrade().ok_or("native view has retired")?;
        view.live().map_err(|_| "native view lease ended")?;
        let current = backend::open_relative(&view, &self.relative, false)
            .map_err(|error| std::io::Error::from_raw_os_error(error).to_string())?;
        let metadata = current.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || (metadata.dev(), metadata.ino()) != self.identity
            || backend::fingerprint(&metadata) != self.stamp
            || backend::identity(&self.pin).map_err(|error| error.to_string())? != self.identity
        {
            return Err("selected source identity or revision changed".into());
        }
        let observed = view
            .observed
            .lock()
            .map_err(|_| "native observations poisoned")?;
        if !observed.get(&self.node).is_some_and(|node| {
            node.linked && node.version == self.generation && node.relative == self.relative
        }) {
            return Err("selected native node generation changed".into());
        }
        Ok(())
    }
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
    candidate: Arc<FileCandidate>,
    deadline: Instant,
    check: Arc<dyn ReadLeaseCheck>,
    file: File,
}

impl GrantedRead {
    pub(super) fn expired(&self) -> bool {
        Instant::now() >= self.deadline
    }
}

#[derive(Clone)]
pub(crate) struct ReadProvider {
    shared: Weak<Shared>,
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
        Ok(Arc::new(FileCandidate {
            view: Arc::downgrade(&shared),
            node,
            relative,
            pin,
            identity,
            stamp: backend::fingerprint(&metadata),
            generation,
        }))
    }

    pub(crate) fn install(&self, selection: &str, permit: ReadPermit) -> Result<(), String> {
        let shared = self.shared.upgrade().ok_or("native view has retired")?;
        shared.controller()?;
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
        permit.candidate.revalidate()?;
        let file = backend::reopen(&shared, &permit.candidate.pin)
            .map_err(|error| std::io::Error::from_raw_os_error(error).to_string())?;
        let mut selected = shared
            .selected_reads
            .lock()
            .map_err(|_| "native read leases poisoned")?;
        if selected.len() >= 32 || selected.contains_key(selection) {
            return Err("native selected-read lease bound reached".into());
        }
        selected.insert(
            selection.into(),
            GrantedRead {
                candidate: permit.candidate,
                deadline: permit.deadline,
                check: permit.check,
                file,
            },
        );
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
            reads
                .iter()
                .find(|(id, read)| {
                    read.candidate.node == node
                        && selection.is_none_or(|selected| id.as_str() == selected)
                })
                .map(|(id, read)| {
                    (
                        id.clone(),
                        read.candidate.clone(),
                        read.deadline,
                        read.check.clone(),
                        read.file.try_clone(),
                    )
                })
                .ok_or(libc::EACCES)?
        };
        let (id, candidate, deadline, check, file) = lease;
        if Instant::now() >= deadline || check.require().is_err() || candidate.revalidate().is_err()
        {
            self.stop();
            return Err(libc::EACCES);
        }
        Ok((
            id,
            file.map_err(|error| error.raw_os_error().unwrap_or(libc::EIO))?,
        ))
    }
}
