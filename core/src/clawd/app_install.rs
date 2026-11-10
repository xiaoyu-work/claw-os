//! Root-only signed directory publication, separate from official APT delivery.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::approvals::system_review::{self as reviews, ReviewKind, ReviewState};
use crate::apps::installation::{DirectoryInstall, DirectoryPublication};
use crate::apps::{App, AppManifest};
use crate::provenance::runtime::PackageRef;
use crate::provenance::{PackageKind, VerifiedPackage, VerifyOptions};

use super::app_store::{self, DeliveryLock};
use super::client_identity::ClientIdentity;
use super::protocol::BrokerError;
use super::wire::requests::{AppDirectoryInstall, AppDirectoryPublish, AppDirectoryRecover};

const STATE_ROOT: &str = "/var/lib/cos/app-directory-installs";
const STATE_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Retiring,
    CleanupPending,
    Publishing,
    Indeterminate,
    Installed,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    schema_version: u32,
    package: PackageRef,
    request_id: String,
    review_id: String,
    source: PathBuf,
    apps_root: PathBuf,
    destination: PathBuf,
    manifest: AppManifest,
    phase: Phase,
}

fn record_path(app: &str) -> PathBuf {
    Path::new(STATE_ROOT).join(format!(
        "{}.json",
        crate::crypto::sha256_hex(app.as_bytes())
    ))
}

fn read_pending(app: &str) -> Result<Option<Pending>, String> {
    match std::fs::symlink_metadata(STATE_ROOT) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "Root directory installation state is unavailable: {error}"
            ))
        }
        Ok(_) => {}
    }
    let metadata = crate::provenance::fsec::require_secure_location(Path::new(STATE_ROOT), &[0])
        .map_err(|error| error.to_string())?;
    if !metadata.is_dir {
        return Err("Root directory installation state is not a protected directory".into());
    }
    let path = record_path(app);
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
        Ok(_) => {}
    }
    let pending: Pending = app_store::read_json(&path)?;
    if pending.schema_version != STATE_SCHEMA_VERSION
        || pending.package.kind != PackageKind::App
        || pending.package.id != app
        || pending.manifest.id != app
        || pending.request_id.len() != 32
        || !pending
            .request_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("Root directory installation state has an invalid App binding".into());
    }
    Ok(Some(pending))
}

pub(crate) fn pending(app: &str) -> Result<bool, String> {
    Ok(read_pending(app)?.is_some_and(|pending| pending.phase != Phase::Installed))
}

fn save(pending: &Pending, phase: Phase) -> Result<(), String> {
    let mut pending = pending.clone();
    pending.phase = phase;
    app_store::write_json(&record_path(&pending.package.id), &pending)
}

