use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::clawd::client_identity::FsIdentityGuard;

/// Source pins stay separate from the Home-relative names used by selected-file authority.
pub(crate) struct MetadataRoot {
    pub(super) owner: u32,
    pub(super) home_path: PathBuf,
    pub(super) relative: PathBuf,
    pub(super) root_path: PathBuf,
    pub(super) home: File,
    pub(super) root: File,
    fenced: Arc<AtomicBool>,
}

impl MetadataRoot {
    pub(crate) fn validate_relative(relative: &Path) -> Result<(), String> {
        let bytes = relative.as_os_str().as_encoded_bytes();
        if bytes.is_empty() {
            return Ok(());
        }
        if bytes.len() > 4096
            || bytes.split(|byte| *byte == b'/').count() > 64
            || bytes.split(|byte| *byte == b'/').any(|part| {
                part.is_empty()
                    || matches!(part, b"." | b"..")
                    || part
                        .iter()
                        .any(|byte| matches!(byte, 0 | b'*' | b'?' | b'[' | b']'))
            })
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(
                "GUI metadata subroot must be a bounded literal owner-relative directory".into(),
            );
        }
        Ok(())
    }

    pub(crate) fn pin(owner: u32, home: &Path, relative: &Path) -> Result<Self, String> {
        Self::validate_relative(relative)?;
        if owner == 0
            || !home.is_absolute()
            || crate::paths::verified_home_for_uid(owner)?
                .canonicalize()
                .map_err(|error| error.to_string())?
                .as_os_str()
                != home.as_os_str()
        {
            return Err("metadata source differs from authenticated owner Home".into());
        }
        let _identity = FsIdentityGuard::enter(owner)?;
        Self::pin_as_owner(owner, home, relative)
    }

    fn pin_as_owner(owner: u32, home: &Path, relative: &Path) -> Result<Self, String> {
        Self::validate_relative(relative)?;
        crate::worker::derive::reject_forbidden_for_owner(home, home)?;
        let home_file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(home)
            .map_err(|error| error.to_string())?;
        Self::check_directory(&home_file, home, owner, true)?;
        let mut root = home_file.try_clone().map_err(|error| error.to_string())?;
        let mut root_path = home.to_path_buf();
        for component in relative.components() {
            let Component::Normal(name) = component else {
                return Err("invalid metadata subroot component".into());
            };
            root_path.push(name);
            crate::worker::derive::reject_forbidden_for_owner(&root_path, home)?;
            root = crate::clawd::filesystem::open_at(
                &root,
                name,
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK,
                0,
            )
            .map_err(|error| error.to_string())?;
            Self::check_directory(&root, &root_path, owner, false)?;
        }
        Self::check_directory(&root, &root_path, owner, true)?;
        Self::check_directory(&home_file, home, owner, true)?;
        Ok(Self {
            owner,
            home_path: home.to_path_buf(),
            relative: relative.to_path_buf(),
            root_path,
            home: home_file,
            root,
            fenced: Arc::new(AtomicBool::new(false)),
        })
    }

    fn check_directory(
        file: &File,
        path: &Path,
        owner: u32,
        require_owner: bool,
    ) -> Result<(), String> {
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        let actual = std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
            .map_err(|error| error.to_string())?;
        if !metadata.is_dir()
            || require_owner && metadata.uid() != owner
            || metadata.uid() == owner && metadata.mode() & 0o500 != 0o500
            || actual.as_os_str() != path.as_os_str()
        {
            return Err(
                "GUI metadata directory identity, owner, access or location changed".into(),
            );
        }
        Ok(())
    }

    pub(crate) fn path(&self) -> &Path {
        &self.root_path
    }

    pub(crate) fn relative(&self) -> &Path {
        &self.relative
    }

    pub(crate) fn identities(&self) -> Result<[(u64, u64); 2], String> {
        let home = self.home.metadata().map_err(|error| error.to_string())?;
        let root = self.root.metadata().map_err(|error| error.to_string())?;
        Ok([(home.dev(), home.ino()), (root.dev(), root.ino())])
    }

    pub(crate) fn revalidate(&self) -> Result<(), String> {
        self.revalidate_with(|| Self::pin(self.owner, &self.home_path, &self.relative))
    }

    fn revalidate_with(&self, pin: impl FnOnce() -> Result<Self, String>) -> Result<(), String> {
        // Live checks also run during owner IO; never hold a latch lock across pinning.
        if self.fenced.load(Ordering::Acquire) {
            return Err(
                "authenticated metadata source is fenced; fresh window consent is required".into(),
            );
        }
        let checked = pin().and_then(|current| {
            if current.identities()? != self.identities()? {
                return Err("authenticated metadata root changed after window consent".into());
            }
            Ok(())
        });
        if checked.is_err() {
            self.fenced.store(true, Ordering::Release);
        }
        checked?;
        if self.fenced.load(Ordering::Acquire) {
            return Err("authenticated metadata source was fenced during validation".into());
        }
        Ok(())
    }

    pub(super) fn try_clone(&self) -> Result<Self, String> {
        Ok(Self {
            owner: self.owner,
            home_path: self.home_path.clone(),
            relative: self.relative.clone(),
            root_path: self.root_path.clone(),
            home: self.home.try_clone().map_err(|error| error.to_string())?,
            root: self.root.try_clone().map_err(|error| error.to_string())?,
            fenced: self.fenced.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/worker/native_fs_metadata_scope.rs"
    ));
}
