//! Checked Root reconciliation after an owner's independent protective policy write.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::provenance::runtime::{InstanceClass, PackageRef};
use crate::provenance::withdrawal::{OwnerPolicy, Target};
use crate::provenance::{PackageKind, VerifiedPackage};

use super::client_identity::ClientIdentity;
use super::protocol::BrokerError;
use super::state::DaemonState;

const STATE_ROOT: &str = "/var/lib/cos/trust-withdrawals";
const MAX_RECORDS: usize = 256;
const MAX_TARGETS: usize = 1024;
const MAX_RECORD_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", try_from = "RequestBody")]
pub enum Request {
    Reconcile { target: Target },
    Status { request_id: String },
    Recover { request_id: String },
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum RequestBody {
    Reconcile {
        target: Target,
    },
    Status {
        request_id: super::wire::bounded::Token<32>,
    },
    Recover {
        request_id: super::wire::bounded::Token<32>,
    },
}

impl TryFrom<RequestBody> for Request {
    type Error = String;
    fn try_from(body: RequestBody) -> Result<Self, String> {
        let request = match body {
            RequestBody::Reconcile { target } => Self::Reconcile { target },
            RequestBody::Status { request_id } => Self::Status {
                request_id: request_id.as_str().to_string(),
            },
            RequestBody::Recover { request_id } => Self::Recover {
                request_id: request_id.as_str().to_string(),
            },
        };
        request.validate()?;
        Ok(request)
    }
}

impl Request {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Reconcile { target } => target.validate(),
            Self::Status { request_id } | Self::Recover { request_id } => valid_id(request_id),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Binding {
    pub owner: u32,
    pub app: String,
    pub package: Option<PackageRef>,
    pub trust_context_uid: Option<u32>,
    pub context_generation_invalidated: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Selecting,
    Retiring,
    Indeterminate,
    Retired,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingApp {
    owner: u32,
    app: String,
    retired: bool,
    supporting_evidence_missing: bool,
    packages: Vec<PackageRef>,
    context_generation_closure: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    schema_version: u32,
    request_id: String,
    requester: u32,
    root_epoch: String,
    target: Target,
    source_generation: Option<u64>,
    source_fingerprint: Option<String>,
    phase: Phase,
    apps: Vec<PendingApp>,
    error: Option<String>,
}

fn epoch() -> &'static str {
    static EPOCH: OnceLock<String> = OnceLock::new();
    EPOCH.get_or_init(|| uuid::Uuid::new_v4().simple().to_string())
}

fn active() -> &'static Mutex<HashSet<String>> {
    static ACTIVE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    ACTIVE.get_or_init(Default::default)
}

fn valid_id(id: &str) -> Result<(), String> {
    if id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err("withdrawal request id must be 32 lowercase hexadecimal characters".into())
    }
}

fn root_exists() -> Result<bool, String> {
    match std::fs::symlink_metadata(STATE_ROOT) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("Root withdrawal state is unavailable: {error}")),
        Ok(_) => {
            let metadata =
                crate::provenance::fsec::require_secure_location(Path::new(STATE_ROOT), &[0])
                    .map_err(|error| error.to_string())?;
            if !metadata.is_dir || metadata.mode & 0o077 != 0 {
                return Err("Root withdrawal state is not a protected private directory".into());
            }
            Ok(true)
        }
    }
}

fn path(id: &str) -> Result<PathBuf, String> {
    valid_id(id)?;
    Ok(Path::new(STATE_ROOT).join(format!("{id}.json")))
}

fn records() -> Result<Vec<Pending>, String> {
    if !root_exists()? {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for entry in std::fs::read_dir(STATE_ROOT).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or("Root withdrawal record name is not UTF-8")?;
        if name.starts_with('.') {
            continue;
        }
        let id = name
            .strip_suffix(".json")
            .ok_or("unexpected Root withdrawal state entry")?;
        records.push(read(id)?);
        if records.len() > MAX_RECORDS {
            return Err("Root withdrawal state exceeds its bounded inventory".into());
        }
    }
    Ok(records)
}

