//! Generic effects use the Store's archive inspector, APT child custody and gate.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::approvals::system_review::{self as reviews, ReviewState};
use crate::apps::{debian, store};
use crate::clawd::app_store::{self, apt};
use crate::clawd::authority::Authorized;
use crate::provenance::runtime::PackageRef;

use super::Operation;

const HOOK: &str = "/usr/lib/cos/bin/claw-app-apt-hook";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Action {
    pub(super) package: String,
    pub(super) old_version: Option<String>,
    pub(super) old_architecture: Option<String>,
    pub(super) version: Option<String>,
    pub(super) architecture: Option<String>,
    pub(super) query: String,
    purge: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RemovedApp {
    pub(super) id: String,
    directory: PathBuf,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Plan {
    pub(super) delivery: apt::Plan,
    pub(super) actions: Vec<Action>,
    pub(super) removed_apps: Vec<RemovedApp>,
    baseline: Option<AppBaseline>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppBaseline {
    verified: BTreeMap<String, PackageRef>,
    quarantined: BTreeSet<String>,
}

impl Plan {
    pub(super) fn payload_effects(&self) -> bool {
        !self.actions.is_empty()
    }

    pub(super) fn summary(&self) -> Value {
        json!({
            "digest": self.delivery.digest,
            "actions": self.actions,
            "packages": self.delivery.packages.iter().map(|package| json!({
                "package": package.identity.package,
                "version": package.identity.version,
                "architecture": package.identity.architecture,
                "size": package.size,
                "sha256": package.sha256,
                "app_channel": package.inventory.is_some(),
            })).collect::<Vec<_>>(),
        })
    }
}

pub(super) fn cache(id: &str) -> Result<PathBuf, String> {
    Ok(apt::directory(id)?.join("archives"))
}

fn arguments(operation: &Operation, id: &str, dispatch: bool) -> Result<Vec<String>, String> {
    let mut args = vec![
        "-o".into(),
        format!("Dir::Cache::archives={}", cache(id)?.display()),
        "-o".into(),
        "APT::Sandbox::User=root".into(),
        "-o".into(),
        "APT::Get::AllowUnauthenticated=false".into(),
        "-o".into(),
        "Acquire::AllowInsecureRepositories=false".into(),
        "-o".into(),
        "Acquire::AllowDowngradeToInsecureRepositories=false".into(),
    ];
    if dispatch {
        args.extend([
            "-o".into(),
            format!("DPkg::Pre-Install-Pkgs::=exec {HOOK}"),
            "-o".into(),
            "DPkg::Tools::Options::exec::Version=3".into(),
            "--no-download".into(),
        ]);
    }
    let (action, package, version) = match operation {
        Operation::Install { package } => ("install", Some(package.as_str()), None),
        Operation::InstallPackages { .. } => ("install", None, None),
        Operation::InstallVersion { package, version } => (
            "install-version",
            Some(package.as_str()),
            Some(version.as_str()),
        ),
        Operation::Remove { package } => ("remove", Some(package.as_str()), None),
        Operation::Purge { package } => ("purge", Some(package.as_str()), None),
        Operation::Upgrade { package } => ("upgrade", Some(package.as_str()), None),
        Operation::UpgradeAll => ("upgrade-all", None, None),
        Operation::Restore {
            package,
            previous_version,
            ..
        } => (
            if previous_version.is_some() {
                "install-version"
            } else {
                "remove"
            },
            Some(package.as_str()),
            previous_version.as_deref(),
        ),
    };
    match action {
        "install" | "install-version" | "upgrade" => {
            args.extend(["install".into(), "--yes".into()]);
            if action != "upgrade" {
                args.push("--no-install-recommends".into());
            }
            if action == "install-version" {
                args.push("--allow-downgrades".into());
            }
            if action == "upgrade" {
                args.push("--only-upgrade".into());
            }
        }
        "remove" | "purge" => args.extend([action.into(), "--yes".into()]),
        "upgrade-all" => args.extend(["upgrade".into(), "--yes".into()]),
        _ => return Err("Invalid closed package operation".into()),
    }
    args.push("--".into());
    if let Some(package) = package {
        args.push(match version {
            Some(version) => format!("{package}={version}"),
            None => package.to_string(),
        });
    } else if let Operation::InstallPackages { packages } = operation {
        args.extend(packages.iter().cloned());
    }
    Ok(args)
}

fn architecture(value: &str) -> Result<String, String> {
    crate::clawd::packages::validate_package_name(&format!("fixture:{value}"))?;
    Ok(value.into())
}

fn version(value: &str) -> Result<Option<String>, String> {
    if value == "-" {
        return Ok(None);
    }
    crate::clawd::packages::validate_version(value)?;
    Ok(Some(value.into()))
}

async fn simulation(output: &str, purge: bool) -> Result<Vec<Action>, String> {
    let mut actions = BTreeMap::new();
    for line in output.lines() {
        let Some((kind, rest)) = line.split_once(' ') else {
            continue;
        };
        if !matches!(kind, "Inst" | "Remv" | "Purg") {
            continue;
        }
        if actions.len() >= 512 {
            return Err("APT effect plan exceeds its bounded package count".into());
        }
        let (query, rest) = rest.split_once(' ').unwrap_or((rest, ""));
        crate::clawd::packages::validate_package_name(query)?;
        let package = query.split(':').next().unwrap().to_string();
        let mut rest = rest.trim();
        let old_version = if let Some(previous) = rest.strip_prefix('[') {
            let (previous, remaining) = previous
                .split_once(']')
                .ok_or("APT simulation omitted the previous version boundary")?;
            rest = remaining.trim();
            version(previous)?
        } else {
            None
        };
        let old_architecture = if old_version.is_some() {
            let (_, actual, architecture) = crate::clawd::packages::query_package_status(query)
                .await?
                .ok_or("APT simulation refers to a missing installed package")?;
            if Some(&actual) != old_version.as_ref() {
                return Err("The installed package changed during concrete planning".into());
            }
            Some(architecture)
        } else {
            None
        };
        let (new_version, new_architecture) = if kind == "Inst" {
            let body = rest
                .strip_prefix('(')
                .and_then(|value| value.strip_suffix(')'))
                .ok_or("APT simulation omitted its concrete install version")?;
            let selected = body
                .split_whitespace()
                .next()
                .ok_or("Missing APT version")?;
            let arch = body
                .rsplit_once('[')
                .and_then(|(_, value)| value.strip_suffix(']'))
                .ok_or("APT simulation omitted its concrete install architecture")?;
            (version(selected)?, Some(architecture(arch)?))
        } else {
            (None, None)
        };
        let qualifier = new_architecture.as_deref().or(old_architecture.as_deref());
        let query_name = match qualifier {
            Some("all") => package.clone(),
            Some(architecture) => format!("{package}:{architecture}"),
            None => query.to_string(),
        };
        let action = Action {
            package,
            old_version,
            old_architecture,
            version: new_version,
            architecture: new_architecture,
            query: query_name,
            purge: purge || kind == "Purg",
        };
        if actions.insert(query.to_string(), action).is_some() {
            return Err("APT simulation contains duplicate package effects".into());
        }
    }
    Ok(actions.into_values().collect())
}

fn empty_os_catalog() -> store::Catalog {
    let now = chrono::Utc::now();
    store::Catalog {
        schema: "claw.app-store/v2".into(),
        repository: store::REPOSITORY.into(),
        runtime_abi: 1,
        generation: "0".into(),
        issued_at: now,
        expires_at: now,
        apps: Vec::new(),
        packages: Vec::new(),
    }
}

async fn contains_app_payload(path: &Path) -> Result<bool, String> {
    let output = apt::run(
        "/usr/bin/dpkg-deb",
        vec![
            "--contents".into(),
            path.to_str().ok_or("Invalid archive path")?.into(),
        ],
        None,
        None,
    )
    .await?;
    for line in output.stdout_string().lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 6 {
            return Err("Cannot classify a concrete Debian payload inventory".into());
        }
        let path = fields[5].trim_start_matches("./");
        if !fields[0].starts_with('d')
            && (path == "usr/lib/cos/apps" || path.starts_with("usr/lib/cos/apps/"))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn removed_apps(actions: &[Action]) -> Result<Vec<RemovedApp>, String> {
    let removed: Vec<_> = actions
        .iter()
        .filter(|action| action.version.is_none())
        .collect();
    if removed.is_empty() {
        return Ok(Vec::new());
    }
    let discovered = crate::apps::discover_all(&app_store::apps_root());
    let mut result = Vec::new();
    for (id, app) in discovered.all() {
        let manifest = app.dir.join("app.json");
        let query = crate::clawd::packages::query_package_owners(&manifest).await?;
        let mut affected = false;
        for owner in query {
            let name = owner.split(':').next().unwrap_or(&owner);
            if !removed.iter().any(|action| action.package == name) {
                continue;
            }
            let status = crate::clawd::packages::query_package_status(&owner).await?;
            if removed.iter().any(|action| {
                action.package == name
                    && action.old_architecture.as_ref().is_none_or(|arch| {
                        status.as_ref().is_some_and(|(_, _, actual)| actual == arch)
                    })
            }) {
                affected = true;
            }
        }
        if affected {
            result.push(RemovedApp {
                id,
                directory: app.dir,
            });
        }
    }
    Ok(result)
}

pub(super) async fn prepare(owner: u32, id: &str, operation: &Operation) -> Result<Plan, String> {
    let _transaction = apt::TRANSACTION.lock().await;
    app_store::initialize()?;
    let directory = apt::directory(id)?;
    app_store::secure_directory(&directory, 0o755)?;
    let archives = cache(id)?;
    app_store::secure_directory(&archives, 0o700)?;
    app_store::secure_directory(&archives.join("partial"), 0o700)?;
    let audit = apt::run("/usr/bin/dpkg", vec!["--audit".into()], None, None).await?;
    if !audit.stdout.is_empty() {
        return Err(
            "Repair the incomplete package transaction before requesting more effects".into(),
        );
    }
    let before = apt::status_digest()?;
    let mut simulated = arguments(operation, id, false)?;
    simulated.insert(0, "--simulate".into());
    let output = apt::run("/usr/bin/apt-get", simulated, None, None).await?;
    let actions = simulation(
        &output.stdout_string(),
        matches!(operation, Operation::Purge { .. }),
    )
    .await?;
    let controller = std::env::current_exe()
        .map_err(|error| format!("The active package controller cannot be identified: {error}"))?;
    let controller_packages = crate::clawd::packages::query_package_owners(&controller).await?;
    if actions.iter().any(|action| controller_packages.iter().any(|owner|
        owner.split(':').next() == Some(action.package.as_str())))
    {
        return Err(
            "This plan changes the active OS package controller and requires an independently retained OS updater; no payload was dispatched"
                .into(),
        );
    }
    if actions.iter().any(|action| action.version.is_some()) {
        let mut budget = arguments(operation, id, false)?;
        budget.splice(0..0, ["--print-uris".into(), "--download-only".into()]);
        let output = apt::run("/usr/bin/apt-get", budget, None, None).await?;
        apt::download_budget(&output.stdout_string())?;
        let mut download = arguments(operation, id, false)?;
        download.insert(0, "--download-only".into());
        apt::run("/usr/bin/apt-get", download, None, None).await?;
    }
    if before != apt::status_digest()? {
        return Err("Installed packages changed during planning; no payload was dispatched".into());
    }
    let mut files = std::fs::read_dir(&archives)
        .map_err(|error| error.to_string())?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    files.retain(|file| file.extension().is_some_and(|extension| extension == "deb"));
    files.sort();
    if files.len() > 512 {
        return Err("APT prepared too many archives".into());
    }
    let mut packages = Vec::new();
    let mut payloads = Vec::new();
    for file in &files {
        let identity = apt::identity(file).await?;
        let (size, sha256) = debian::file_binding(file)?;
        packages.push(apt::Package {
            file: file
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or("Invalid archive name")?
                .into(),
            identity,
            size,
            sha256,
            inventory: None,
        });
        payloads.push(contains_app_payload(file).await?);
    }
    let os_sources = if packages.is_empty() {
        Vec::new()
    } else {
        apt::authenticated_sources(&packages).await?
    };
    let catalog = if os_sources.iter().any(|os| !os) || payloads.iter().any(|app| *app) {
        app_store::package_catalog().await?
    } else {
        empty_os_catalog()
    };
    for (package, app_payload) in packages.iter().zip(&payloads) {
        if *app_payload
            && !catalog.packages.iter().any(|item| {
                item.package == package.identity.package
                    && item.version == package.identity.version
                    && item.architecture == package.identity.architecture
                    && item.sha256 == package.sha256
            })
        {
            return Err(
                "App payloads require the authenticated App channel's exact Debian inventory"
                    .into(),
            );
        }
    }
    let context = serde_json::to_value(&actions).map_err(|error| error.to_string())?;
    let delivery =
        apt::prepare_packages(owner, id, None, &catalog, before, files, Some(&context)).await?;
    let installs: BTreeSet<_> = actions
        .iter()
        .filter_map(|action| {
            action.version.as_ref().map(|version| {
                (
                    action.package.clone(),
                    version.clone(),
                    action.architecture.clone().unwrap(),
                )
            })
        })
        .collect();
    let downloaded: BTreeSet<_> = delivery
        .packages
        .iter()
        .map(|package| {
            (
                package.identity.package.clone(),
                package.identity.version.clone(),
                package.identity.architecture.clone(),
            )
        })
        .collect();
    if installs != downloaded || downloaded.len() != delivery.packages.len() {
        return Err("APT archives differ from the complete concrete resolver plan".into());
    }
    let removed_apps = removed_apps(&actions).await?;
    Ok(Plan {
        delivery,
        actions,
        removed_apps,
        baseline: None,
    })
}

pub(super) fn capture_apps(plan: &mut Plan) -> Result<(), String> {
    let discovered = crate::apps::discover_all(&app_store::apps_root());
    let mut verified = BTreeMap::new();
    for (id, app) in discovered.verified {
        verified.insert(id, PackageRef::of(app.require_verified()?));
    }
    plan.baseline = Some(AppBaseline {
        verified,
        quarantined: discovered.quarantined.into_keys().collect(),
    });
    Ok(())
}

pub(super) async fn wait_for_reviews(owner: u32, plan: &Plan) -> Result<(), String> {
    for _ in 0..3600 {
        let mut approved = true;
        for review in &plan.delivery.reviews {
            match reviews::get(owner, &review.review_id)?.effective_state()? {
                ReviewState::Approved => {}
                ReviewState::Pending => approved = false,
                _ => {
                    return Err(
                        "Protected App delivery was denied, expired or already consumed".into(),
                    )
                }
            }
        }
        if approved {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err("Protected App delivery confirmation expired before package dispatch".into())
}

pub(super) fn check_prepared(plan: &Plan) -> Result<(), String> {
    if plan.delivery.hook_consumed || plan.delivery.status_sha256 != apt::status_digest()? {
        return Err("The concrete package plan changed or was already dispatched; effects cannot be replayed".into());
    }
    Ok(())
}

pub(super) async fn apply(
    _authorized: &Authorized,
    id: &str,
    operation: &Operation,
    plan: &Plan,
) -> Result<(), String> {
    if !plan.actions.is_empty() {
        apt::run(
            "/usr/bin/apt-get",
            arguments(operation, id, true)?,
            Some(id.into()),
            None,
        )
        .await?;
        let record = super::read(id)?;
        if !record
            .plan
            .as_ref()
            .is_some_and(|plan| plan.delivery.hook_consumed)
        {
            return Err("No protected Root pre-dpkg gate confirmed this package dispatch".into());
        }
    }
    if let Operation::Restore {
        package, was_held, ..
    } = operation
    {
        apt::run(
            "/usr/bin/apt-mark",
            vec![
                if *was_held { "hold" } else { "unhold" }.into(),
                "--".into(),
                package.clone(),
            ],
            Some(id.into()),
            None,
        )
        .await?;
    }
    Ok(())
}

fn hook_actions(lines: &[String]) -> Result<Vec<(Action, String)>, String> {
    if lines.len() > 4096 || lines.first().map(String::as_str) != Some("VERSION 3") {
        return Err("Package dispatch requires the bounded APT v3 action protocol".into());
    }
    let separator = lines
        .iter()
        .position(String::is_empty)
        .ok_or("APT omitted its action-list boundary")?;
    let mut actions = Vec::new();
    for line in &lines[separator + 1..] {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 9
            || !matches!(fields[4], "<" | ">" | "=")
            || !matches!(fields[3], "same" | "foreign" | "allowed" | "none" | "no")
            || !matches!(fields[7], "same" | "foreign" | "allowed" | "none" | "no")
        {
            return Err("APT supplied an invalid concrete action".into());
        }
        crate::clawd::packages::validate_package_name(fields[0])?;
        let old_version = version(fields[1])?;
        let new_version = version(fields[5])?;
        let old_architecture = if old_version.is_some() {
            Some(architecture(fields[2])?)
        } else if fields[2] == "-" {
            None
        } else {
            return Err("Unexpected old architecture".into());
        };
        let new_architecture = if new_version.is_some() {
            Some(architecture(fields[6])?)
        } else if fields[6] == "-" {
            None
        } else {
            return Err("Unexpected new architecture".into());
        };
        actions.push((
            Action {
                package: fields[0].into(),
                old_version,
                old_architecture,
                version: new_version,
                architecture: new_architecture,
                query: String::new(),
                purge: false,
            },
            fields[8].into(),
        ));
        if actions.len() > 1536 {
            return Err("APT supplied too many concrete actions".into());
        }
    }
    Ok(actions)
}

fn same_effect(left: &Action, right: &Action) -> bool {
    left.package == right.package
        && left.old_version == right.old_version
        && left.old_architecture == right.old_architecture
        && left.version == right.version
        && left.architecture == right.architecture
}

pub(super) async fn gate(
    owner: u32,
    id: &str,
    plan: &mut Plan,
    lines: &[String],
) -> Result<(), String> {
    check_prepared(plan)?;
    let actual = hook_actions(lines)?;
    let archives = cache(id)?;
    let mut unpacked = BTreeSet::new();
    let mut removed = BTreeSet::new();
    for (action, target) in actual {
        let index = plan
            .actions
            .iter()
            .position(|expected| same_effect(expected, &action))
            .ok_or("APT changed the reviewed dependency/removal plan")?;
        if target == "**REMOVE**" {
            if action.version.is_some() || !removed.insert(index) {
                return Err("APT supplied an unreviewed or duplicate removal".into());
            }
        } else if target == "**CONFIGURE**" {
            if action.version.is_none() {
                return Err("APT supplied an unreviewed configure action".into());
            }
        } else {
            if action.version.is_none() || !unpacked.insert(index) {
                return Err("APT supplied an unreviewed or duplicate unpack action".into());
            }
            let archive = plan
                .delivery
                .packages
                .iter()
                .find(|package| {
                    package.identity.package == action.package
                        && Some(&package.identity.version) == action.version.as_ref()
                        && Some(&package.identity.architecture) == action.architecture.as_ref()
                })
                .ok_or("APT unpack has no authenticated archive")?;
            let expected = archives.join(&archive.file);
            if Path::new(&target)
                .canonicalize()
                .map_err(|error| error.to_string())?
                != expected
                || debian::file_binding(&expected)? != (archive.size, archive.sha256.clone())
            {
                return Err(
                    "APT substituted bytes or an archive outside the protected plan".into(),
                );
            }
        }
    }
    if unpacked.len() + removed.len() != plan.actions.len()
        || unpacked.len() != plan.delivery.packages.len()
    {
        return Err("APT omitted a concrete reviewed package effect".into());
    }
    let sources = if plan.delivery.packages.is_empty() {
        Vec::new()
    } else {
        apt::authenticated_sources(&plan.delivery.packages).await?
    };
    if plan
        .delivery
        .packages
        .iter()
        .zip(sources)
        .any(|(package, os)| package.inventory.is_none() && !os)
    {
        return Err("A dependency lost its authenticated OS/App source binding".into());
    }
    if plan
        .delivery
        .packages
        .iter()
        .any(|package| package.inventory.is_some())
    {
        let current = app_store::package_catalog().await?;
        if plan
            .delivery
            .packages
            .iter()
            .filter_map(|package| package.inventory.as_ref())
            .any(|package| !current.packages.contains(package))
        {
            return Err(
                "An authenticated App-channel package changed or disappeared before dpkg".into(),
            );
        }
    }
    for review in &plan.delivery.reviews {
        let package = apt::verify_source(review)?;
        crate::clawd::trust_withdrawal::check_package(owner, &package)?;
        reviews::consume_delivery(owner, &review.review_id, &package, &plan.delivery.digest)?;
    }
    plan.delivery.hook_consumed = true;
    Ok(())
}

async fn verify_action(action: &Action) -> Result<(), String> {
    let state = crate::clawd::packages::query_package_status(&action.query).await?;
    if let Some(version) = &action.version {
        let state = state.ok_or("APT omitted a planned installed package")?;
        if !matches!(
            state.0.as_str(),
            "install ok installed" | "hold ok installed"
        ) || state.1 != *version
            || Some(&state.2) != action.architecture.as_ref()
        {
            return Err("APT did not confirm the exact concrete installed state".into());
        }
    } else if let Some((status, _, _)) = state {
        let allowed = status == "deinstall ok config-files" && !action.purge
            || status.ends_with(" ok not-installed");
        if !allowed {
            return Err("APT did not confirm the exact concrete removed/purged state".into());
        }
    }
    Ok(())
}

pub(super) async fn verify(operation: &Operation, plan: &Plan) -> Result<Value, String> {
    if !plan.actions.is_empty() && !plan.delivery.hook_consumed {
        return Err("No protected Root dispatch receipt exists; recovery cannot replay APT".into());
    }
    let audit = apt::run("/usr/bin/dpkg", vec!["--audit".into()], None, None).await?;
    if !audit.stdout.is_empty() {
        return Err("The package database still has incomplete effects".into());
    }
    for action in &plan.actions {
        verify_action(action).await?;
    }
    for review in &plan.delivery.reviews {
        let app = crate::clawd::app_sessions::installed_app(&review.app_id)?;
        if PackageRef::of(app.require_verified()?) != review.package {
            return Err(
                "Installed App bytes differ from their protected delivery confirmation".into(),
            );
        }
    }
    for removed in &plan.removed_apps {
        if plan
            .delivery
            .reviews
            .iter()
            .any(|review| review.app_id == removed.id)
        {
            continue;
        }
        match std::fs::symlink_metadata(&removed.directory) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
            Ok(_) => {
                return Err("Removed App payload still exists; admission remains blocked".into())
            }
        }
    }
    if plan.payload_effects() {
        let baseline = plan
            .baseline
            .as_ref()
            .ok_or("Missing protected pre-dispatch App inventory")?;
        let current = crate::apps::discover_all(&app_store::apps_root());
        let delivered: BTreeSet<_> = plan
            .delivery
            .reviews
            .iter()
            .map(|review| review.app_id.as_str())
            .collect();
        let removed: BTreeSet<_> = plan
            .removed_apps
            .iter()
            .map(|app| app.id.as_str())
            .collect();
        for (id, package) in &baseline.verified {
            if delivered.contains(id.as_str()) || removed.contains(id.as_str()) {
                continue;
            }
            let current = current.verified.get(id).ok_or(
                "An unreviewed App was removed, changed or quarantined by package effects",
            )?;
            if PackageRef::of(current.require_verified()?) != *package {
                return Err(
                    "Package effects changed an App outside the protected delivery plan".into(),
                );
            }
        }
        if current
            .verified
            .keys()
            .any(|id| !baseline.verified.contains_key(id) && !delivered.contains(id.as_str()))
        {
            return Err(
                "Package effects introduced an App without protected delivery confirmation".into(),
            );
        }
        let expected_quarantine: BTreeSet<_> = baseline
            .quarantined
            .iter()
            .filter(|id| !removed.contains(id.as_str()) && !delivered.contains(id.as_str()))
            .cloned()
            .collect();
        let actual_quarantine: BTreeSet<_> = current.quarantined.into_keys().collect();
        if actual_quarantine != expected_quarantine {
            return Err("Unreviewed App quarantine state changed during package effects".into());
        }
    }
    let after = match operation.package() {
        Some(package) => Some(crate::clawd::packages::query_package_state(package).await?),
        None => None,
    };
    let mut installed = Vec::new();
    if let Operation::InstallPackages { packages } = operation {
        for package in packages {
            let state = crate::clawd::packages::query_package_state(package).await?;
            if !state.installed {
                return Err("A requested package is not confirmed installed".into());
            }
            installed.push(state);
        }
    }
    if let Operation::Restore {
        previous_version,
        was_held,
        ..
    } = operation
    {
        let after = after.as_ref().ok_or("Missing restored package state")?;
        if after.version != *previous_version || after.held != *was_held {
            return Err("The recorded inverse package/hold state is not confirmed".into());
        }
    }
    if let Some(after) = &after {
        let expected = match operation {
            Operation::Install { .. } => after.installed,
            Operation::InstallVersion { version, .. } => {
                after.installed && after.version.as_ref() == Some(version)
            }
            Operation::Remove { .. } | Operation::Purge { .. } => !after.installed,
            _ => true,
        };
        if !expected {
            return Err("The requested package outcome is not confirmed".into());
        }
        if matches!(operation, Operation::Purge { .. }) {
            let status = crate::clawd::packages::query_package_status(&after.package).await?;
            if status.is_some_and(|(status, _, _)| !status.ends_with(" ok not-installed")) {
                return Err("The requested purge still retains package configuration".into());
            }
        }
    }
    Ok(json!({
        "status": "applied",
        "confirmed": true,
        "after": after,
        "packages": installed,
        "already_satisfied": plan.actions.is_empty(),
    }))
}

#[cfg(test)]
pub(crate) fn check_hook_fixture(lines: Vec<String>) -> Result<usize, String> {
    hook_actions(&lines).map(|actions| actions.len())
}