fn destination_paths(pending: &Pending) -> Result<(), String> {
    let configured = app_store::apps_root();
    if !configured.is_absolute()
        || configured
            .canonicalize()
            .map_err(|error| format!("resolve daemon App root: {error}"))?
            != pending.apps_root
    {
        return Err("The daemon App root differs from the original directory installation".into());
    }
    let root = crate::provenance::fsec::require_secure_location(&pending.apps_root, &[0])
        .map_err(|error| format!("daemon App root is not protected: {error}"))?;
    if !root.is_dir {
        return Err("The daemon App root is not a directory".into());
    }
    let relative = pending
        .destination
        .strip_prefix(&pending.apps_root)
        .map_err(|_| "Original App destination is outside its daemon root")?;
    let parts = relative
        .components()
        .map(|component| match component {
            std::path::Component::Normal(part) => {
                part.to_str().ok_or("App destination is not UTF-8")
            }
            _ => Err("Original App destination is not normalized"),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if parts.join("-") != pending.package.id {
        return Err("Original App destination does not retain its installed identity".into());
    }
    let parent = pending
        .destination
        .parent()
        .ok_or("App destination has no parent")?;
    crate::provenance::fsec::require_secure_location(parent, &[0])
        .map_err(|error| format!("App destination parent is not protected: {error}"))?;
    match std::fs::symlink_metadata(&pending.destination) {
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            return Err("App destination is not an ordinary installed directory".into());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
        Ok(_) => {}
    }
    Ok(())
}

fn paths(pending: &Pending) -> Result<(), String> {
    destination_paths(pending)?;
    if pending
        .source
        .canonicalize()
        .map_err(|error| format!("resolve App source: {error}"))?
        != pending.source
    {
        return Err("App source is no longer the reviewed canonical directory".into());
    }
    Ok(())
}

fn require_package(package: &VerifiedPackage, pending: &Pending) -> Result<(), String> {
    if !matches!(
        package.source(),
        crate::provenance::verify::TrustSource::Publisher { .. }
    ) || PackageRef::of(package) != pending.package
    {
        return Err(
            "Directory installation requires the exact reviewed, publisher-signed App".into(),
        );
    }
    let manifest =
        AppManifest::from_json(&package.manifest_text().map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    if serde_json::to_value(manifest).map_err(|error| error.to_string())?
        != serde_json::to_value(&pending.manifest).map_err(|error| error.to_string())?
    {
        return Err("App manifest changed after directory installation review".into());
    }
    Ok(())
}

fn require_review(pending: &Pending) -> Result<(), String> {
    let review = reviews::get(0, &pending.review_id)?;
    if review.kind != ReviewKind::AppInstall
        || review.delivery.is_some()
        || review.source_dir != pending.source
        || review.package != pending.package
        || serde_json::to_value(&review.manifest).map_err(|error| error.to_string())?
            != serde_json::to_value(&pending.manifest).map_err(|error| error.to_string())?
        || review.effective_state()? != ReviewState::Approved
    {
        return Err(
            "Directory installation requires Root's current protected App confirmation".into(),
        );
    }
    Ok(())
}

fn require_publishable(package: &PackageRef) -> Result<(), String> {
    if let Some(current) = read_pending(&package.id)? {
        if matches!(current.phase, Phase::Publishing | Phase::Indeterminate) {
            return Err(format!("An earlier publication has an unchecked outcome; publication cannot be replayed. Use `cos app install --recover {}`", package.id));
        }
        if current.phase != Phase::Installed && current.package != *package {
            return Err("Pending pre-publication retirement belongs to another package".into());
        }
    }
    Ok(())
}

fn canonical_root(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("The App root must be absolute".into());
    }
    match path.canonicalize() {
        Ok(root) => Ok(root),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or("App root has no parent")?;
            let name = path.file_name().ok_or("App root has no directory name")?;
            Ok(parent
                .canonicalize()
                .map_err(|error| format!("resolve App root parent: {error}"))?
                .join(name))
        }
        Err(error) => Err(format!("resolve App root: {error}")),
    }
}

fn prepare_apps_root(expected: &Path) -> Result<PathBuf, String> {
    let root = canonical_root(&app_store::apps_root())?;
    if canonical_root(expected).ok().as_ref() != Some(&root) {
        return Err("A different local App root is unsupported; directory installation uses only the daemon App root".into());
    }
    let parent = root.parent().ok_or("Daemon App root has no parent")?;
    let metadata = crate::provenance::fsec::require_secure_location(parent, &[0])
        .map_err(|error| format!("daemon App root parent is not protected: {error}"))?;
    if !metadata.is_dir {
        return Err("Daemon App root parent is not a directory".into());
    }
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o755);
    }
    match builder.create(&root) {
        Ok(()) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755))
                    .map_err(|error| format!("protect daemon App root: {error}"))?;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(format!("initialize daemon App root: {error}")),
    }
    let metadata = crate::provenance::fsec::require_secure_location(&root, &[0])
        .map_err(|error| format!("daemon App root is not protected: {error}"))?;
    if !metadata.is_dir {
        return Err("Daemon App root is not a directory".into());
    }
    crate::agent::util::sync_dir(&root).map_err(|error| error.to_string())?;
    crate::agent::util::sync_dir(parent).map_err(|error| error.to_string())?;
    Ok(root)
}