fn read(id: &str) -> Result<Pending, String> {
    root_exists()?
        .then_some(())
        .ok_or("no protected withdrawal outcome exists")?;
    let path = path(id)?;
    let metadata = crate::provenance::fsec::require_secure_location(&path, &[0])
        .map_err(|error| error.to_string())?;
    if !metadata.is_file || metadata.nlink != 1 || metadata.size > MAX_RECORD_BYTES {
        return Err("Root withdrawal outcome is not a bounded single-link regular file".into());
    }
    #[cfg(unix)]
    let bytes = crate::provenance::fsec::DirHandle::open(Path::new(STATE_ROOT))
        .and_then(|directory| directory.open_file(&format!("{id}.json")))
        .and_then(|file| file.read_bounded(MAX_RECORD_BYTES))
        .map_err(|error| error.to_string())?;
    #[cfg(not(unix))]
    let bytes: Vec<u8> = return Err("Root withdrawal requires Linux".into());
    let pending: Pending = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if pending.schema_version != 1 || pending.request_id != id || pending.apps.len() > MAX_TARGETS {
        return Err("Root withdrawal record has an invalid binding".into());
    }
    pending.target.validate()?;
    valid_id(&pending.root_epoch)?;
    if pending.apps.iter().any(|app| {
        app.app.is_empty()
            || app.app.len() > 128
            || pending.requester != 0 && app.owner != pending.requester
            || app.packages.len() > MAX_TARGETS
    }) {
        return Err("Root withdrawal target exceeds its App bound".into());
    }
    Ok(pending)
}

fn save(pending: &Pending) -> Result<(), String> {
    let bytes = serde_json::to_vec(pending).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_RECORD_BYTES || pending.apps.len() > MAX_TARGETS {
        return Err("Root withdrawal outcome exceeds its persistence bound".into());
    }
    super::app_store::write_json(&path(&pending.request_id)?, pending)
}

fn visible(pending: &Pending) -> Value {
    json!({
        "request_id": pending.request_id,
        "scope": "apps_only",
        "target": pending.target,
        "root_retirement_confirmed": pending.phase == Phase::Retired,
        "no_affected_root_bindings": pending.phase == Phase::Retired && pending.apps.is_empty(),
        "state": pending.phase,
        "apps": pending.apps.iter().map(|app| json!({
            "owner_uid": app.owner, "app_id": app.app, "retired": app.retired,
            "supporting_evidence_missing": app.supporting_evidence_missing,
            "context_generation_closure": app.context_generation_closure,
        })).collect::<Vec<_>>(),
        "source_generation": pending.source_generation,
        "source_fingerprint": pending.source_fingerprint,
        "non_app_retirement": { "confirmed": false, "state": "unknown" },
        "error": pending.error,
    })
}

pub(crate) fn pending(owner: u32, app: &str) -> Result<bool, String> {
    Ok(records()?.iter().any(|record| {
        record.phase != Phase::Retired
            && record
                .apps
                .iter()
                .any(|target| target.owner == owner && target.app == app && !target.retired)
    }))
}

fn selector_applies(record: &Pending, owner: u32, package: &PackageRef) -> bool {
    (record.requester == 0 || record.requester == owner)
        && (!matches!(record.target, Target::Developer { .. }) || record.requester == 0)
        && record.target.matches(package)
}

pub(crate) fn check_package(owner: u32, package: &VerifiedPackage) -> Result<(), String> {
    package
        .assert_current(&crate::provenance::trust_store())
        .map_err(|error| error.to_string())?;
    check_reference(owner, &PackageRef::of(package))
}

fn check_reference(owner: u32, package: &PackageRef) -> Result<(), String> {
    // Keep positive verification in the existing daemon-default context.
    // These private stores can only deny; absent consumer grants never grant
    // or withdraw a developer grant supported by a different context.
    OwnerPolicy::load(0)?.check_denials(package)?;
    if owner != 0 {
        OwnerPolicy::load(owner)?.check_denials(package)?;
    }
    if records()?.iter().any(|record| {
        record.phase != Phase::Retired
            && (selector_applies(record, owner, package)
                || record
                    .apps
                    .iter()
                    .any(|app| app.owner == owner && app.app == package.id && !app.retired))
    }) {
        return Err(
            "Root App retirement is pending or indeterminate for this owner/package".into(),
        );
    }
    package.is_live(&crate::provenance::trust_store())
}

pub(crate) fn check_session(owner: u32, session: &str) -> Result<(), String> {
    let instance = crate::provenance::runtime::instance_for(owner, session)?
        .ok_or("Root App supporting instance evidence is missing")?;
    let package = instance
        .package
        .ok_or("Root App supporting package evidence is missing")?;
    if package.kind != PackageKind::App || instance.trust_context_uid != Some(0) {
        return Err(
            "Root App supporting trust context is missing or changed; retirement is indeterminate"
                .into(),
        );
    }
    check_reference(owner, &package)?;
    crate::provenance::runtime::assert_live_instance_now(owner, session)
}

