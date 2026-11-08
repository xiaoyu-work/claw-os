//! Official App delivery. Protected human review precedes atomic publication.

use std::path::{Path, PathBuf};
use std::time::Duration;

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
        if record.phase == "ready" && record.expires_at <= Utc::now() {
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
    if value.phase != "ready" {
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
        .filter(|value| matches!(value.phase.as_str(), "ready" | "committing"))
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

pub async fn commit(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
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
    let record = pending.record.clone();
    let source = source_path(&id).map_err(BrokerError::authorization)?;
    let review_id = pending.review_id.clone();
    let result = tokio::task::spawn_blocking(move || {
        let apps = apps_root();
        let existing = crate::apps::discover(&apps)
            .get(&record.app_id)
            .map(|app| app.dir.clone());
        let install = DirectoryInstall::prepare_at(&source, &apps, existing.as_deref())?;
        let mut confirmation = |app: &crate::apps::App| {
            let package = app.require_verified()?;
            record.verify_package(package)?;
            reviews::consume(owner_uid, &review_id, package)?;
            Ok(())
        };
        install.publish(true, false, &mut confirmation)?;
        let app = super::app_sessions::installed_app(&record.app_id)?;
        store::web_metadata(app.require_verified()?)
    })
    .await
    .map_err(|error| BrokerError::unavailable(error.to_string()))?;
    match result {
        Ok(app) => {
            pending.phase = "installed".into();
            write_json(
                &request_path(&id).map_err(BrokerError::authorization)?,
                &pending,
            )
            .map_err(BrokerError::unavailable)?;
            std::fs::remove_dir_all(root().join("staging").join(&id))
                .map_err(|error| BrokerError::unavailable(error.to_string()))?;
            Ok(json!({"status": "installed", "app": app}))
        }
        Err(error) => {
            pending.phase = "failed".into();
            write_json(
                &request_path(&id).map_err(BrokerError::authorization)?,
                &pending,
            )
            .map_err(BrokerError::unavailable)?;
            Err(BrokerError::unavailable(format!(
                "App installation was not confirmed: {error}. Inspect installed state before retrying."
            )))
        }
    }
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