fn prepare(
    body: &AppDirectoryPublish,
) -> Result<(DirectoryInstall, DeliveryLock, Pending), String> {
    let expected: PackageRef = serde_json::from_value(body.expected_package.as_value().clone())
        .map_err(|error| format!("invalid expected App package: {error}"))?;
    require_publishable(&expected)?;
    let source = PathBuf::from(body.source.as_str());
    if !source.is_absolute()
        || source
            .canonicalize()
            .map_err(|error| format!("resolve App source: {error}"))?
            != source
    {
        return Err("Directory installation source must be an absolute canonical path".into());
    }
    let apps_root = prepare_apps_root(Path::new(body.expected_apps_root.as_str()))?;
    let existing = crate::apps::discover(&apps_root)
        .remove(&expected.id)
        .map(|app| app.dir);
    let install = DirectoryInstall::prepare_at(&source, &apps_root, existing.as_deref())?;
    let app: App = install.preview()?;
    let pending = Pending {
        schema_version: STATE_SCHEMA_VERSION,
        package: expected,
        request_id: uuid::Uuid::new_v4().simple().to_string(),
        review_id: body.review_id.as_str().to_string(),
        source,
        apps_root,
        destination: install.destination().to_path_buf(),
        manifest: app.manifest.clone(),
        phase: Phase::Retiring,
    };
    paths(&pending)?;
    if !install.is_in_place()
        && (pending.source.starts_with(&pending.destination)
            || pending.destination.starts_with(&pending.source))
    {
        return Err("App source and destination must be separate complete directory trees".into());
    }
    require_package(app.require_verified()?, &pending)?;
    require_review(&pending)?;
    if !body.force && !install.is_in_place() && install.destination().exists() {
        return Err("App destination already exists; use --force to replace it".into());
    }
    let delivery = DeliveryLock::acquire()?;
    delivery.require_directory_delivery(install.destination())?;
    Ok((install, delivery, pending))
}

fn publish(
    install: DirectoryInstall,
    delivery: DeliveryLock,
    pending: &Pending,
    force: bool,
) -> Result<DirectoryPublication, String> {
    paths(pending)?;
    delivery.require_directory_delivery(install.destination())?;
    save(pending, Phase::Publishing)?;
    install.publish(force, false, &mut |candidate| {
        paths(pending)?;
        let package = candidate.require_verified()?;
        require_package(package, pending)?;
        require_review(pending)?;
        reviews::consume(0, &pending.review_id, package)?;
        Ok(())
    })?;
    let published = checked_outcome(pending, &delivery)?;
    save(pending, Phase::Installed)?;
    crate::provenance::audit("provenance.app_installed", published.provenance.clone());
    Ok(published)
}

fn checked_outcome(
    pending: &Pending,
    delivery: &DeliveryLock,
) -> Result<DirectoryPublication, String> {
    destination_paths(pending)?;
    delivery.require_directory_delivery(&pending.destination)?;
    let verified = crate::provenance::verify::verify_package(
        &pending.destination,
        &VerifyOptions::new(PackageKind::App)
            .expect_id(pending.package.id.as_str())
            .signature_only(),
        &crate::provenance::trust_store(),
    )
    .map_err(|error| error.to_string())?;
    require_package(&verified, pending)?;
    crate::apps::installation::require_complete_at(&pending.destination, &pending.package.id)?;
    Ok(DirectoryPublication {
        manifest: pending.manifest.clone(),
        destination: pending.destination.clone(),
        in_place: pending.source == pending.destination,
        provenance: verified.audit_facts(),
        review_id: Some(pending.review_id.clone()),
    })
}

pub(crate) async fn install(
    state: &super::state::DaemonState,
    params: Value,
    client: &ClientIdentity,
) -> Result<Value, BrokerError> {
    if client.uid != Some(0) || client.process_uid() != Some(0) || client.extension_host.is_some() {
        return Err(BrokerError::authorization(
            "Directory publication requires a direct kernel UID 0 peer",
        ));
    }
    super::app_sessions::require_non_app_caller(client)
        .await
        .map_err(BrokerError::authorization)?;
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (state, params);
        return Err(BrokerError::unavailable(
            "Root directory installation requires Linux",
        ));
    }
    #[cfg(target_os = "linux")]
    {
        let pid = client
            .pid
            .ok_or_else(|| BrokerError::authorization("installer PID is unavailable"))?;
        if unsafe { libc::geteuid() } != 0
            || client.start_time_ticks.is_none()
            || crate::proc::read_start_time_ticks_pub(pid) != client.start_time_ticks
        {
            return Err(BrokerError::authorization(
                "Root installer peer identity changed",
            ));
        }
        let body: AppDirectoryInstall = serde_json::from_value(params)
            .map_err(|_| BrokerError::authorization("invalid typed App directory installation"))?;
        match body {
            AppDirectoryInstall::Publish(body) => publish_request(state, body).await,
            AppDirectoryInstall::Recover(body) => recover_request(body).await,
        }
    }
}