pub(crate) async fn handle(
    state: &DaemonState,
    params: Value,
    client: &ClientIdentity,
) -> Result<Value, BrokerError> {
    let owner = client.require_uid().map_err(BrokerError::authorization)?;
    if client.extension_host.is_some() || client.process_uid() != Some(owner) {
        return Err(BrokerError::authorization(
            "withdrawal reconciliation requires a direct non-Host peer",
        ));
    }
    super::app_sessions::require_non_app_caller(client)
        .await
        .map_err(BrokerError::authorization)?;
    let pid = client
        .pid
        .ok_or_else(|| BrokerError::authorization("withdrawal peer PID is unavailable"))?;
    if client.start_time_ticks.is_none()
        || crate::proc::read_start_time_ticks_pub(pid) != client.start_time_ticks
        || crate::provenance::fsec::effective_uid() != 0
    {
        return Err(BrokerError::authorization(
            "withdrawal peer or Root broker identity changed",
        ));
    }
    let request: Request = serde_json::from_value(params)
        .map_err(|_| BrokerError::authorization("invalid closed withdrawal request"))?;
    request.validate().map_err(BrokerError::authorization)?;
    let recover = matches!(&request, Request::Recover { .. });
    match request {
        Request::Reconcile { target } => {
            let id = prepare(owner, target)?;
            run(state.clone(), id).await
        }
        Request::Status { request_id } | Request::Recover { request_id } => {
            let record = read(&request_id).map_err(BrokerError::unavailable)?;
            if record.requester != owner {
                return Err(BrokerError::authorization(
                    "withdrawal outcome belongs to another peer",
                ));
            }
            if recover {
                run(state.clone(), request_id).await
            } else {
                Ok(visible(&record))
            }
        }
    }
}

fn prepare(owner: u32, target: Target) -> Result<String, BrokerError> {
    static REQUESTS: Mutex<()> = Mutex::new(());
    let _requests = REQUESTS
        .lock()
        .map_err(|_| BrokerError::unavailable("Root withdrawal request lock failed"))?;
    if !root_exists().map_err(BrokerError::unavailable)? {
        let directory = Path::new(STATE_ROOT);
        crate::provenance::fsec::require_secure_location(
            directory.parent().ok_or_else(|| {
                BrokerError::unavailable("withdrawal state has no protected parent")
            })?,
            &[0],
        )
        .map_err(|error| BrokerError::unavailable(error.to_string()))?;
        crate::agent::util::ensure_durable_private_dir(directory)
            .map_err(|error| BrokerError::unavailable(error.to_string()))?;
        super::app_store::secure_directory(directory, 0o700).map_err(BrokerError::unavailable)?;
    }
    let existing = records().map_err(BrokerError::unavailable)?;
    if let Some(record) = existing.iter().find(|record| {
        record.requester == owner && record.target == target && record.phase != Phase::Retired
    }) {
        return Ok(record.request_id.clone());
    }
    if existing.len() >= MAX_RECORDS {
        return Err(BrokerError::unavailable(
            "Root withdrawal records are at capacity; own policy remains withdrawn",
        ));
    }
    let record = Pending {
        schema_version: 1,
        request_id: uuid::Uuid::new_v4().simple().to_string(),
        requester: owner,
        root_epoch: epoch().to_string(),
        target,
        source_generation: None,
        source_fingerprint: None,
        phase: Phase::Selecting,
        apps: Vec::new(),
        error: None,
    };
    save(&record).map_err(BrokerError::indeterminate)?;
    Ok(record.request_id)
}

struct Controller(String);
impl Drop for Controller {
    fn drop(&mut self) {
        if let Ok(mut active) = active().lock() {
            active.remove(&self.0);
        }
    }
}

