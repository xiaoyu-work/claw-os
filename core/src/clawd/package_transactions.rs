//! Retained Root package work. Acceptance is not an installed-state receipt.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::caps::{Cap, Scope, Verb};
use crate::proc::SessionInfo;

use super::app_store;
use super::authority::{Authorized, Decision};
use super::client_identity::ClientIdentity;
use super::protocol::BrokerError;
use super::state::DaemonState;

mod backend;
mod impact;
pub(crate) mod stop_review;
#[cfg(test)]
pub(crate) use backend::check_hook_fixture;
#[cfg(test)]
pub(crate) use impact::dependency_fixture;

const ROOT: &str = "/var/lib/cos/package-transactions";
const SCHEMA: u32 = 1;
const MAX_RECORDS: usize = 1000;
static ACCEPT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum Operation {
    Install {
        package: String,
    },
    InstallPackages {
        packages: Vec<String>,
    },
    RemovePackages {
        packages: Vec<String>,
    },
    PurgePackages {
        packages: Vec<String>,
    },
    UpgradePackages {
        packages: Vec<String>,
    },
    InstallVersion {
        package: String,
        version: String,
    },
    Remove {
        package: String,
    },
    Purge {
        package: String,
    },
    Upgrade {
        package: String,
    },
    UpgradeAll,
    Restore {
        package: String,
        previous_version: Option<String>,
        was_held: bool,
        mutation_session: String,
        mutation_seq: u64,
    },
}

impl Operation {
    pub(crate) fn install_packages(mut packages: Vec<String>) -> Result<Self, String> {
        packages.sort();
        packages.dedup();
        if packages.is_empty() || packages.len() > 128 {
            return Err("A package transaction requires 1..128 package names".into());
        }
        for package in &packages {
            super::packages::validate_package_name(package)?;
        }
        Ok(Self::InstallPackages { packages })
    }

    pub(crate) fn from_action(
        action: &str,
        package: Option<&str>,
        version: Option<&str>,
    ) -> Result<Self, String> {
        let package = || {
            package
                .map(str::to_string)
                .ok_or("Missing package".to_string())
        };
        match action {
            "install" => Ok(Self::Install {
                package: package()?,
            }),
            "install-version" => Ok(Self::InstallVersion {
                package: package()?,
                version: version.ok_or("Missing package version")?.to_string(),
            }),
            "remove" => Ok(Self::Remove {
                package: package()?,
            }),
            "purge" => Ok(Self::Purge {
                package: package()?,
            }),
            "upgrade" => Ok(Self::Upgrade {
                package: package()?,
            }),
            "upgrade-all" => Ok(Self::UpgradeAll),
            _ => Err("This is not a payload-mutating package operation".into()),
        }
    }