async fn publish_request(
    state: &super::state::DaemonState,
    body: AppDirectoryPublish,
) -> Result<Value, BrokerError> {
    let force = body.force;
    let (install, delivery, pending) = tokio::task::spawn_blocking(move || prepare(&body))
        .await
        .map_err(|error| BrokerError::unavailable(error.to_string()))?
        .map_err(BrokerError::authorization)?;
    let state = state.clone();
    let app_id = pending.package.id.clone();
    let controller = tokio::spawn(async move {
        let app = pending.package.id.clone();
        let retiring = pending.clone();
        app_store::with_mutation(
            &app,
            |fence| async move {
                require_publishable(&retiring.package)?;
                paths(&retiring)?;
                require_review(&retiring)?;
                if app_store::delivery_pending(&retiring.package.id)? {
                    return Err("An official App delivery needs checked recovery first".into());
                }
                let directory = Path::new(STATE_ROOT);
                crate::provenance::fsec::require_secure_location(
                    directory
                        .parent()
                        .ok_or("Root installation state has no parent")?,
                    &[0],
                )
                .map_err(|error| error.to_string())?;
                crate::agent::util::ensure_durable_private_dir(directory)
                    .map_err(|error| error.to_string())?;
                app_store::secure_directory(directory, 0o700)?;
                save(&retiring, Phase::Retiring)?;
                match app_store::retire_installed(&state, &retiring.package.id, fence).await {
                    Ok(()) => Ok(()),
                    Err(error) => {
                        save(&retiring, Phase::CleanupPending)?;
                        Err(error)
                    }
                }
            },
            move |fence| async move {
                tokio::task::spawn_blocking(move || {
                    let _fence = fence;
                    match publish(install, delivery, &pending, force) {
                        Ok(published) => {
                            serde_json::to_value(published).map_err(|error| error.to_string())
                        }
                        Err(error) => {
                            save(&pending, Phase::Indeterminate)?;
                            Err(error)
                        }
                    }
                })
                .await
                .map_err(|error| error.to_string())?
            },
        )
        .await
    });
    controller
        .await
        .map_err(|error| publication_error(&app_id, &error.to_string()))?
        .map_err(|error| publication_error(&app_id, &error))
}

fn publication_error(app: &str, error: &str) -> BrokerError {
    BrokerError::indeterminate(format!(
        "Root directory installation is not confirmed: {error}; check the outcome with `cos app install --recover {app}`, never replay uncertain publication"
    ))
}

async fn recover_request(body: AppDirectoryRecover) -> Result<Value, BrokerError> {
    let app = body.app_id.as_str().to_string();
    let expected_root = PathBuf::from(body.expected_apps_root.as_str());
    let controller = tokio::spawn(async move {
        let fence = super::app_mutation::fence(&app).await?;
        tokio::task::spawn_blocking(move || {
            let _fence = fence;
            let pending = read_pending(&app)?.ok_or("No protected directory installation outcome exists for this App")?;
            if matches!(pending.phase, Phase::Retiring | Phase::CleanupPending) {
                return Err("Retirement was not confirmed before publication; only a reviewed retirement retry is permitted".into());
            }
            if !expected_root.is_absolute()
                || expected_root.canonicalize().ok().as_ref() != Some(&pending.apps_root)
            {
                return Err("A different local App root is unsupported for outcome recovery".into());
            }
            if app_store::delivery_pending(&app)? {
                return Err("An official App delivery needs its own checked outcome recovery".into());
            }
            let delivery = DeliveryLock::acquire()?;
            let published = checked_outcome(&pending, &delivery)?;
            save(&pending, Phase::Installed)?;
            serde_json::to_value(published).map_err(|error| error.to_string())
        }).await.map_err(|error| error.to_string())?
    });
    controller.await.map_err(|error| BrokerError::indeterminate(format!(
        "Read-only outcome checking was interrupted: {error}; retry outcome checking only"
    )))?
        .map_err(|error| BrokerError::indeterminate(format!("Read-only directory outcome is not confirmed: {error}; no pending custody was cleared, and this check never authorizes publication replay")))
}