async fn run(state: DaemonState, id: String) -> Result<Value, BrokerError> {
    {
        let mut active = active()
            .lock()
            .map_err(|_| BrokerError::unavailable("Root withdrawal controller lock failed"))?;
        if !active.insert(id.clone()) {
            return read(&id)
                .map(|record| visible(&record))
                .map_err(BrokerError::indeterminate);
        }
    }
    let controller = Controller(id.clone());
    let outcome_id = id.clone();
    let retained = tokio::spawn(async move {
        let _controller = controller;
        static CAPACITY: OnceLock<tokio::sync::Semaphore> = OnceLock::new();
        let _capacity = CAPACITY
            .get_or_init(|| tokio::sync::Semaphore::new(2))
            .acquire()
            .await
            .map_err(|_| "Root withdrawal capacity is unavailable")?;
        let mut record = read(&id)?;
        if record.phase == Phase::Retired {
            return Ok(visible(&record));
        }
        if record.root_epoch != epoch() {
            record.phase = Phase::Indeterminate;
            record.error = Some("Original Root custody belongs to an earlier daemon epoch; missing custody is not a checked retirement receipt".into());
            save(&record)?;
            return Ok(visible(&record));
        }
        let result = drive(&state, &mut record).await;
        if let Err(error) = result {
            record.phase = Phase::Indeterminate;
            record.error = Some(error);
            save(&record)?;
        }
        Ok::<_, String>(visible(&record))
    });
    retained
        .await
        .map_err(|error| {
            outcome_error(
                &outcome_id,
                &format!("Root reconciliation wait interrupted: {error}"),
            )
        })?
        .map_err(|error| outcome_error(&outcome_id, &error))
}

fn outcome_error(id: &str, message: &str) -> BrokerError {
    let mut error = BrokerError::indeterminate(message);
    error.data = Some(json!({ "request_id": id, "root_retirement_confirmed": false }));
    error
}

async fn drive(state: &DaemonState, record: &mut Pending) -> Result<(), String> {
    let source = OwnerPolicy::load(record.requester)?;
    if !record.target.observed(&source) {
        return Err("The current authenticated private policy does not prove every requested revocation; no selector is proof".into());
    }
    record.source_generation = source.generation;
    record.source_fingerprint = Some(source.fingerprint.clone());
    save(record)?;
    if record.apps.is_empty() {
        let bindings = tokio::time::timeout(
            Duration::from_secs(10),
            retained_bindings(state, (record.requester != 0).then_some(record.requester)),
        )
        .await
        .map_err(|_| "Root custody inventory is still draining")??;
        record.apps = select(record, &source, bindings)?;
        record.phase = Phase::Retiring;
        save(record)?;
    }
    for index in 0..record.apps.len() {
        if record.apps[index].retired {
            continue;
        }
        if record.apps[index].supporting_evidence_missing {
            return Err("A potentially affected App has no retained supporting context/package; Root recovery is required".into());
        }
        let app = record.apps[index].app.clone();
        let owner = record.apps[index].owner;
        let fence = std::sync::Arc::new(super::app_mutation::fence_owner(&app, owner).await?);
        // A shared package read lock serializes publication, while only this
        // owner's admission is closed. Retained cleanup never fences others.
        super::app_store::retire_for_owner(state, &app, owner, fence).await?;
        record.apps[index].retired = true;
        save(record)?;
    }
    let latest = OwnerPolicy::load(record.requester)?;
    if !record.target.observed(&latest) {
        return Err("Current private policy no longer proves the withdrawal; no pending custody was released".into());
    }
    record.source_generation = latest.generation;
    record.source_fingerprint = Some(latest.fingerprint.clone());
    let remaining = tokio::time::timeout(
        Duration::from_secs(10),
        retained_bindings(state, (record.requester != 0).then_some(record.requester)),
    )
    .await
    .map_err(|_| "Final Root custody inspection is still draining")??;
    let remaining = select(record, &latest, remaining)?;
    if !remaining.is_empty() {
        for app in remaining {
            if let Some(previous) = record
                .apps
                .iter_mut()
                .find(|previous| previous.owner == app.owner && previous.app == app.app)
            {
                *previous = app;
            } else {
                record.apps.push(app);
            }
        }
        save(record)?;
        return Err("Affected Root bindings remain or policy changed during retirement; checked reconciliation remains pending".into());
    }
    record.phase = Phase::Retired;
    record.error = None;
    save(record)
}

