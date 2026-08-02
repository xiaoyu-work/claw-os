//! Shared application state.
//!
//! The bridge holds three things:
//! - the configured HTTP port (env override or random)
//! - the `clawd` Unix socket path used for chat turns
//! - a per-process bearer token required by every HTTP route
//!
//! Everything else (sessions, models, credentials) is owned by `clawd`. The
//! bridge no longer serves a static SPA — the React frontend was
//! retired in favour of the native libcosmic UI (`cos-agent-ui`),
//! which calls only the `/api/*` JSON+SSE endpoints.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Serialize;

const DISCOVERY_FILE: &str = "endpoint.json";

#[derive(Clone)]
pub struct AppState {
    pub port: u16,
    pub clawd_socket: PathBuf,
    pub auth_token: String,
}

impl AppState {
    pub fn from_env() -> anyhow::Result<Self> {
        let port = std::env::var("COS_AGENT_BRIDGE_PORT")
            .ok()
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(0);
        let clawd_socket = clawd_socket_path();
        let auth_token = generate_auth_token()?;
        Ok(Self {
            port,
            clawd_socket,
            auth_token,
        })
    }
}

fn clawd_socket_path() -> PathBuf {
    if let Ok(path) = std::env::var("CLAWD_SOCKET") {
        return PathBuf::from(path);
    }
    if let Ok(path) = std::env::var("COS_RUNTIME_DIR") {
        return PathBuf::from(path).join("clawd.sock");
    }
    PathBuf::from("/run/cos/clawd.sock")
}

fn generate_auth_token() -> anyhow::Result<String> {
    let mut random = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut random))
        .context("generating bridge authentication token")?;
    Ok(random.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn runtime_dir() -> anyhow::Result<PathBuf> {
    let dir = std::env::var("XDG_RUNTIME_DIR")
        .ok()
        .map(PathBuf::from)
        .map(|dir| dir.join("cos-agent-bridge"))
        .context("XDG_RUNTIME_DIR is required for bridge discovery")?;
    if !dir.is_absolute() {
        anyhow::bail!("bridge runtime directory must be absolute");
    }

    if !dir.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&dir)
                .with_context(|| format!("creating bridge runtime directory {}", dir.display()))?;
        }
        #[cfg(not(unix))]
        std::fs::create_dir(&dir)
            .with_context(|| format!("creating bridge runtime directory {}", dir.display()))?;
    }
    validate_private_directory(&dir)?;
    Ok(dir)
}

fn validate_private_directory(dir: &Path) -> anyhow::Result<()> {
    let metadata = std::fs::symlink_metadata(dir)
        .with_context(|| format!("inspecting bridge runtime directory {}", dir.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        anyhow::bail!(
            "bridge runtime path is not a real directory: {}",
            dir.display()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o077 != 0 {
            anyhow::bail!(
                "bridge runtime directory {} must not be accessible by group or other users",
                dir.display()
            );
        }
        #[cfg(target_os = "linux")]
        {
            let current_uid = std::fs::metadata("/proc/self")
                .context("inspecting bridge process identity")?
                .uid();
            if metadata.uid() != current_uid {
                anyhow::bail!(
                    "bridge runtime directory {} belongs to uid {}, expected {}",
                    dir.display(),
                    metadata.uid(),
                    current_uid
                );
            }
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct Discovery<'a> {
    port: u16,
    token: &'a str,
}

/// Atomically publish the bound endpoint and bearer token inside the
/// current user's private runtime directory.
pub fn publish_endpoint(port: u16, token: &str) -> anyhow::Result<()> {
    let dir = runtime_dir()?;
    let path = dir.join(DISCOVERY_FILE);
    let temp_path = dir.join(format!(
        ".{DISCOVERY_FILE}.{}.{}.tmp",
        std::process::id(),
        &token[..token.len().min(16)]
    ));
    let body = serde_json::to_vec(&Discovery { port, token })
        .context("serializing bridge discovery state")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temp_path)
            .with_context(|| format!("creating discovery file {}", temp_path.display()))?;
        if let Err(error) = file
            .write_all(&body)
            .and_then(|_| file.write_all(b"\n"))
            .and_then(|_| file.sync_all())
            .and_then(|_| std::fs::rename(&temp_path, &path))
        {
            let _ = std::fs::remove_file(&temp_path);
            return Err(error)
                .with_context(|| format!("publishing bridge endpoint {}", path.display()));
        }
        std::fs::File::open(&dir)
            .and_then(|directory| directory.sync_all())
            .with_context(|| format!("syncing bridge runtime directory {}", dir.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&temp_path, &body)
            .and_then(|_| std::fs::rename(&temp_path, &path))
            .with_context(|| format!("publishing bridge endpoint {}", path.display()))?;
    }

    if let Some(parent) = dir.parent() {
        let legacy = parent.join("cos-agent-bridge.port");
        if let Err(error) = std::fs::remove_file(&legacy) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    path = %legacy.display(),
                    %error,
                    "failed to remove legacy bridge port file"
                );
            }
        }
    }
    Ok(())
}
