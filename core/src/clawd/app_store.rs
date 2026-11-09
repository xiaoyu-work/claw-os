//! Official App delivery. Protected human review precedes atomic publication.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::approvals::system_review::{self as reviews, ReviewKind};
use crate::apps::installation::DirectoryInstall;
use crate::apps::store::{self, Catalog, Record};
use crate::provenance::runtime::PackageRef;

use super::client_identity::ClientIdentity;
use super::protocol::BrokerError;

const STORE_ROOT: &str = "/var/lib/cos/app-store";
static SERIALIZER: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Floor {
    generation: u64,
    sha256: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Prepared {
    owner_uid: u32,
    request_id: String,
    review_id: String,
    record: Record,
    expires_at: DateTime<Utc>,
    phase: String,
}

fn records() -> Result<Vec<Prepared>, String> {
    initialize()?;
    let mut result = Vec::new();
    for entry in std::fs::read_dir(root().join("requests")).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        if result.len() >= 1000 {
            return Err("App Store installation history exceeds its bounded capacity".into());
        }
        let path = entry.path();
        let id = path
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or("Invalid OS installation record name")?;
        if request_path(id)? != path {
            return Err("Unexpected file in protected App Store history".into());
        }
        let record: Prepared = read_json(&path)?;
        if record.request_id != id {
            return Err("OS installation record identity does not match its file".into());
        }
        result.push(record);
    }
    Ok(result)
}