fn select(
    record: &Pending,
    source: &OwnerPolicy,
    bindings: Vec<Binding>,
) -> Result<Vec<PendingApp>, String> {
    let app_selector =
        !matches!(&record.target, Target::Developer { kind, .. } if *kind != PackageKind::App);
    let mut apps = BTreeMap::<(u32, String), (bool, Vec<PackageRef>, bool)>::new();
    for binding in bindings {
        if record.requester != 0 && record.requester != binding.owner {
            continue;
        }
        let context_closure = record.requester == 0
            && binding.trust_context_uid == Some(0)
            && binding.context_generation_invalidated;
        if !app_selector && !context_closure {
            continue;
        }
        let missing = binding.package.is_none()
            || binding.trust_context_uid.is_none()
            || binding.package.as_ref().is_some_and(|package| {
                package.kind != PackageKind::App
                    || package.id != binding.app
                    || package.tier == "developer" && package.developer_path.is_none()
            });
        if !missing {
            let package = binding
                .package
                .as_ref()
                .ok_or("retained package disappeared")?;
            if !record.target.matches(package) && !context_closure {
                continue;
            }
            if !context_closure {
                match &record.target {
                    Target::Revocation { .. } => {
                        if record.requester == 0 && binding.trust_context_uid != Some(0) {
                            continue;
                        }
                        if source.check_denials(package).is_ok() {
                            continue;
                        }
                    }
                    Target::Developer { .. } => {
                        if binding.trust_context_uid != Some(record.requester) {
                            continue;
                        }
                        if package.is_live(&source.store).is_ok() {
                            continue;
                        }
                    }
                }
            }
        } else if !context_closure
            && binding
                .package
                .as_ref()
                .is_some_and(|package| !record.target.matches(package))
        {
            continue;
        }
        let key = (binding.owner, binding.app);
        let entry = apps
            .entry(key)
            .or_insert_with(|| (false, Vec::new(), false));
        entry.0 |= missing;
        entry.2 |= context_closure;
        if let Some(package) = binding.package {
            if !entry.1.contains(&package) {
                entry.1.push(package);
            }
        }
        if apps.len() > MAX_TARGETS {
            return Err("affected Root App inventory exceeds its bound".into());
        }
    }
    Ok(apps
        .into_iter()
        .map(
            |((owner, app), (missing, packages, context_generation_closure))| PendingApp {
                owner,
                app,
                retired: false,
                supporting_evidence_missing: missing,
                packages,
                context_generation_closure,
            },
        )
        .collect())
}

pub(crate) async fn retained_bindings(
    state: &DaemonState,
    owner_filter: Option<u32>,
) -> Result<Vec<Binding>, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (state, owner_filter);
        Err("checked Root App reconciliation requires Linux".into())
    }
    #[cfg(target_os = "linux")]
    {
        let services = state
            .app_service_manager()
            .ok_or("Root App service custody is unavailable")?;
        let mut bindings = services.retained_trust_bindings(owner_filter).await?;
        bindings.extend(super::gui::retained_trust_bindings(owner_filter)?);
        bindings.extend(super::app_execution::retained_trust_bindings(owner_filter)?);
        bindings.extend(
            crate::extension_host::spawn::TaskAppData::retained_trust_bindings(owner_filter)?,
        );
        let root = Path::new("/run/cos/caps");
        match std::fs::symlink_metadata(root) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(bindings),
            Err(error) => return Err(error.to_string()),
            Ok(_) => {}
        }
        crate::provenance::fsec::require_secure_location(root, &[0])
            .map_err(|error| error.to_string())?;
        let mut owners = 0;
        for entry in std::fs::read_dir(root).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            if !entry
                .file_type()
                .map_err(|error| error.to_string())?
                .is_dir()
            {
                continue;
            }
            let owner = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
                .ok_or("unexpected owner in Root capability inventory")?;
            if owner_filter.is_some_and(|expected| expected != owner) {
                continue;
            }
            owners += 1;
            if owners > MAX_TARGETS {
                return Err("Root owner inventory exceeds its bound".into());
            }
            crate::provenance::fsec::require_secure_location(&entry.path(), &[0])
                .map_err(|error| error.to_string())?;
            let instances = crate::provenance::runtime::running_instances(owner)?;
            for (session, instance) in &instances {
                if instance.class != InstanceClass::App {
                    continue;
                }
                let package = instance.package.clone();
                let app = package
                    .as_ref()
                    .map(|package| package.id.clone())
                    .or_else(|| {
                        crate::proc::try_registry_sessions_for_owner(owner)
                            .ok()?
                            .into_iter()
                            .find(|row| row.session_id == *session)?
                            .app_id
                    })
                    .ok_or("Root App record has no recoverable installed identity")?;
                if !bindings.iter().any(|binding| {
                    binding.owner == owner
                        && binding.app == app
                        && binding.package == package
                        && binding.trust_context_uid == Some(0)
                }) {
                    bindings.push(Binding {
                        owner,
                        app,
                        package,
                        trust_context_uid: instance.trust_context_uid,
                        context_generation_invalidated: false,
                    });
                }
            }
            for row in crate::proc::try_registry_sessions_for_owner(owner)? {
                let Some(app) = row.app_id else {
                    continue;
                };
                if !bindings
                    .iter()
                    .any(|binding| binding.owner == owner && binding.app == app)
                {
                    bindings.push(Binding {
                        owner,
                        app,
                        package: None,
                        trust_context_uid: None,
                        context_generation_invalidated: false,
                    });
                }
            }
            if bindings.len() > MAX_TARGETS * 4 {
                return Err("Root binding inventory exceeds its bound".into());
            }
        }
        Ok(bindings)
    }
}