    fn validate(&self) -> Result<(), String> {
        if let Some(package) = self.package() {
            super::packages::validate_package_name(package)?;
        }
        match self {
            Self::InstallPackages { packages } | Self::RemovePackages { packages }
            | Self::PurgePackages { packages } | Self::UpgradePackages { packages } => {
                let canonical = Self::install_packages(packages.clone())?;
                if canonical.packages() != self.packages() {
                    return Err("The package set is not canonical".into());
                }
                Ok(())
            }
            Self::InstallVersion { version, .. } => super::packages::validate_version(version),
            Self::Restore {
                previous_version,
                mutation_session,
                ..
            } => {
                mutation_session
                    .parse::<crate::session::SessionId>()
                    .map_err(|error| error.to_string())?;
                if let Some(version) = previous_version {
                    super::packages::validate_version(version)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    pub(crate) fn package(&self) -> Option<&str> {
        match self {
            Self::Install { package }
            | Self::InstallVersion { package, .. }
            | Self::Remove { package }
            | Self::Purge { package }
            | Self::Upgrade { package }
            | Self::Restore { package, .. } => Some(package),
            Self::InstallPackages { .. } | Self::RemovePackages { .. }
            | Self::PurgePackages { .. } | Self::UpgradePackages { .. } | Self::UpgradeAll => None,
        }
    }

    fn required_caps(&self) -> Vec<Cap> {
        if let Self::InstallPackages { packages } | Self::RemovePackages { packages }
            | Self::PurgePackages { packages } | Self::UpgradePackages { packages } = self {
            return packages
                .iter()
                .map(|package| Cap::new(Verb::SYS_PACKAGE, Scope::name(package)))
                .collect();
        }
        vec![Cap::new(
            Verb::SYS_PACKAGE,
            self.package().map(Scope::name).unwrap_or(Scope::Wild),
        )]
    }

    fn packages(&self) -> Vec<&str> {
        match self {
            Self::InstallPackages { packages } | Self::RemovePackages { packages }
            | Self::PurgePackages { packages } | Self::UpgradePackages { packages } =>
                packages.iter().map(String::as_str).collect(),
            _ => self.package().into_iter().collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Accepted,
    Planning,
    AwaitingAppConfirmation,
    AwaitingImpactConfirmation,
    Retiring,
    CleanupPending,
    Dispatching,
    Indeterminate,
    Applied,
    NotApplied,
}

impl Phase {
    fn blocks_admission(self) -> bool {
        matches!(
            self,
            Self::Retiring | Self::CleanupPending | Self::Dispatching | Self::Indeterminate
        )
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema_version: u32,
    request_id: String,
    invocation_id: String,
    invocation_binding: String,
    owner_uid: u32,
    session_id: String,
    app_id: Option<String>,
    grant_ref: String,
    operation: Operation,
    phase: Phase,
    accepted_at: String,
    affected_apps: Vec<String>,
    impact: Option<impact::Impact>,
    global_review_id: Option<String>,
    cleanup_started: bool,
    cleanup_confirmed: bool,
    cleanup_inflight: Option<String>,
    retired_apps: Vec<String>,
    children: Vec<Child>,
    plan: Option<backend::Plan>,
    rollback: Option<Value>,
    completion: Option<Value>,
    error: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Child {
    pid: u32,
    start: u64,
    ended: bool,
    #[serde(default)]
    exit_code: Option<i32>,
}

pub(crate) struct AcceptedInvocation<'a>(&'a Record);

impl AcceptedInvocation<'_> {
    pub(crate) fn check(&self, authority: &Decision) -> Result<(), String> {
        let record = self.0;
        if authority.owner_uid() != record.owner_uid
            || authority.session_id() != Some(record.session_id.as_str())
            || authority.grant_ref().as_str() != record.grant_ref
        {
            return Err(
                "The accepted package invocation belongs to different Root authority".into(),
            );
        }
        Ok(())
    }
}

fn active() -> &'static Mutex<HashSet<String>> {
    static ACTIVE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    ACTIVE.get_or_init(Default::default)
}

struct Controller(String);

impl Drop for Controller {
    fn drop(&mut self) {
        if let Ok(mut active) = active().lock() {
            active.remove(&self.0);
        }
    }
}

fn initialize() -> Result<(), String> {
    if unsafe { libc::geteuid() } != 0 {
        return Err("Package transactions require the Root broker".into());
    }
    app_store::secure_directory(Path::new(ROOT), 0o700)?;
    app_store::secure_directory(&Path::new(ROOT).join("requests"), 0o700)?;
    app_store::secure_directory(&Path::new(ROOT).join("global-stop-reviews"), 0o700)
}

fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn path(id: &str) -> Result<PathBuf, String> {
    if !valid_id(id) {
        return Err("Invalid Root package request identity".into());
    }
    Ok(Path::new(ROOT).join("requests").join(format!("{id}.json")))
}

fn read(id: &str) -> Result<Record, String> {
    let record: Record = app_store::read_json(&path(id)?)?;
    if record.schema_version != SCHEMA
        || record.request_id != id
        || !valid_id(&record.invocation_id)
        || record.invocation_binding.len() != 64
        || record.session_id.is_empty()
        || record.grant_ref.is_empty()
    {
        return Err("Invalid protected package transaction binding".into());
    }
    record.operation.validate()?;
    Ok(record)
}

fn records() -> Result<Vec<Record>, String> {
    match std::fs::symlink_metadata(ROOT) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
        Ok(_) => {}
    }
    let metadata = crate::provenance::fsec::require_secure_location(Path::new(ROOT), &[0])
        .map_err(|error| error.to_string())?;
    if !metadata.is_dir {
        return Err("Package transaction state is not a protected directory".into());
    }
    let mut result = Vec::new();
    for entry in
        std::fs::read_dir(Path::new(ROOT).join("requests")).map_err(|error| error.to_string())?
    {
        if result.len() >= MAX_RECORDS {
            return Err(
                "Protected package transaction history reached its bounded capacity".into(),
            );
        }
        let entry = entry.map_err(|error| error.to_string())?;
        let file = entry.path();
        let id = file
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or("Invalid protected package transaction file")?;
        if file != path(id)? {
            return Err("Unexpected file in protected package transaction state".into());
        }
        result.push(read(id)?);
    }
    Ok(result)
}

fn save(record: &Record) -> Result<(), String> {
    app_store::write_json(&path(&record.request_id)?, record)
}

pub(crate) fn pending(app: &str) -> Result<bool, String> {
    for record in records()? {
        if blocks_admission(&record) {
            let impact = record
                .impact
                .as_ref()
                .ok_or("Pending package effect has no protected impact binding")?;
            if impact.global() || record.affected_apps.iter().any(|id| id == app) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn blocks_admission(record: &Record) -> bool {
    record.phase.blocks_admission()
        && record
            .plan
            .as_ref()
            .is_none_or(backend::Plan::payload_effects)
}

pub(crate) fn require_idle(except: Option<&str>) -> Result<(), String> {
    if records()?
        .iter()
        .any(|record| Some(record.request_id.as_str()) != except && blocks_admission(record))
    {
        return Err(
            "Reconcile the retained Root package effect before starting another payload mutation"
                .into(),
        );
    }
    Ok(())
}

pub(crate) fn owns(id: &str) -> Result<bool, String> {
    match std::fs::symlink_metadata(path(id)?) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.to_string()),
        Ok(_) => {
            read(id)?;
            Ok(true)
        }
    }
}

pub(crate) fn apt_started(id: &str, pid: u32, start: u64) -> Result<(), String> {
    let mut record = read(id)?;
    if record.phase != Phase::Dispatching
        || !record.cleanup_confirmed
        || record.children.len() >= 2
        || record.children.iter().any(|child| !child.ended)
    {
        return Err("The Root package child has no checked dispatch custody".into());
    }
    record.children.push(Child {
        pid,
        start,
        ended: false,
        exit_code: None,
    });
    save(&record)
}

pub(crate) fn apt_finished(id: &str, pid: u32, start: u64, exit_code: i32) -> Result<(), String> {
    let mut record = read(id)?;
    let child = record
        .children
        .iter_mut()
        .find(|child| child.pid == pid && child.start == start)
        .ok_or("Root package child completion does not match its retained identity")?;
    if record.phase != Phase::Dispatching || child.ended {
        return Err("Root package child completion cannot be replayed".into());
    }
    child.ended = true;
    child.exit_code = Some(exit_code);
    save(&record)
}

fn completed_children(record: &Record) -> Result<(), String> {
    let plan = record
        .plan
        .as_ref()
        .ok_or("Missing protected package plan")?;
    let expected = usize::from(plan.payload_effects())
        + usize::from(matches!(record.operation, Operation::Restore { .. }));
    if record.children.len() != expected
        || record
            .children
            .iter()
            .any(|child| !child.ended || child.exit_code != Some(0))
    {
        return Err("Successful Root package child completion is unconfirmed; registry absence or installed versions cannot replace it".into());
    }
    Ok(())
}

fn receipt(record: &Record) -> Value {
    json!({
        "status": "accepted",
        "accepted": true,
        "request_id": record.request_id,
        "invocation_id": record.invocation_id,
        "completion": "query_required",
    })
}

fn projection(record: &Record) -> Value {
    let reviews = record
        .plan
        .as_ref()
        .map(|plan| {
            plan.delivery
                .reviews
                .iter()
                .map(|review| json!({"app_id": review.app_id, "review_id": review.review_id}))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({
        "request_id": record.request_id,
        "invocation_id": record.invocation_id,
        "session": record.session_id,
        "app_id": record.app_id,
        "operation": record.operation,
        "phase": record.phase,
        "accepted_at": record.accepted_at,
        "affected_apps": record.affected_apps,
        "app_reviews": reviews,
        "plan": record.plan.as_ref().map(backend::Plan::summary),
        "impact": record.impact.as_ref().map(impact::Impact::summary),
        "global_stop_review_id": record.global_review_id,
        "checked_retired_apps": record.retired_apps,
        "rollback": record.rollback,
        "completion": record.completion,
        "error": record.error,
    })
}

fn owner_record(id: &str, owner: u32) -> Result<Record, BrokerError> {
    let record = read(id).map_err(BrokerError::authorization)?;
    if record.owner_uid != owner {
        return Err(BrokerError::authorization(
            "No package request belongs to this peer",
        ));
    }
    Ok(record)
}

pub(crate) async fn accept(
    state: &DaemonState,
    client: &ClientIdentity,
    authority: &Decision,
    operation: Operation,
    invocation: Option<&str>,
) -> Result<Value, String> {
    let _accept = ACCEPT.lock().await;
    accept_locked(state, client, authority, operation, invocation)
}

pub(crate) fn accept_inverse(
    state: &DaemonState,
    client: &ClientIdentity,
    authority: &Decision,
    operation: Operation,
) -> Result<Value, String> {
    let _accept = ACCEPT
        .try_lock()
        .map_err(|_| "Root package acceptance is busy; no inverse was dispatched")?;
    let Operation::Restore {
        mutation_session,
        mutation_seq,
        ..
    } = &operation
    else {
        return Err("The rollback adapter accepts only a recorded package inverse".into());
    };
    let invocation = crate::crypto::sha256_hex(
        format!("package-inverse:{mutation_session}:{mutation_seq}").as_bytes(),
    );
    accept_locked(state, client, authority, operation, Some(&invocation[..32]))
}

fn accept_locked(
    state: &DaemonState,
    client: &ClientIdentity,
    authority: &Decision,
    operation: Operation,
    invocation: Option<&str>,
) -> Result<Value, String> {
    initialize()?;
    operation.validate()?;
    let owner = client.require_uid()?;
    if authority.owner_uid() != owner {
        return Err("Package authority belongs to another owner".into());
    }
    let session = authority.session()?.clone();
    let invocation = invocation
        .ok_or("Payload requests require an invocation id from the retained package client")?;
    if !valid_id(invocation) {
        return Err("Invalid package invocation correlation".into());
    }
    let grant = authority.grant_ref().as_str();
    let binding = crate::crypto::sha256_hex(
        &serde_json::to_vec(&(owner, &session.session_id, grant, invocation))
            .map_err(|error| error.to_string())?,
    );
    let retained = records()?;
    if let Some(record) = retained
        .iter()
        .find(|record| record.invocation_binding == binding)
    {
        if record.operation != operation
            || record.owner_uid != owner
            || record.session_id != session.session_id
            || record.grant_ref != grant
        {
            return Err(
                "Package invocation correlation conflicts with its retained request".into(),
            );
        }
        // This is a read of an already accepted exact invocation, not a second spend.
        authority.acknowledge_package_acceptance(&AcceptedInvocation(record))?;
        return Ok(receipt(record));
    }
    if let Operation::Restore {
        mutation_session,
        mutation_seq,
        ..
    } = &operation
    {
        if let Some(record) = retained.iter().find(|record| {
            record.phase != Phase::NotApplied
                && matches!(&record.operation,
                Operation::Restore { mutation_session: previous, mutation_seq: seq, .. }
                    if previous == mutation_session && seq == mutation_seq)
        }) {
            let hint = if record.owner_uid == owner {
                record.request_id.as_str()
            } else {
                "owned by another authenticated requester"
            };
            return Err(format!("This inverse already has a protected Root request ({hint}); inspect it instead of replaying"));
        }
    }
    if retained.len() >= MAX_RECORDS {
        return Err(
            "Protected package history must be reconciled before accepting more work".into(),
        );
    }
    let authorized = authority.require_all(&operation.required_caps())?;
    let id = uuid::Uuid::new_v4().simple().to_string();
    let record = Record {
        schema_version: SCHEMA,
        request_id: id.clone(),
        invocation_id: invocation.into(),
        invocation_binding: binding,
        owner_uid: owner,
        session_id: session.session_id.clone(),
        app_id: authority.app_id().map(str::to_string),
        grant_ref: grant.to_string(),
        operation,
        phase: Phase::Accepted,
        accepted_at: chrono::Utc::now().to_rfc3339(),
        affected_apps: Vec::new(),
        impact: None,
        global_review_id: None,
        cleanup_started: false,
        cleanup_confirmed: false,
        cleanup_inflight: None,
        retired_apps: Vec::new(),
        children: Vec::new(),
        plan: None,
        rollback: None,
        completion: None,
        error: None,
    };
    save(&record)?;
    active()
        .lock()
        .map_err(|_| "Root package controller registry is unavailable")?
        .insert(id.clone());
    let response = receipt(&record);
    let state = state.clone();
    let client = client.clone();
    // Nothing from this task is awaited by the requesting App. Its normal accepted
    // result releases ordinary/GUI providers and the outer MCP slot before cleanup drains.
    tokio::spawn(async move {
        let _controller = Controller(id);
        let mut record = record;
        if let Err(error) = execute(&state, &client, &session, authorized, &mut record).await {
            record = match read(&record.request_id) {
                Ok(current) => current,
                Err(state_error) => {
                    tracing::error!(%state_error, %error, request = %record.request_id,
                        "Root package failure could not be recorded; protected evidence retained");
                    return;
                }
            };
            record.error = Some(error);
            record.phase = if matches!(record.phase, Phase::Dispatching | Phase::Indeterminate) {
                Phase::Indeterminate
            } else if record.cleanup_inflight.is_some() && !record.cleanup_confirmed {
                Phase::CleanupPending
            } else {
                Phase::NotApplied
            };
            if let Err(error) = save(&record) {
                tracing::error!(%error, request = %record.request_id,
                    "Root package outcome remains unconfirmed");
            }
        }
    });
    Ok(response)
}

async fn confirmations(record: &mut Record) -> Result<(), String> {
    record.phase = Phase::AwaitingAppConfirmation;
    save(record)?;
    backend::wait_for_reviews(record.owner_uid, record.plan.as_ref().unwrap()).await?;
    let impact = record
        .impact
        .as_ref()
        .ok_or("Missing protected package impact")?;
    if impact.needs_confirmation() {
        if record.global_review_id.is_none() {
            record.global_review_id = Some(stop_review::submit(
                record.owner_uid,
                &record.request_id,
                impact,
            )?);
        }
        record.phase = Phase::AwaitingImpactConfirmation;
        save(record)?;
        stop_review::wait(
            record.owner_uid,
            record.global_review_id.as_deref().unwrap(),
        )
        .await?;
    }
    Ok(())
}

async fn execute(
    state: &DaemonState,
    client: &ClientIdentity,
    session: &SessionInfo,
    authorized: Authorized,
    record: &mut Record,
) -> Result<(), String> {
    record.phase = Phase::Planning;
    save(record)?;
    let prepared =
        backend::prepare(record.owner_uid, &record.request_id, &record.operation).await?;
    record.plan = Some(prepared);
    record.impact =
        Some(impact::inspect(state, record.plan.as_ref().unwrap(), &record.request_id).await?);
    save(record)?;
    let mut refreshes = 0;
    let _publication = loop {
        confirmations(record).await?;
        let publication = app_store::PUBLICATION.lock().await;
        backend::check_prepared(record.plan.as_ref().unwrap())?;
        let current =
            impact::inspect(state, record.plan.as_ref().unwrap(), &record.request_id).await?;
        if current.digest == record.impact.as_ref().unwrap().digest {
            break publication;
        }
        record.impact = Some(current);
        record.global_review_id = None;
        save(record)?;
        drop(publication);
        refreshes += 1;
        if refreshes >= 3 {
            return Err("App/package impact kept changing; no App stop was dispatched".into());
        }
    };
    let transaction = app_store::apt::TRANSACTION.lock().await;
    backend::check_prepared(record.plan.as_ref().unwrap())?;
    let payload = record.plan.as_ref().unwrap().payload_effects();
    if payload {
        require_idle(Some(&record.request_id))?;
    }
    if record.impact.as_ref().unwrap().needs_confirmation() {
        stop_review::require_approved(
            record.owner_uid,
            record
                .global_review_id
                .as_deref()
                .ok_or("Missing separate all-App stop review")?,
            record.impact.as_ref().unwrap(),
        )?;
    }
    backend::check_prepared(record.plan.as_ref().unwrap())?;
    let mut rollback = Vec::new();
    for package in record.operation.packages() {
        let before = super::packages::query_package_state(package).await?;
        if !matches!(
            record.operation,
            Operation::Purge { .. } | Operation::Restore { .. }
        ) {
            rollback.push(
                crate::paths::with_user_override(
                    record.owner_uid,
                    client.require_home_dir()?,
                    async {
                        serde_json::to_value(super::packages::prepare_rollback_record(
                            session, &before,
                        )?)
                        .map_err(|error| error.to_string())
                    },
                )
                .await?,
            );
        }
    }
    if !rollback.is_empty() {
        record.rollback = Some(if rollback.len() == 1 {
            rollback.remove(0)
        } else {
            json!(rollback)
        });
    } else if !matches!(record.operation, Operation::Restore { .. }) {
        record.rollback = Some(json!({
            "available": false,
            "note": if matches!(record.operation, Operation::Purge { .. }) {
                "Purge removes package configuration and requires a system snapshot for complete rollback."
            } else {
                "Global apt index and upgrade operations require a system snapshot for rollback."
            },
        }));
    }
    // Providers may already be queued on the APT mutex. They must drain without
    // this controller holding that mutex (or the dpkg lock) across retirement.
    drop(transaction);
    if payload {
        record.phase = Phase::Retiring;
        let mut apps: BTreeSet<_> = record
            .impact
            .as_ref()
            .unwrap()
            .retirement_apps()
            .iter()
            .cloned()
            .collect();
        apps.extend(
            record
                .plan
                .as_ref()
                .unwrap()
                .delivery
                .reviews
                .iter()
                .map(|review| review.app_id.clone()),
        );
        record.affected_apps = apps.into_iter().collect();
        save(record)?;
    }
    let _admission = if payload && record.impact.as_ref().unwrap().global() {
        Some(super::app_mutation::fence_all().await?)
    } else {
        None
    };
    if payload {
        let current =
            impact::inspect(state, record.plan.as_ref().unwrap(), &record.request_id).await?;
        if current.digest != record.impact.as_ref().unwrap().digest {
            return Err(
                "The confirmed plan/impact changed before any App stop; prepare a new request"
                    .into(),
            );
        }
        if current.needs_confirmation() {
            let _gate = app_store::apt::TRANSACTION.lock().await;
            backend::check_prepared(record.plan.as_ref().unwrap())?;
            stop_review::consume(
                record.owner_uid,
                record
                    .global_review_id
                    .as_deref()
                    .ok_or("Missing separate all-App stop review")?,
                &current,
            )?;
        }
    }
    for app in &record.affected_apps {
        if super::app_install::pending(app)? || app_store::delivery_pending(app)? {
            return Err(
                "Recover the pending checked App publication before package effects".into(),
            );
        }
    }
    for app in &record.affected_apps {
        let fence = std::sync::Arc::new(super::app_mutation::fence(app).await?);
        {
            let _gate = app_store::apt::TRANSACTION.lock().await;
            backend::check_prepared(record.plan.as_ref().unwrap())?;
        }
        record.cleanup_started = true;
        record.cleanup_inflight = Some(app.clone());
        save(record)?;
        app_store::retire_installed(state, app, fence.clone()).await?;
        record.cleanup_inflight = None;
        record.retired_apps.push(app.clone());
        save(record)?;
        // The persisted impact closes these admissions. Do not retain completed
        // writes while another owner's cleanup waits on a different App.
        drop(fence);
    }
    record.cleanup_confirmed = true;
    let _transaction = app_store::apt::TRANSACTION.lock().await;
    let delivery = if payload {
        Some(app_store::DeliveryLock::acquire_for_readonly_recovery()?)
    } else {
        None
    };
    if payload {
        backend::capture_apps(record.plan.as_mut().unwrap())?;
    }
    backend::check_prepared(record.plan.as_ref().unwrap())?;
    record.phase = Phase::Dispatching;
    save(record)?;
    drop(delivery);
    backend::apply(
        &authorized,
        &record.request_id,
        &record.operation,
        record.plan.as_ref().unwrap(),
    )
    .await?;
    *record = read(&record.request_id)?;
    completed_children(record)?;
    let completion = backend::verify(&record.operation, record.plan.as_ref().unwrap()).await?;
    record.completion = Some(completion);
    record.phase = Phase::Applied;
    record.error = None;
    save(record)?;
    Ok(())
}

pub(crate) async fn status(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let owner = client.require_uid().map_err(BrokerError::authorization)?;
    if params.get("request_id").is_some_and(|id| !id.is_null())
        && params.get("invocation_id").is_some_and(|id| !id.is_null())
    {
        return Err(BrokerError::authorization(
            "Select one package request correlation",
        ));
    }
    if let Some(id) = params.get("request_id").and_then(Value::as_str) {
        return Ok(projection(&owner_record(id, owner)?));
    }
    let invocation = params.get("invocation_id").and_then(Value::as_str);
    if invocation.is_some_and(|id| !valid_id(id)) {
        return Err(BrokerError::authorization(
            "Invalid package invocation correlation",
        ));
    }
    Ok(
        json!({"transactions": records().map_err(BrokerError::unavailable)?
        .into_iter().filter(|record| record.owner_uid == owner
            && invocation.is_none_or(|id| record.invocation_id == id))
        .map(|record| projection(&record)).collect::<Vec<_>>()}),
    )
}

pub(crate) fn inverse_completion(
    client: &ClientIdentity,
    session: &crate::session::SessionId,
    seq: u64,
    package: &str,
    version: Option<&str>,
    held: bool,
) -> Result<Option<bool>, String> {
    let owner = client.require_uid()?;
    let expected = Operation::Restore {
        package: package.into(),
        previous_version: version.map(str::to_string),
        was_held: held,
        mutation_session: session.as_str().into(),
        mutation_seq: seq,
    };
    let matching = records()?
        .into_iter()
        .filter(|record| record.owner_uid == owner && record.operation == expected)
        .collect::<Vec<_>>();
    if matching.iter().any(|record| {
        record.phase == Phase::Applied
            && record.cleanup_confirmed
            && record
                .completion
                .as_ref()
                .and_then(|value| value.get("confirmed"))
                .and_then(Value::as_bool)
                == Some(true)
    }) {
        return Ok(Some(true));
    }
    if !matching.is_empty()
        && matching
            .iter()
            .all(|record| record.phase == Phase::NotApplied)
    {
        return Ok(Some(false));
    }
    Ok(None)
}

pub(crate) async fn recover(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let owner = client.require_uid().map_err(BrokerError::authorization)?;
    let id = params
        .get("request_id")
        .and_then(Value::as_str)
        .ok_or_else(|| BrokerError::authorization("Missing Root package request identity"))?;
    let mut record = {
        let _accept = ACCEPT.lock().await;
        let record = owner_record(id, owner)?;
        if active()
            .lock()
            .map_err(|_| BrokerError::unavailable("Root controller registry is unavailable"))?
            .contains(id)
        {
            return Ok(projection(&record));
        }
        record
    };
    let _publication = app_store::PUBLICATION.lock().await;
    let _transaction = app_store::apt::TRANSACTION.lock().await;
    match record.phase {
        Phase::Applied | Phase::NotApplied => {}
        Phase::Accepted
        | Phase::Planning
        | Phase::AwaitingAppConfirmation
        | Phase::AwaitingImpactConfirmation => {
            record.phase = Phase::NotApplied;
            record.error = Some(
                "Root preparation ended before payload dispatch; no effect was replayed".into(),
            );
            save(&record).map_err(BrokerError::indeterminate)?;
        }
        Phase::Retiring | Phase::CleanupPending => {
            if record.cleanup_inflight.is_none() {
                record.phase = Phase::NotApplied;
                record.error = Some(
                    "No App cleanup is unconfirmed and no package payload was dispatched; checked stops are listed separately; no effect was replayed"
                        .into(),
                );
                save(&record).map_err(BrokerError::indeterminate)?;
                return Ok(projection(&record));
            }
            return Err(BrokerError::indeterminate(
                "Root process/Host cleanup remains unconfirmed; read-only recovery cannot retire or infer missing custody",
            ));
        }
        Phase::Dispatching | Phase::Indeterminate => {
            completed_children(&record).map_err(BrokerError::indeterminate)?;
            if !record.cleanup_confirmed {
                return Err(BrokerError::indeterminate(
                    "Root App retirement is not confirmed",
                ));
            }
            let impact = record
                .impact
                .as_ref()
                .ok_or_else(|| BrokerError::indeterminate("Missing protected package impact"))?;
            if impact.needs_confirmation() {
                stop_review::require_consumed(
                    owner,
                    record
                        .global_review_id
                        .as_deref()
                        .ok_or_else(|| BrokerError::indeterminate("Missing all-App stop review"))?,
                    impact,
                )
                .map_err(BrokerError::indeterminate)?;
            }
            let _admission = if impact.global() {
                Some(
                    super::app_mutation::fence_all()
                        .await
                        .map_err(BrokerError::indeterminate)?,
                )
            } else {
                None
            };
            let _delivery = app_store::DeliveryLock::acquire_for_readonly_recovery()
                .map_err(BrokerError::indeterminate)?;
            let plan = record
                .plan
                .as_ref()
                .ok_or_else(|| BrokerError::indeterminate("Missing protected package plan"))?;
            record.completion = Some(
                backend::verify(&record.operation, plan)
                    .await
                    .map_err(BrokerError::indeterminate)?,
            );
            record.phase = Phase::Applied;
            record.error = None;
            save(&record).map_err(BrokerError::indeterminate)?;
        }
    }
    Ok(projection(&record))
}

pub(crate) async fn hook(
    id: &str,
    lines: Vec<String>,
    proof: &app_store::apt::HookContext,
) -> Result<Value, BrokerError> {
    let _ = proof;
    let mut record = read(id).map_err(BrokerError::authorization)?;
    if record.phase != Phase::Dispatching || !record.cleanup_confirmed {
        return Err(BrokerError::authorization(
            "No checked Root package dispatch matches this hook",
        ));
    }
    let impact = record
        .impact
        .as_ref()
        .ok_or_else(|| BrokerError::authorization("Missing protected package impact"))?;
    if impact.needs_confirmation() {
        stop_review::require_consumed(
            record.owner_uid,
            record
                .global_review_id
                .as_deref()
                .ok_or_else(|| BrokerError::authorization("Missing all-App stop review"))?,
            impact,
        )
        .map_err(BrokerError::authorization)?;
    }
    impact::verify_static(
        impact,
        record
            .plan
            .as_ref()
            .ok_or_else(|| BrokerError::authorization("Missing Root package plan"))?,
        id,
    )
    .await
    .map_err(BrokerError::authorization)?;
    let plan = record
        .plan
        .as_mut()
        .ok_or_else(|| BrokerError::authorization("Missing Root package plan"))?;
    backend::gate(record.owner_uid, id, plan, &lines)
        .await
        .map_err(BrokerError::authorization)?;
    save(&record).map_err(BrokerError::indeterminate)?;
    Ok(json!({"proceed": true}))
}