fn expire_requests() -> Result<(), String> {
    for mut record in records()? {
        if matches!(record.phase.as_str(), "ready" | "cleanup_pending") && record.expires_at <= Utc::now() {
            reviews::invalidate(record.owner_uid, &record.review_id)?;
            record.phase = "expired".into();
            write_json(&request_path(&record.request_id)?, &record)?;
            let directory = root().join("staging").join(&record.request_id);
            if directory.exists() {
                std::fs::remove_dir_all(&directory).map_err(|error| error.to_string())?;
            }
        }
        if !matches!(record.phase.as_str(), "ready" | "committing")
            && record.expires_at + chrono::Duration::days(1) < Utc::now()
        {
            let directory = root().join("staging").join(&record.request_id);
            if directory.exists() {
                std::fs::remove_dir_all(&directory).map_err(|error| error.to_string())?;
            }
            std::fs::remove_file(request_path(&record.request_id)?)
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

pub async fn requests(_params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let owner_uid = owner(client).await?;
    let _guard = SERIALIZER.lock().await;
    expire_requests().map_err(BrokerError::unavailable)?;
    let requests = records()
        .map_err(BrokerError::unavailable)?
        .into_iter()
        .filter(|record| record.owner_uid == owner_uid)
        .map(|record| {
            json!({
                "request_id": record.request_id, "review_id": record.review_id,
                "app_id": record.record.app_id, "name": record.record.display.name,
                "phase": record.phase, "expires_at": record.expires_at,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({"requests": requests}))
}

fn root() -> PathBuf {
    PathBuf::from(STORE_ROOT)
}

fn apps_root() -> PathBuf {
    std::env::var_os("COS_APPS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/lib/cos/apps"))
}

fn secure_directory(path: &Path, mode: u32) -> Result<(), String> {
    std::fs::create_dir_all(path).map_err(|error| error.to_string())?;
    let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err("App Store state must be a root-owned, non-writable directory".into());
        }
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .map_err(|error| error.to_string())?;
    }
    #[cfg(not(unix))]
    {
        let _ = (metadata, mode);
        return Err("App Store installation requires a Linux OS service".into());
    }
    Ok(())
}

fn initialize() -> Result<(), String> {
    secure_directory(&root(), 0o711)?;
    secure_directory(&root().join("metadata"), 0o700)?;
    secure_directory(&root().join("requests"), 0o700)?;
    secure_directory(&root().join("staging"), 0o711)
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    crate::agent::util::atomic_write_with_fsync(path, &bytes).map_err(|error| error.to_string())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err("App Store state is not protected by the OS".into());
        }
    }
    if !metadata.is_file() || metadata.len() > store::MAX_METADATA as u64 {
        return Err("App Store state is not a bounded regular file".into());
    }
    serde_json::from_slice(&std::fs::read(path).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())
}

async fn owner(client: &ClientIdentity) -> Result<u32, BrokerError> {
    super::app_sessions::require_non_app_caller(client)
        .await
        .map_err(BrokerError::authorization)?;
    client.require_uid().map_err(BrokerError::authorization)
}

async fn catalog(refresh: bool) -> Result<Catalog, String> {
    initialize()?;
    let metadata = root().join("metadata");
    let index_path = metadata.join("index.json");
    let signature_path = metadata.join("index.json.asc");
    let cached = index_path.exists()
        && signature_path.exists()
        && !refresh
        && std::fs::metadata(&index_path)
            .and_then(|value| value.modified())
            .ok()
            .and_then(|value| value.elapsed().ok())
            .is_some_and(|age| age < Duration::from_secs(60));
    let (bytes, signature) = if cached {
        (
            std::fs::read(&index_path).map_err(|error| error.to_string())?,
            std::fs::read(&signature_path).map_err(|error| error.to_string())?,
        )
    } else {
        let http = store::http_client()?;
        let bytes = store::fetch_bytes(&http, store::INDEX_URL, store::MAX_METADATA).await?;
        let signature =
            store::fetch_bytes(&http, &format!("{}.asc", store::INDEX_URL), 128 * 1024).await?;
        (bytes, signature)
    };
    store::verify_catalog_signature(&bytes, &signature).await?;
    let catalog = Catalog::decode(&bytes, Utc::now())?;
    let generation = catalog
        .generation
        .parse::<u64>()
        .map_err(|error| error.to_string())?;
    let checksum = store::sha256(&bytes);
    let floor_path = metadata.join("floor.json");
    if floor_path.exists() {
        let floor: Floor = read_json(&floor_path)?;
        if generation < floor.generation
            || generation == floor.generation && checksum != floor.sha256
        {
            return Err("App Store metadata is older than the OS's accepted catalog".into());
        }
    }
    write_json(
        &floor_path,
        &Floor {
            generation,
            sha256: checksum,
        },
    )?;
    if !cached {
        crate::agent::util::atomic_write_with_fsync(&index_path, &bytes)
            .map_err(|error| error.to_string())?;
        crate::agent::util::atomic_write_with_fsync(&signature_path, &signature)
            .map_err(|error| error.to_string())?;
    }
    Ok(catalog)
}

fn installed_apps() -> Result<Value, String> {
    let discovery = crate::apps::discover_all(&apps_root());
    let mut installed = Vec::new();
    for (id, app) in discovery.verified {
        let package = app.require_verified()?;
        let metadata = store::web_metadata(package);
        installed.push(match metadata {
            Ok(mut value) => {
                value["state"] = json!("installed");
                value["web_ready"] = json!(true);
                value
            }
            Err(_) => json!({
                "id": id, "version": package.version(), "name": id,
                "content_digest": package.content_digest(), "state": "installed",
                "web_ready": false, "tools": [],
            }),
        });
    }
    for (id, app) in discovery.quarantined {
        installed.push(json!({
            "id": id, "name": id, "state": "quarantined", "web_ready": false,
            "error": app.quarantine_reason(),
        }));
    }
    Ok(json!({"apps": installed, "architecture": store::architecture()}))
}

pub async fn list(_params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    owner(client).await?;
    let _guard = SERIALIZER.lock().await;
    let catalog = catalog(true).await.map_err(BrokerError::unavailable)?;
    Ok(json!({
        "catalog": catalog, "architecture": store::architecture(),
        "installed": installed_apps().map_err(BrokerError::unavailable)?["apps"],
    }))
}

pub async fn installed(_params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    owner(client).await?;
    tokio::task::spawn_blocking(installed_apps)
        .await
        .map_err(|error| BrokerError::unavailable(error.to_string()))?
        .map_err(BrokerError::unavailable)
}

fn request_path(id: &str) -> Result<PathBuf, String> {
    if id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Invalid App installation request".into());
    }
    Ok(root().join("requests").join(format!("{id}.json")))
}

fn source_path(id: &str) -> Result<PathBuf, String> {
    request_path(id)?;
    Ok(root().join("staging").join(id).join("package"))
}

fn prepared(id: &str, owner_uid: u32) -> Result<Prepared, String> {
    let value: Prepared = read_json(&request_path(id)?)?;
    if value.owner_uid != owner_uid || value.request_id != id || value.expires_at <= Utc::now() {
        return Err("This App installation request belongs to another owner or has expired".into());
    }
    if !matches!(value.phase.as_str(), "ready" | "cleanup_pending") {
        return Err(
            "The installation outcome must be inspected before preparing another request".into(),
        );
    }
    Ok(value)
}