pub(crate) fn initialize(state: DaemonState) {
    if crate::provenance::fsec::effective_uid() != 0 {
        return;
    }
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        loop {
            tick.tick().await;
            let pending = match records() {
                Ok(records) => records,
                Err(error) => {
                    tracing::error!(%error, "Root withdrawal recovery state is unavailable");
                    continue;
                }
            };
            for record in pending
                .into_iter()
                .filter(|record| record.phase != Phase::Retired && record.root_epoch == epoch())
            {
                let _ = run(state.clone(), record.request_id).await;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package() -> PackageRef {
        PackageRef {
            kind: PackageKind::App,
            id: "withdrawal-fixture".into(),
            content_digest: format!("sha256:{}", "a".repeat(64)),
            publisher_key_id: None,
            tier: "developer".into(),
            developer_path: Some(PathBuf::from("/synthetic/retained/tree")),
        }
    }

    fn request(owner: u32) -> Pending {
        Pending {
            schema_version: 1,
            request_id: "a".repeat(32),
            requester: owner,
            root_epoch: "b".repeat(32),
            target: Target::Developer {
                kind: PackageKind::App,
                id: package().id,
            },
            source_generation: None,
            source_fingerprint: None,
            phase: Phase::Selecting,
            apps: Vec::new(),
            error: None,
        }
    }

    #[test]
    fn closed_requests_refuse_authority_selectors_and_malformed_references() {
        for value in [
            json!({"action":"reconcile","target":{"type":"developer","kind":"app","id":"fixture"},"owner":0}),
            json!({"action":"reconcile","target":{"type":"developer","kind":"app","id":"fixture","path":"/elsewhere"}}),
            json!({"action":"reconcile","target":{"type":"revocation","digest":"not-a-digest"}}),
            json!({"action":"status","request_id":"a".repeat(33)}),
            json!({"action":"recover","request_id":"a".repeat(32),"pid":1}),
        ] {
            assert!(serde_json::from_value::<Request>(value).is_err());
        }
    }

    #[test]
    fn developer_absence_only_affects_the_retained_supporting_context() {
        let policy = OwnerPolicy {
            owner: 0,
            generation: None,
            fingerprint: String::new(),
            store: std::sync::Arc::new(crate::provenance::TrustStore::default()),
        };
        let binding = Binding {
            owner: 101,
            app: package().id.clone(),
            package: Some(package()),
            trust_context_uid: Some(0),
            context_generation_invalidated: false,
        };
        assert!(select(&request(101), &policy, vec![binding.clone()])
            .unwrap()
            .is_empty());
        let affected = select(
            &request(0),
            &policy,
            vec![
                binding.clone(),
                Binding {
                    owner: 202,
                    ..binding.clone()
                },
            ],
        )
        .unwrap();
        assert_eq!(affected.len(), 2);
        assert!(select(
            &request(101),
            &policy,
            vec![Binding {
                owner: 202,
                trust_context_uid: Some(101),
                ..binding
            }]
        )
        .unwrap()
        .is_empty());
        let mut other = package();
        other.id = "unrelated-key".into();
        other.tier = "vendor".into();
        other.developer_path = None;
        let invalidated = Binding {
            owner: 101,
            app: other.id.clone(),
            package: Some(other),
            trust_context_uid: Some(0),
            context_generation_invalidated: true,
        };
        assert!(select(&request(101), &policy, vec![invalidated.clone()])
            .unwrap()
            .is_empty());
        let closure = select(&request(0), &policy, vec![invalidated]).unwrap();
        assert_eq!(closure.len(), 1);
        assert!(closure[0].context_generation_closure);
    }
}
