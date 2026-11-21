use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use crate::clawd::app_store;
use crate::update::debver::Version;

const APP_SOURCE: &str = "/etc/apt/sources.list.d/claw-apps.sources";
const REFRESH_INTERVAL: Duration = Duration::from_secs(60 * 60);
const MIN_ATTEMPT_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Default)]
struct RefreshState {
    attempt: Option<Instant>,
    checked_at: Option<DateTime<Utc>>,
    error: Option<String>,
}

static REFRESH: tokio::sync::Mutex<RefreshState> = tokio::sync::Mutex::const_new(RefreshState {
    attempt: None,
    checked_at: None,
    error: None,
});

pub(crate) fn spawn_refresh() {
    if unsafe { libc::geteuid() } != 0 || !Path::new(APP_SOURCE).is_file() {
        return;
    }
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_secs(30)).await;
        loop {
            match refresh().await {
                Ok(result) if result["error"].is_string() => {
                    tracing::warn!(error = %result["error"], "software index refresh failed");
                }
                Err(error) => tracing::warn!(%error, "software index refresh failed"),
                _ => {}
            }
            tokio::time::sleep(REFRESH_INTERVAL).await;
        }
    });
}

pub(super) async fn refresh() -> Result<Value, String> {
    let mut state = REFRESH
        .try_lock()
        .map_err(|_| "Software indexes are already refreshing")?;
    if state
        .attempt
        .is_none_or(|attempt| attempt.elapsed() >= MIN_ATTEMPT_INTERVAL)
    {
        state.attempt = Some(Instant::now());
        let (apt, catalog) = tokio::join!(
            refresh_apt(),
            tokio::time::timeout(Duration::from_secs(45), app_store::refresh_catalog()),
        );
        let errors = [
            apt.err(),
            match catalog {
                Ok(result) => result.err().map(|error| format!("Claw Apps: {error}")),
                Err(_) => Some("Claw Apps catalog refresh timed out".into()),
            },
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        state.error = (!errors.is_empty()).then(|| errors.join("; "));
        if state.error.is_none() {
            state.checked_at = Some(Utc::now());
        }
    }
    Ok(json!({"checked_at": state.checked_at, "error": state.error}))
}

async fn refresh_apt() -> Result<(), String> {
    // APT index refresh must not race a retained payload plan.
    let _apt = app_store::apt::TRANSACTION
        .try_lock()
        .map_err(|_| "APT is busy; retry after the current package operation".to_string())?;
    app_store::apt::run_bounded(
        "/usr/bin/apt-get",
        [
            "-o",
            "Acquire::AllowInsecureRepositories=false",
            "-o",
            "Acquire::AllowWeakRepositories=false",
            "-o",
            "Acquire::AllowDowngradeToInsecureRepositories=false",
            "-o",
            "Acquire::Check-Valid-Until=true",
            "-o",
            "Acquire::Retries=0",
            "--error-on=any",
            "update",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
        None,
        None,
        Duration::from_secs(75),
    )
    .await
    .map_err(|_| "APT could not refresh every configured source".to_string())?;
    Ok(())
}

pub(super) async fn store(owner: u32) -> Result<Value, String> {
    let catalog = tokio::time::timeout(Duration::from_secs(45), app_store::package_catalog())
        .await
        .map_err(|_| "Claw Apps catalog request timed out")??;
    let installed = super::execute(
        "/usr/bin/dpkg-query",
        &[
            "--show",
            "--showformat=${binary:Package}\t${db:Status-Status}\t${Version}\n",
        ],
        owner,
    )
    .await?;
    if !installed.status.success() {
        return Err("Cannot read installed Debian package versions".into());
    }
    let installed = super::installed_result(&installed.stdout)?;
    let versions: BTreeMap<String, String> = serde_json::from_value(installed["versions"].clone())
        .map_err(|_| "Installed Debian versions are not available")?;
    let architecture = crate::apps::store::architecture();
    let mut apps = Vec::new();
    for record in catalog
        .apps
        .iter()
        .filter(|record| record.desktop_available())
    {
        let package = catalog.apt_package(record)?;
        let installed = versions
            .get(&package.package)
            .or_else(|| versions.get(&format!("{}:{architecture}", package.package)));
        let release = Version::parse(&package.version)?;
        let update = match installed {
            Some(current) => Version::parse(current)? < release,
            None => false,
        };
        apps.push(json!({
            "id": record.app_id, "name": record.display.name, "description": record.display.description,
            "package": package.package, "released": package.version, "installed": installed,
            "update": update,
        }));
    }
    let checked_at = REFRESH.try_lock().ok().and_then(|state| state.checked_at);
    Ok(json!({"apps": apps, "issued_at": catalog.issued_at, "checked_at": checked_at}))
}