pub async fn prepare(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let owner_uid = owner(client).await?;
    let app_id = super::permissions::required_string(&params, "app_id")?;
    let checksum = super::permissions::required_string(&params, "sha256")?;
    if !store::valid_sha256(&checksum) {
        return Err(BrokerError::authorization("Invalid App artifact SHA256"));
    }
    let _guard = SERIALIZER.lock().await;
    expire_requests().map_err(BrokerError::unavailable)?;
    let record = catalog(false)
        .await
        .and_then(|catalog| catalog.select(&app_id, &checksum))
        .map_err(BrokerError::unavailable)?;
    let active = records()
        .map_err(BrokerError::unavailable)?
        .into_iter()
        .filter(|value| matches!(value.phase.as_str(), "ready" | "committing" | "cleanup_pending"))
        .collect::<Vec<_>>();
    if let Some(existing) = active.iter().find(|value| {
        value.owner_uid == owner_uid
            && value.phase == "ready"
            && value.record.app_id == app_id
            && value.record.artifact.sha256 == checksum
    }) {
        let review = super::system_review::show(json!({"id": existing.review_id}), client).await?;
        return Ok(json!({"request_id": existing.request_id, "app_id": app_id,
            "status": "review_required", "review": review}));
    }
    if active.len() >= 16
        || active
            .iter()
            .filter(|value| value.owner_uid == owner_uid)
            .count()
            >= 4
    {
        return Err(BrokerError::unavailable(
            "Finish or cancel a pending App installation before downloading another",
        ));
    }
    if record.artifact.size > store::MAX_DOWNLOAD {
        return Err(BrokerError::unavailable(
            "This App exceeds the current Host's verified payload limit",
        ));
    }
    let http = store::http_client().map_err(BrokerError::unavailable)?;
    let bytes = store::fetch_bytes(&http, &record.download_url(), store::MAX_DOWNLOAD as usize)
        .await
        .map_err(BrokerError::unavailable)?;
    if bytes.len() as u64 != record.artifact.size || store::sha256(&bytes) != checksum {
        return Err(BrokerError::authorization(
            "App download does not match its signed release digest",
        ));
    }
    let request_id = uuid::Uuid::new_v4().simple().to_string();
    let directory = root().join("staging").join(&request_id);
    secure_directory(&directory, 0o755).map_err(BrokerError::unavailable)?;
    let archive = directory.join("archive.tar.gz");
    std::fs::write(&archive, &bytes)
        .map_err(|error| BrokerError::unavailable(error.to_string()))?;
    drop(bytes);
    let source = directory.join("package");
    let selected = record.clone();
    let prepare_id = request_id.clone();
    let result = tokio::task::spawn_blocking(move || {
        store::extract_snapshot(&archive, &source)?;
        let install = DirectoryInstall::prepare(&source, &apps_root())?;
        let app = install.preview()?;
        let package = app.require_verified()?;
        selected.verify_package(package)?;
        let review = reviews::submit(
            owner_uid,
            ReviewKind::AppInstall,
            package,
            format!("App Store install {prepare_id}"),
        )?;
        let pending = Prepared {
            owner_uid,
            request_id: prepare_id.clone(),
            review_id: review.id.clone(),
            record: selected,
            expires_at: Utc::now() + chrono::Duration::hours(1),
            phase: "ready".into(),
        };
        write_json(&request_path(&prepare_id)?, &pending)?;
        Ok::<_, String>(review.id)
    })
    .await
    .map_err(|error| BrokerError::unavailable(error.to_string()))?;
    let review_id = match result {
        Ok(id) => id,
        Err(error) => {
            std::fs::remove_dir_all(&directory).map_err(|cleanup| {
                BrokerError::unavailable(format!("{error}; cleanup failed: {cleanup}"))
            })?;
            return Err(BrokerError::authorization(error));
        }
    };
    let review = super::system_review::show(json!({"id": review_id}), client).await?;
    Ok(json!({
        "request_id": request_id, "app_id": app_id, "status": "review_required",
        "review": review,
    }))
}

