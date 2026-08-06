//! Shared application state held by the axum router.

use std::sync::Arc;

use crate::config::AgentConfig;

#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<AppStateInner>,
}

pub struct AppStateInner {
    pub cfg: AgentConfig,
    pub owner_uid: u32,
    pub started_at_unix: u64,
}

impl AppState {
    pub fn new(cfg: AgentConfig, owner_uid: u32) -> Self {
        let started_at_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            inner: Arc::new(AppStateInner {
                cfg,
                owner_uid,
                started_at_unix,
            }),
        }
    }

}

pub fn current_owner_uid() -> Result<u32, String> {
    #[cfg(unix)]
    {
        let uid = unsafe { libc::geteuid() } as u32;
        if uid == 0 {
            return Err(
                "cos agent serve refuses to run as root; launch one instance per desktop user"
                    .to_string(),
            );
        }
        Ok(uid)
    }
    #[cfg(not(unix))]
    {
        Err("cos agent serve multi-user isolation requires Unix user identities".to_string())
    }
}

pub fn validate_owner_storage(owner_uid: u32) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let roots = [
            crate::paths::data_dir(),
            crate::paths::caps_data_dir(),
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
        for path in roots {
            if !path.exists() {
                crate::storage::ensure_private_dir(&path)
                    .map_err(|error| format!("create private {}: {error}", path.display()))?;
            }
            let before = std::fs::symlink_metadata(&path)
                .map_err(|error| format!("inspect {}: {error}", path.display()))?;
            if !before.is_dir()
                || before.file_type().is_symlink()
                || before.uid() != owner_uid
            {
                return Err(format!(
                    "{} must be a directory owned by uid {owner_uid}; choose per-user COS_DATA_DIR and COS_CAPS_DATA_DIR",
                    path.display()
                ));
            }
            crate::storage::ensure_private_dir(&path)
                .map_err(|error| format!("tighten permissions on {}: {error}", path.display()))?;
            let after = std::fs::symlink_metadata(&path)
                .map_err(|error| format!("reinspect {}: {error}", path.display()))?;
            if !after.is_dir()
                || after.file_type().is_symlink()
                || after.uid() != owner_uid
                || after.dev() != before.dev()
                || after.ino() != before.ino()
            {
                return Err(format!(
                    "{} changed while its ownership was being secured",
                    path.display()
                ));
            }
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = owner_uid;
        Err("owner storage validation requires Unix".to_string())
    }
}