pub async fn commit(
    state: &super::state::DaemonState,
    params: Value, client: &ClientIdentity,
) -> Result<Value, BrokerError> {
    let owner_uid = owner(client).await?;
    let id = super::permissions::required_string(&params, "request_id")?;
    let _guard = SERIALIZER.lock().await;
    let mut pending = prepared(&id, owner_uid).map_err(BrokerError::authorization)?;
    catalog(false)
        .await
        .and_then(|catalog| catalog.select(&pending.record.app_id, &pending.record.artifact.sha256))
        .map_err(BrokerError::authorization)?;
    let review = reviews::get(owner_uid, &pending.review_id).map_err(BrokerError::authorization)?;
    if review
        .effective_state()
        .map_err(BrokerError::authorization)?
        != reviews::ReviewState::Approved
    {
        return Err(BrokerError::authorization(
            "Installation requires the owner's current protected OS confirmation",
        ));
    }
    pending.phase = "committing".into();
    write_json(
        &request_path(&id).map_err(BrokerError::authorization)?,
        &pending,
    )
    .map_err(BrokerError::unavailable)?;
    let state = state.clone();
    // The controller owns the transaction after the durable phase change. A disconnected
    // client cannot release its fence while blocking publication or cleanup is still running.
    let controller = tokio::spawn(async move {
        let result = commit_prepared(&state, &pending).await;
        match result {
            Ok(app) => {
                pending.phase = "installed".into();
                write_json(&request_path(&pending.request_id).map_err(BrokerError::indeterminate)?, &pending)
                    .map_err(BrokerError::indeterminate)?;
                std::fs::remove_dir_all(root().join("staging").join(&pending.request_id))
                    .map_err(|error| BrokerError::indeterminate(error.to_string()))?;
                Ok(json!({"status": "installed", "app": app}))
            }
            Err(error) => {
                // Review is still one-use: a consumed review makes this terminal.
                let approved = reviews::get(owner_uid, &pending.review_id)
                    .and_then(|review| review.effective_state()).map_err(BrokerError::indeterminate)?
                    == reviews::ReviewState::Approved;
                pending.phase = if approved {
                    "cleanup_pending".into()
                } else { "failed".into() };
                write_json(&request_path(&pending.request_id).map_err(BrokerError::indeterminate)?, &pending)
                    .map_err(BrokerError::indeterminate)?;
                let message = format!("App installation is not confirmed: {error}. Inspect installed state; cleanup may still be pending.");
                Err(if approved { BrokerError::unavailable(message) } else { BrokerError::indeterminate(message) })
            }
        }
    });
    drop(_guard);
    controller.await.map_err(|error| BrokerError::indeterminate(error.to_string()))?
}

async fn commit_prepared(state: &super::state::DaemonState, pending: &Prepared) -> Result<Value, String> {
    let app_id = pending.record.app_id.clone();
    let source = source_path(&pending.request_id)?;
    let record = pending.record.clone();
    let owner = pending.owner_uid;
    let review = pending.review_id.clone();
    let prepared = tokio::task::spawn_blocking(move || {
        let apps = apps_root();
        let existing = crate::apps::discover(&apps).get(&record.app_id).map(|app| app.dir.clone());
        let install = DirectoryInstall::prepare_at(&source, &apps, existing.as_deref())?;
        record.verify_package(install.preview()?.require_verified()?)?;
        let delivery = DeliveryLock::acquire()?;
        delivery.require_directory_delivery(install.destination())?;
        Ok::<_, String>((install, delivery, record))
    }).await.map_err(|error| error.to_string())??;
    with_mutation(&app_id, |fence| retire_installed(state, &app_id, fence), move |fence| async move {
        tokio::task::spawn_blocking(move || {
            let _fence = fence;
            let (install, delivery, record) = prepared;
            delivery.require_directory_delivery(install.destination())?;
            let mut confirmation = |app: &crate::apps::App| {
                let package = app.require_verified()?;
                record.verify_package(package)?;
                reviews::consume(owner, &review, package)?;
                Ok(())
            };
            install.publish(true, false, &mut confirmation)?;
            let app = super::app_sessions::installed_app(&record.app_id)?;
            let verified = app.require_verified()?;
            record.verify_package(verified)?;
            store::web_metadata(verified)
        }).await.map_err(|error| error.to_string())?
    }).await
}

type MutationFence = std::sync::Arc<tokio::sync::OwnedRwLockWriteGuard<()>>;

pub(crate) async fn with_mutation<T, C, F, P, R>(
    app: &str, retire: C, publish: P,
) -> Result<T, String>
where
    C: FnOnce(MutationFence) -> F,
    F: std::future::Future<Output = Result<(), String>>,
    P: FnOnce(MutationFence) -> R,
    R: std::future::Future<Output = Result<T, String>>,
{
    let fence = std::sync::Arc::new(super::app_mutation::fence(app).await?);
    retire(fence.clone()).await?;
    publish(fence).await
}

async fn retire_installed(state: &super::state::DaemonState, app: &str, fence: MutationFence) -> Result<(), String> {
    #[cfg(not(target_os = "linux"))]
    { let _ = (state, app, fence); Err("Root App mutation requires Linux".into()) }
    #[cfg(target_os = "linux")]
    {
        let services = state.app_service_manager().ok_or("Root App service custody is unavailable")?;
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut completed = services.retire_app(app, deadline).await?;
        let app_id = app.to_string();
        let result = tokio::task::spawn_blocking(move || {
            let _fence = fence;
            completed.extend(super::gui::retire_installed_app(&app_id, deadline)?);
            completed.extend(super::app_execution::retire_app(&app_id, deadline)?);
            let mut owners = Vec::new();
            match std::fs::read_dir("/run/cos/caps") {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("enumerate Root App owners: {error}")),
                Ok(entries) => for entry in entries {
                    let entry = entry.map_err(|error| error.to_string())?;
                    if entry.file_type().map_err(|error| error.to_string())?.is_dir() {
                        let owner = entry.file_name().to_str().and_then(|value| value.parse::<u32>().ok())
                            .ok_or("unexpected owner in Root capability state")?;
                        owners.push(owner);
                    }
                },
            }
            for owner in owners {
                super::authority::authority().revoke_app(owner, &app_id);
                let rows = crate::proc::try_registry_sessions_for_owner(owner)?;
                let instances = crate::provenance::runtime::running_instances(owner)?;
                for row in rows.iter().filter(|row| row.app_id.as_deref() == Some(app_id.as_str())) {
                    if !row.pending_bind && !completed.contains(&(owner, row.session_id.clone())) {
                        return Err(format!("App session {} has no checked Root retirement receipt; stop/recover its legacy Host before updating", row.session_id));
                    }
                }
                for (session, _) in instances.iter().filter(|(_, instance)|
                    instance.package.as_ref().is_some_and(|package|
                        package.kind == crate::provenance::PackageKind::App && package.id == app_id))
                {
                    let pending = rows.iter().any(|row| row.session_id == *session && row.pending_bind);
                    if !pending && !completed.contains(&(owner, session.clone())) {
                        return Err(format!("App instance {session} retains unconfirmed execution custody"));
                    }
                }
                for row in rows.iter().filter(|row| row.app_id.as_deref() == Some(app_id.as_str())) {
                    super::authority::revoke_session_for_owner(&row.session_id, owner);
                    crate::proc::try_deregister_session_for_owner(&row.session_id, owner)?;
                    crate::provenance::runtime::deregister_checked(owner, &row.session_id)?;
                }
            }
            crate::extension_host::spawn::TaskAppData::retire_app_views(&app_id)?;
            Ok(())
        });
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), result).await
            .map_err(|_| "Root App retirement remains pending")?
            .map_err(|error| error.to_string())?
    }
}

struct DeliveryLock(std::fs::File);

impl DeliveryLock {
    fn acquire() -> Result<Self, String> {
        #[cfg(not(target_os = "linux"))]
        { Err("App delivery ownership requires Linux".into()) }
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
            let file = std::fs::OpenOptions::new().read(true).write(true)
                .custom_flags(libc::O_NOFOLLOW).open("/var/lib/dpkg/lock")
                .map_err(|error| format!("cannot protect OS package ownership: {error}; use the OS package updater"))?;
            let metadata = file.metadata().map_err(|error| error.to_string())?;
            if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 || metadata.nlink() != 1 {
                return Err("OS package database lock is not protected".into());
            }
            let lock = libc::flock { l_type: libc::F_WRLCK as _, l_whence: libc::SEEK_SET as _, l_start: 0, l_len: 0, l_pid: 0 };
            if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_OFD_SETLK, &lock) } != 0 {
                return Err("An OS package operation is active; retry Store installation afterwards".into());
            }
            Ok(Self(file))
        }
    }

    fn require_directory_delivery(&self, destination: &Path) -> Result<(), String> {
        use std::io::Read;
        let _held = &self.0;
        let paths = delivery_paths(destination)?;
        let mut query = std::process::Command::new("/usr/bin/dpkg-query")
            .args(["--search", "--"]).args(paths)
            .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null())
            .spawn().map_err(|error| format!("cannot verify OS package ownership: {error}"))?;
        let stdout = query.stdout.take().ok_or("dpkg-query output is unavailable")?;
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout.take(65537).read_to_end(&mut bytes).map(|_| bytes)
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = query.try_wait().map_err(|error| error.to_string())? { break status; }
            if Instant::now() >= deadline {
                let _ = query.kill();
                let _ = query.wait();
                let _ = reader.join();
                return Err("OS package ownership query did not complete".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let output = reader.join().map_err(|_| "OS package ownership reader failed")?
            .map_err(|error| error.to_string())?;
        delivery_owner_result(status.code(), &output)
    }
}

fn delivery_paths(destination: &Path) -> Result<Vec<String>, String> {
    let canonical = if destination.exists() {
        destination.canonicalize()
    } else {
        let parent = destination.parent().ok_or("App destination has no parent")?;
        let name = destination.file_name().ok_or("App destination has no file name")?;
        parent.canonicalize().map(|parent| parent.join(name))
    }.map_err(|error| format!("cannot resolve App delivery ownership: {error}"))?;
    let mut paths = Vec::new();
    for path in [destination, canonical.as_path()] {
        let path = path.to_str().ok_or("App destination must be UTF-8")?;
        if !paths.iter().any(|existing| existing == path) {
            paths.extend([path.to_string(), format!("{path}/*")]);
        }
    }
    Ok(paths)
}

fn delivery_owner_result(status: Option<i32>, output: &[u8]) -> Result<(), String> {
    if !output.is_empty() || status == Some(0) {
        return Err("This App path is owned by an OS package. Update its owning package through the OS package updater (APT), not a directory Store replacement.".into());
    }
    if status != Some(1) {
        return Err("OS package ownership could not be established; directory replacement is refused".into());
    }
    Ok(())
}

pub async fn cancel(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let owner_uid = owner(client).await?;
    let id = super::permissions::required_string(&params, "request_id")?;
    let _guard = SERIALIZER.lock().await;
    let mut pending = prepared(&id, owner_uid).map_err(BrokerError::authorization)?;
    super::system_review::cancel(json!({"id": pending.review_id}), client).await?;
    pending.phase = "cancelled".into();
    write_json(
        &request_path(&id).map_err(BrokerError::authorization)?,
        &pending,
    )
    .map_err(BrokerError::unavailable)?;
    std::fs::remove_dir_all(root().join("staging").join(&id))
        .map_err(|error| BrokerError::unavailable(error.to_string()))?;
    Ok(json!({"status": "cancelled", "request_id": id}))
}

pub async fn open(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let owner_uid = owner(client).await?;
    let id = super::permissions::required_string(&params, "app_id")?;
    let app = super::app_sessions::installed_app(&id).map_err(BrokerError::authorization)?;
    if !reviews::has_accepted(
        owner_uid,
        app.require_verified().map_err(BrokerError::authorization)?,
    )
    .map_err(BrokerError::unavailable)?
    {
        let review = reviews::submit(
            owner_uid,
            ReviewKind::AppActivation,
            app.require_verified().map_err(BrokerError::authorization)?,
            "Web App Host".to_string(),
        )
        .map_err(BrokerError::unavailable)?;
        if review.effective_state().map_err(BrokerError::unavailable)?
            == reviews::ReviewState::Approved
        {
            reviews::consume(
                owner_uid,
                &review.id,
                app.require_verified().map_err(BrokerError::authorization)?,
            )
            .map_err(BrokerError::authorization)?;
        } else {
            return Ok(json!({
                "status": "review_required",
                "review": super::system_review::show(json!({"id": review.id}), client).await?,
            }));
        }
    }
    let package = app.require_verified().map_err(BrokerError::authorization)?;
    let metadata = store::web_metadata(package).map_err(BrokerError::authorization)?;
    Ok(json!({
        "status": "ready", "app": metadata, "package_dir": app.dir,
        "package": PackageRef::of(package),
    }))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/app_store.rs"
    ));
}
